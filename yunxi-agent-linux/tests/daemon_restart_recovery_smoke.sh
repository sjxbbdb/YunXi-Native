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
export YUNXI_TEST_DAEMON_EVENT_DELAY_MS=500
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

"$BINARY" daemon >"$TMP_ROOT/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || { cat "$TMP_ROOT/daemon.log" >&2; exit 1; }

RECOVERED_RUN_ID="$(python3 - "$SOCKET" "$STATE_HOME/yunxi/runs" "$DAEMON_PID" <<'PY'
import json
import os
import pathlib
import signal
import socket
import struct
import sys
import time

path = sys.argv[1]
state_directory = pathlib.Path(sys.argv[2])
daemon_pid = int(sys.argv[3])
run_ids = []

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

for index in range(16):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(5)
        sock.connect(path)
        hello(sock)
        send(sock, {
            "kind": "turn",
            "request_id": f"restart-recovery-{index}",
            "cwd": "/tmp",
            "prompt": f"deterministic restart recovery fixture {index}",
            "session_id": None,
            "offline": True,
            "live": False,
            "provider": None,
            "model": None,
            "delivery": "detached_output_only",
        })
        accepted = recv(sock)
        assert accepted["kind"] == "run_accepted", accepted
        run_ids.append(accepted["run_id"])

# Wait for a real daemon-owned detached run to persist at least one event while
# still running, then freeze and kill that daemon. The smoke-only environment
# delay above yields after each event, so this polling observes a deterministic
# running+event window without changing normal production behavior.
caught = None
deadline = time.monotonic() + 10
while time.monotonic() < deadline and caught is None:
    for state_path in state_directory.glob("*.json"):
        try:
            record = json.loads(state_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if record.get("status") == "running" and len(record.get("events", [])) >= 1:
            os.kill(daemon_pid, signal.SIGSTOP)
            caught = record
            os.kill(daemon_pid, signal.SIGKILL)
            break
    if caught is None:
        time.sleep(0.0005)

if caught is None:
    raise AssertionError("detached fixture completed before an event could be persisted")
print(caught["run_id"])
PY
)"

set +e
wait "$DAEMON_PID" 2>/dev/null
CRASH_STATUS=$?
set -e
DAEMON_PID=""
[[ "$CRASH_STATUS" -ne 0 ]] || {
  echo "SIGKILL fixture unexpectedly exited cleanly" >&2
  exit 1
}

# Restart the real daemon against the same XDG state directory. Its startup
# recovery must consume the durable running manifest left by SIGKILL.
"$BINARY" daemon >"$TMP_ROOT/restarted-daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 40); do
  if [[ -S "$SOCKET" ]] && "$BINARY" run-status "$RECOVERED_RUN_ID" >"$TMP_ROOT/restarted-status.json" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
[[ -s "$TMP_ROOT/restarted-status.json" ]] || {
  cat "$TMP_ROOT/restarted-daemon.log" >&2
  exit 1
}

python3 - "$SOCKET" "$RECOVERED_RUN_ID" <<'PY'
import json
import socket
import struct
import sys

path, run_id = sys.argv[1:]

def send(sock, frame):
    payload = json.dumps(frame, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)

def recv(sock):
    header = b""
    while len(header) < 4:
        chunk = sock.recv(4 - len(header))
        if not chunk:
            raise AssertionError("daemon closed before frame header")
        header += chunk
    length = struct.unpack(">I", header)[0]
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise AssertionError("daemon closed before frame")
        data += chunk
    return json.loads(data)

def hello(sock):
    send(sock, {
        "kind": "hello",
        "protocol_version": 2,
        "client": "recovery-smoke",
        "capabilities": ["run_status", "follow"],
    })
    assert recv(sock)["kind"] == "hello_ack"

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    hello(sock)
    send(sock, {"kind": "status", "run_id": run_id})
    status = recv(sock)
    assert status["kind"] == "run_status", status
    assert status["status"] == "interrupted", status
    assert status["recoverable"] is True, status
    assert status["next_seq"] >= 2, status

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    hello(sock)
    send(sock, {"kind": "follow", "run_id": run_id, "after_seq": 0})
    events = []
    while True:
        frame = recv(sock)
        assert frame["kind"] == "event", frame
        events.append(frame)
        if frame["frame"]["kind"] == "done":
            break
    assert any(event["frame"]["kind"] in {"thread", "message"} for event in events), events
    done_frame = events[-1]["frame"]
    assert done_frame["kind"] == "done", events
    assert done_frame["status"] == "interrupted", events
    assert done_frame.get("error"), events
PY

echo "daemon-restart-recovery-smoke=ok"
