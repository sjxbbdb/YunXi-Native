#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }

TMP_ROOT="$(mktemp -d)"
RUNTIME_DIR="$TMP_ROOT/runtime"
STATE_HOME="$TMP_ROOT/state"
mkdir -p "$RUNTIME_DIR" "$STATE_HOME/yunxi/runs"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export XDG_STATE_HOME="$STATE_HOME"
export HOME="$TMP_ROOT/home"
mkdir -p "$HOME"
SOCKET="$RUNTIME_DIR/yunxi/yunxi.sock"
DAEMON_PID=""

cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.05
    done
    kill -KILL "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

python3 - "$STATE_HOME/yunxi/runs" <<'PY'
import json
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
run_id = "recovered-run"
h = 0xcbf29ce484222325
for byte in run_id.encode():
    h = ((h ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
path = directory / f"{h:016x}.json"
path.write_text(json.dumps({
    "version": 1,
    "run_id": run_id,
    "status": "running",
    "next_seq": 1,
    "active_follow_allowed": True,
    "request_id": "request-before-restart",
    "cwd": "/tmp/project",
    "session_id": None,
    "created_at_unix_secs": 1,
    "events": [{"seq": 1, "frame": {"kind": "message", "content": "before restart"}}],
}, ensure_ascii=False), encoding="utf-8")
PY

"$BINARY" daemon >"$TMP_ROOT/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || { cat "$TMP_ROOT/daemon.log" >&2; exit 1; }

python3 - "$SOCKET" <<'PY'
import json
import socket
import struct
import sys

path = sys.argv[1]

def send(sock, frame):
    payload = json.dumps(frame, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)

def recv(sock):
    length = struct.unpack(">I", sock.recv(4))[0]
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise AssertionError("daemon closed before frame")
        data += chunk
    return json.loads(data)

def hello(sock):
    send(sock, {"kind": "hello", "protocol_version": 2, "client": "recovery-smoke", "capabilities": ["run_status", "follow"]})
    assert recv(sock)["kind"] == "hello_ack"

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    hello(sock)
    send(sock, {"kind": "status", "run_id": "recovered-run"})
    status = recv(sock)
    assert status["kind"] == "run_status", status
    assert status["status"] == "interrupted", status
    assert status["recoverable"] is True, status
    assert status["next_seq"] == 2, status

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    hello(sock)
    send(sock, {"kind": "follow", "run_id": "recovered-run", "after_seq": 0})
    events = []
    while True:
        frame = recv(sock)
        assert frame["kind"] == "event", frame
        events.append(frame)
        if frame["frame"]["kind"] == "done":
            break
    assert events[0]["frame"] == {"kind": "message", "content": "before restart"}, events
    assert events[-1]["frame"] == {"kind": "done", "status": "interrupted"}, events
PY

echo "daemon-restart-recovery-smoke=ok"
