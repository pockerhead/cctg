"""QA fake hub. mode=capture: answer 204 and append each request body (JSON,
no headers) to OUT. mode=blackhole: accept and never answer. Writes the bound
port to PORTFILE. Stops after LIFETIME seconds."""
import socket, sys, threading, time

mode, portfile, out, lifetime = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
srv = socket.socket()
srv.bind(("127.0.0.1", 0))
srv.listen(64)
open(portfile, "w").write(str(srv.getsockname()[1]))
held = []


def handle(conn):
    if mode == "blackhole":
        held.append(conn)
        return
    data = b""
    conn.settimeout(3)
    try:
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(65536)
            if not chunk:
                break
            data += chunk
        head, _, body = data.partition(b"\r\n\r\n")
        length = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":", 1)[1])
        auth_ok = b"authorization: bearer " in head.lower()
        while len(body) < length:
            body += conn.recv(65536)
        with open(out, "ab") as f:
            f.write(body + (b"" if auth_ok else b" NOAUTH") + b"\n")
        conn.sendall(b"HTTP/1.1 204 No Content\r\n\r\n")
    finally:
        conn.close()


srv.settimeout(0.2)
end = time.time() + lifetime
while time.time() < end:
    try:
        c, _ = srv.accept()
    except socket.timeout:
        continue
    threading.Thread(target=handle, args=(c,), daemon=True).start()
