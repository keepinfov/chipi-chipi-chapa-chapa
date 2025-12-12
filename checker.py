#!/usr/bin/env python3
import socket
import sys
import os

PORT = int(os.environ.get("PORT", "5000"))

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

def must_ok(resp: bytes):
    if not resp.startswith(b"OK "):
        raise RuntimeError(resp.decode(errors="replace"))

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
    s.close()
    return True

def put(host, flag_id, flag):
    # flag_id: 1..64 chars [A-Za-z0-9_-]
    # flag: 32 bytes "[A-Z0-9]{31}="
    if not (len(flag) == 32 and flag[-1] == "="):
        raise RuntimeError("flag format wrong")
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello

    fid_b = flag_id.encode()
    if len(fid_b) == 0 or len(fid_b) > 64:
        raise RuntimeError("bad flag_id length")

    payload = bytes([len(fid_b)]) + fid_b + flag.encode()
    resp = send_opcode(s, 0xF001, payload=payload)
    must_ok(resp)
    s.close()
    return True

def get(host, flag_id, expected_flag):
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello

    fid_b = flag_id.encode()
    payload = bytes([len(fid_b)]) + fid_b
    resp = send_opcode(s, 0xF002, payload=payload)
    must_ok(resp)

    text = resp.decode(errors="replace")
    # line looks like: OK FLAG id=... value=XXXX...=
    if "value=" not in text:
        raise RuntimeError("no value in response")
    got = text.split("value=", 1)[1].splitlines()[0].strip()
    if got != expected_flag:
        raise RuntimeError(f"flag mismatch: got={got!r} expected={expected_flag!r}")

    s.close()
    return True

def main():
    # Simple A/D-style interface:
    #   checker.py check HOST
    #   checker.py put HOST FLAG_ID FLAG
    #   checker.py get HOST FLAG_ID FLAG
    if len(sys.argv) < 3:
        print("usage:\n  checker.py check HOST\n  checker.py put HOST FLAG_ID FLAG\n  checker.py get HOST FLAG_ID FLAG")
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
