#!/usr/bin/env bash
set -euo pipefail

# Real PTY smoke for cancellation while shell-intercept waits for approval.
# A local protocol-compatible daemon removes model/provider dependencies while
# keeping the client, /dev/tty and signal path real.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

python3 - "$BINARY" "$TMP_ROOT" <<'PY'
import json
import os
import pty
import select
import socket
import struct
import sys
import threading
import time

binary = sys.argv[1]
tmp_root = sys.argv[2]
runtime = os.path.join(tmp_root, "runtime")
os.makedirs(os.path.join(runtime, "yunxi"), mode=0o700)
socket_path = os.path.join(runtime, "yunxi", "yunxi.sock")
os.environ["XDG_RUNTIME_DIR"] = runtime
os.environ["TERM"] = "dumb"
errors = []
cancel_received = threading.Event()
trace = []

def recv_exact(conn, size):
    data = bytearray()
    while len(data) < size:
        chunk = conn.recv(size - len(data))
        if not chunk:
            raise RuntimeError("daemon client disconnected")
        data.extend(chunk)
    return bytes(data)

def recv_frame(conn):
    length = struct.unpack(">I", recv_exact(conn, 4))[0]
    return json.loads(recv_exact(conn, length))

def send_frame(conn, frame):
    payload = json.dumps(frame, ensure_ascii=False, separators=(",", ":")).encode()
    conn.sendall(struct.pack(">I", len(payload)) + payload)

def serve():
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(socket_path)
            os.chmod(socket_path, 0o600)
            server.listen(8)
            # ensure_daemon() may probe more than once while racing a daemon
            # startup. Keep answering probes until the real Turn arrives.
            while not cancel_received.is_set():
                conn, _ = server.accept()
                with conn:
                    hello = recv_frame(conn)
                    trace.append(f"hello:{hello.get('kind')}")
                    assert hello["kind"] == "hello", hello
                    send_frame(conn, {"kind": "hello_ack", "protocol_version": 2,
                                      "max_frame_bytes": 24 * 1024 * 1024,
                                      "capabilities": ["turn", "cancel"]})
                    frame = recv_frame(conn)
                    trace.append(f"request:{frame.get('kind')}")
                    if frame["kind"] == "ping":
                        send_frame(conn, {"kind": "pong", "request_id": frame.get("request_id")})
                        continue
                    assert frame["kind"] == "turn", frame
                    request_id = frame["request_id"]
                    send_frame(conn, {"kind": "run_accepted", "run_id": "pty-cancel-run"})
                    send_frame(conn, {"kind": "approval", "id": "approval-1",
                                      "tool_name": "linux_readonly",
                                      "reason": "PTY cancellation smoke",
                                      "command": "printf safe", "cwd": frame["cwd"]})
                    conn.settimeout(4)
                    cancel = recv_frame(conn)
                    assert cancel == {"kind": "cancel", "request_id": request_id}, cancel
                    cancel_received.set()
                    send_frame(conn, {"kind": "done", "status": "cancelled"})
    except Exception as error:
        errors.append(error)

thread = threading.Thread(target=serve, daemon=True)
thread.start()
pid, fd = pty.fork()
if pid == 0:
    os.execl(binary, binary, "shell-intercept", "--shell", "fish", "--stdin", "--cwd", tmp_root)

def read_until(needle, timeout=5):
    data = bytearray()
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        data.extend(chunk)
        if needle in data:
            return bytes(data)
    raise AssertionError(
        f"timed out waiting for {needle!r}; got {bytes(data)!r}; daemon_trace={trace!r}; daemon_errors={errors!r}"
    )

# --stdin reads until EOF. Ctrl+D completes the prose while keeping the child
# attached to the PTY for the subsequent approval prompt.
os.write(fd, "请执行一个安全查询".encode())
os.write(fd, b"\x04")
time.sleep(0.1)
os.write(fd, b"\x04")
read_until("[YunXi 请求审批]".encode())
# The child is waiting in /dev/tty polling. Ctrl+C must cancel the turn.
os.write(fd, b"\x03")
deadline = time.time() + 5
status = None
while time.time() < deadline:
    waited, candidate = os.waitpid(pid, os.WNOHANG)
    if waited == pid:
        status = candidate
        break
    time.sleep(0.05)
if status is None:
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    raise AssertionError("shell-intercept did not exit after approval Ctrl+C")
assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, status
thread.join(timeout=2)
assert not thread.is_alive(), "fake daemon did not finish"
assert cancel_received.is_set(), "client did not send matching Cancel"
assert not errors, errors
print("shell-prompt-cancel-smoke=ok")
PY
