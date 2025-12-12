mod chip8;

use chip8::{Chip8, OpcodeDispatch, TOKEN_LEN};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    signal,
    sync::Mutex,
};

const DEFAULT_PORT: u16 = 5000;

#[derive(Clone)]
struct AppState {
    blob_dir: PathBuf,
    stdout_lock: Arc<Mutex<()>>,
    last_blob: Arc<Mutex<Option<[u8; TOKEN_LEN]>>>,
}

impl AppState {
    async fn ensure_blob_dir(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.blob_dir).await
    }

    fn sanitize_id(id: &str) -> Option<String> {
        if id.is_empty() || id.len() > 64 {
            return None;
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        Some(id.to_string())
    }

    fn blob_path(&self, id: &str) -> PathBuf {
        self.blob_dir.join(format!("{id}.blob"))
    }

    async fn put_blob(&self, id: &str, blob: &[u8; TOKEN_LEN]) -> std::io::Result<()> {
        self.ensure_blob_dir().await?;
        tokio::fs::write(self.blob_path(id), blob).await
    }

    async fn get_blob(&self, id: &str) -> std::io::Result<Option<[u8; TOKEN_LEN]>> {
        let p = self.blob_path(id);
        match tokio::fs::read(p).await {
            Ok(data) => {
                if data.len() != TOKEN_LEN {
                    return Ok(None);
                }
                let mut out = [0u8; TOKEN_LEN];
                out.copy_from_slice(&data);
                Ok(Some(out))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn update_last_blob(&self, blob: [u8; TOKEN_LEN]) {
        let mut g = self.last_blob.lock().await;
        *g = Some(blob);
    }

    async fn mirror_to_stdout(&self, peer: SocketAddr, frame: &str) {
        let _g = self.stdout_lock.lock().await;
        // Note: multiple clients share stdout; we prepend peer.
        print!("\x1b[2J\x1b[H[peer {peer}] \n{frame}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let blob_dir = std::env::var("STORE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/chip8_store"));

    let state = AppState {
        blob_dir,
        stdout_lock: Arc::new(Mutex::new(())),
        last_blob: Arc::new(Mutex::new(None)),
    };

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("CHIP-8 TCP service listening on 0.0.0.0:{port}");

    let state = Arc::new(state);

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((sock, peer)) => {
                        let st = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(sock, peer, st).await {
                                eprintln!("[{peer}] connection ended: {e}");
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("accept failed: {err}");
                    }
                }
            }
            _ = signal::ctrl_c() => {
                eprintln!("received shutdown signal, exiting");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_client(
    mut sock: TcpStream,
    peer: SocketAddr,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let mut vm = Chip8::new();

    if let Some(blob) = state.last_blob.lock().await.clone() {
        vm.set_token_bytes(&blob);
    }

    // Greet with initial frame
    send_ok(&mut sock, "HELLO", Some(&vm.render_ansi())).await?;

    loop {
        let mut opbuf = [0u8; 2];
        if sock.read_exact(&mut opbuf).await.is_err() {
            break;
        }
        let opcode = u16::from_be_bytes(opbuf);

        match vm.dispatch_opcode(opcode) {
            OpcodeDispatch::TokenStore => {
                let id = read_len_prefixed(&mut sock, 64).await?;
                let id = match AppState::sanitize_id(&id) {
                    Some(v) => v,
                    None => {
                        send_err(&mut sock, "BAD_ID").await?;
                        continue;
                    }
                };

                let mut blob = [0u8; TOKEN_LEN];
                sock.read_exact(&mut blob).await?;
                if !looks_like_blob(&blob) {
                    send_err(&mut sock, "BAD_TOKEN_FORMAT").await?;
                    continue;
                }

                vm.set_token_bytes(&blob);
                state.put_blob(&id, &blob).await?;
                state.update_last_blob(blob).await;
                send_ok(&mut sock, &format!("PAYLOAD_STORED id={id}"), None).await?;
            }
            OpcodeDispatch::LeakNote { byte } => {
                sock.write_all(&[byte]).await?;
                continue;
            }
            OpcodeDispatch::PutToken => {
                let id = read_len_prefixed(&mut sock, 64).await?;
                let id = match AppState::sanitize_id(&id) {
                    Some(v) => v,
                    None => {
                        send_err(&mut sock, "BAD_ID").await?;
                        continue;
                    }
                };

                let mut blob = [0u8; TOKEN_LEN];
                sock.read_exact(&mut blob).await?;

                if !looks_like_blob(&blob) {
                    send_err(&mut sock, "BAD_TOKEN_FORMAT").await?;
                    continue;
                }

                state.put_blob(&id, &blob).await?;
                send_ok(&mut sock, &format!("STORED id={id}"), None).await?;
            }
            OpcodeDispatch::GetToken => {
                let id = read_len_prefixed(&mut sock, 64).await?;
                let id = match AppState::sanitize_id(&id) {
                    Some(v) => v,
                    None => {
                        send_err(&mut sock, "BAD_ID").await?;
                        continue;
                    }
                };

                match state.get_blob(&id).await? {
                    Some(blob) => {
                        let blob_str = String::from_utf8_lossy(&blob);
                        send_ok(&mut sock, &format!("BLOB id={id} value={blob_str}"), None).await?;
                    }
                    None => {
                        send_err(&mut sock, "NOT_FOUND").await?;
                    }
                }
            }
            OpcodeDispatch::SetKeyPayload => {
                let mut b = [0u8; 2];
                sock.read_exact(&mut b).await?;
                vm.apply_set_key_payload(b);
                send_ok(&mut sock, "KEY_SET", None).await?;
            }
            OpcodeDispatch::LoadRomPayload => {
                let mut lenb = [0u8; 2];
                sock.read_exact(&mut lenb).await?;
                let len = u16::from_be_bytes(lenb) as usize;
                if len > 3584 {
                    send_err(&mut sock, "ROM_TOO_LARGE").await?;
                    continue;
                }
                let mut rom = vec![0u8; len];
                sock.read_exact(&mut rom).await?;
                match vm.load_rom(&rom) {
                    Ok(()) => send_ok(&mut sock, "ROM_LOADED", Some(&vm.render_ansi())).await?,
                    Err(e) => send_err(&mut sock, e).await?,
                }
            }
            OpcodeDispatch::ResetVm => {
                send_ok(&mut sock, "RESET", Some(&vm.render_ansi())).await?;
            }
            OpcodeDispatch::Frame { frame } => {
                send_ok(&mut sock, "FRAME", Some(&frame)).await?;
            }
            OpcodeDispatch::Standard(r) => {
                if r.draw {
                    let frame = vm.render_ansi();
                    state.mirror_to_stdout(peer, &frame).await;
                    send_ok(&mut sock, "DRAW", Some(&frame)).await?;
                } else if r.waiting_for_key {
                    send_ok(&mut sock, "WAIT_KEY", None).await?;
                } else {
                    send_ok(&mut sock, "OK", None).await?;
                }
            }
        }
    }

    Ok(())
}

async fn read_len_prefixed(sock: &mut TcpStream, max_len: usize) -> std::io::Result<String> {
    let mut lb = [0u8; 1];
    sock.read_exact(&mut lb).await?;
    let len = lb[0] as usize;
    if len == 0 || len > max_len {
        return Ok(String::new());
    }
    let mut buf = vec![0u8; len];
    sock.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn looks_like_blob(blob: &[u8; TOKEN_LEN]) -> bool {
    // Enforce `[A-Z0-9]{31}=`
    if blob[31] != b'=' {
        return false;
    }
    for &c in &blob[..31] {
        if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

async fn send_ok(sock: &mut TcpStream, msg: &str, frame: Option<&str>) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str("OK ");
    out.push_str(msg);
    out.push('\n');
    if let Some(f) = frame {
        out.push_str(f);
    }
    out.push_str("--END--\n");
    sock.write_all(out.as_bytes()).await
}

async fn send_err(sock: &mut TcpStream, msg: &str) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str("ERR ");
    out.push_str(msg);
    out.push('\n');
    out.push_str("--END--\n");
    sock.write_all(out.as_bytes()).await
}
