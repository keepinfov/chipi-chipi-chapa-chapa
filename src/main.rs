mod chip8;

use chip8::Chip8;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

const DEFAULT_PORT: u16 = 5000;
const FLAG_LEN: usize = 32;

#[derive(Clone)]
struct AppState {
    flag_dir: PathBuf,
    stdout_lock: Arc<Mutex<()>>,
}

impl AppState {
    async fn ensure_flag_dir(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.flag_dir).await
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

    fn flag_path(&self, id: &str) -> PathBuf {
        self.flag_dir.join(format!("{id}.flag"))
    }

    async fn put_flag(&self, id: &str, flag: &[u8; FLAG_LEN]) -> std::io::Result<()> {
        self.ensure_flag_dir().await?;
        tokio::fs::write(self.flag_path(id), flag).await
    }

    async fn get_flag(&self, id: &str) -> std::io::Result<Option<[u8; FLAG_LEN]>> {
        let p = self.flag_path(id);
        match tokio::fs::read(p).await {
            Ok(data) => {
                if data.len() != FLAG_LEN {
                    return Ok(None);
                }
                let mut out = [0u8; FLAG_LEN];
                out.copy_from_slice(&data);
                Ok(Some(out))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
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

    let flag_dir = std::env::var("FLAG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/chip8_flags"));

    let state = AppState {
        flag_dir,
        stdout_lock: Arc::new(Mutex::new(())),
    };

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("CHIP-8 TCP service listening on 0.0.0.0:{port}");

    let state = Arc::new(state);

    loop {
        let (sock, peer) = listener.accept().await?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(sock, peer, st).await {
                eprintln!("[{peer}] connection ended: {e}");
            }
        });
    }
}

async fn handle_client(
    mut sock: TcpStream,
    peer: SocketAddr,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let mut vm = Chip8::new();

    // Greet with initial frame
    send_ok(&mut sock, "HELLO", Some(&vm.render_ansi())).await?;

    loop {
        let mut opbuf = [0u8; 2];
        if sock.read_exact(&mut opbuf).await.is_err() {
            break;
        }
        let opcode = u16::from_be_bytes(opbuf);

        match opcode {
            // ---- A/D Custom opcodes (safe) ----

            // 0xF001: PUT_FLAG
            // payload: u8 id_len | id bytes | 32-byte flag
            0xF001 => {
                let id = read_len_prefixed(&mut sock, 64).await?;
                let id = match AppState::sanitize_id(&id) {
                    Some(v) => v,
                    None => {
                        send_err(&mut sock, "BAD_ID").await?;
                        continue;
                    }
                };

                let mut flag = [0u8; FLAG_LEN];
                sock.read_exact(&mut flag).await?;

                // Optional: enforce the regex-like constraint `[A-Z0-9]{31}=`
                if !looks_like_flag(&flag) {
                    send_err(&mut sock, "BAD_FLAG_FORMAT").await?;
                    continue;
                }

                state.put_flag(&id, &flag).await?;
                send_ok(&mut sock, &format!("STORED id={id}"), None).await?;
            }

            // 0xF002: GET_FLAG
            // payload: u8 id_len | id bytes
            0xF002 => {
                let id = read_len_prefixed(&mut sock, 64).await?;
                let id = match AppState::sanitize_id(&id) {
                    Some(v) => v,
                    None => {
                        send_err(&mut sock, "BAD_ID").await?;
                        continue;
                    }
                };

                match state.get_flag(&id).await? {
                    Some(flag) => {
                        let flag_str = String::from_utf8_lossy(&flag);
                        send_ok(&mut sock, &format!("FLAG id={id} value={flag_str}"), None).await?;
                    }
                    None => {
                        send_err(&mut sock, "NOT_FOUND").await?;
                    }
                }
            }

            // 0xF003: SET_KEY
            // payload: u8 key (0..15) | u8 down (0/1)
            0xF003 => {
                let mut b = [0u8; 2];
                sock.read_exact(&mut b).await?;
                let key = (b[0] & 0x0F) as usize;
                let down = b[1] != 0;
                vm.set_key(key, down);
                send_ok(&mut sock, "KEY_SET", None).await?;
            }

            // 0xF010: LOAD_ROM
            // payload: u16 len_be | len bytes
            0xF010 => {
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

            // 0xF011: RESET VM
            0xF011 => {
                vm.reset();
                send_ok(&mut sock, "RESET", Some(&vm.render_ansi())).await?;
            }

            // 0xF0FF: GET_FRAME
            0xF0FF => {
                send_ok(&mut sock, "FRAME", Some(&vm.render_ansi())).await?;
            }

            // ---- Standard CHIP-8 opcode execution ----
            _ => {
                let r = vm.step(opcode);

                // Always allow clients to see frames; only mirror to stdout when draw occurred.
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

fn looks_like_flag(flag: &[u8; FLAG_LEN]) -> bool {
    // Enforce `[A-Z0-9]{31}=`
    if flag[31] != b'=' {
        return false;
    }
    for &c in &flag[..31] {
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
