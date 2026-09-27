#!/usr/bin/env bash
set -euo pipefail

# Real Unix-socket smoke for the Linux daemon.  It deliberately uses a
# deterministic provider-selection error instead of a live model, so the test
# validates the protocol and lifecycle without credentials or network access.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"

command -v python3 >/dev/null || {
  echo "python3 is required" >&2
  exit 77
}
test -x "$BINARY" || {
  echo "release binary not found: $BINARY" >&2
  exit 77
}

TMP_ROOT="$(mktemp -d)"
RUNTIME_DIR="$TMP_ROOT/runtime"
mkdir -p "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export HOME="$TMP_ROOT/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
SOCKET="$RUNTIME_DIR/yunxi/yunxi.sock"
LOCK="$RUNTIME_DIR/yunxi/yunxi.lock"
DAEMON_LOG="$TMP_ROOT/daemon.log"
DAEMON_PID=""

cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.05
    done
    if kill -0 "$DAEMON_PID" 2>/dev/null; then
      kill -KILL "$DAEMON_PID" 2>/dev/null || true
    fi
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

"$BINARY" daemon >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || {
  echo "daemon socket did not appear" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
}
[[ "$(stat -c '%a' "$(dirname "$SOCKET")")" == "700" ]] || {
  echo "daemon runtime directory is not mode 700" >&2
  exit 1
}
[[ "$(stat -c '%a' "$SOCKET")" == "600" ]] || {
  echo "daemon socket is not mode 600" >&2
  exit 1
}
[[ -f "$LOCK" ]] || {
  echo "daemon lock did not appear" >&2
  exit 1
}

python3 - "$SOCKET" <<'PY'
import json
import socket
import struct
import sys

socket_path = sys.argv[1]
protocol_version = 2


def send(sock, frame):
    payload = json.dumps(frame, ensure_ascii=False, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)


def recv_exact(sock, size):
    chunks = bytearray()
    while len(chunks) < size:
        chunk = sock.recv(size - len(chunks))
        if not chunk:
            raise AssertionError("daemon closed the socket before the frame completed")
        chunks.extend(chunk)
    return bytes(chunks)


def recv(sock):
    length = struct.unpack(">I", recv_exact(sock, 4))[0]
    assert 0 < length <= 24 * 1024 * 1024, length
    return json.loads(recv_exact(sock, length))


def hello(sock):
    send(
        sock,
        {
            "kind": "hello",
            "protocol_version": protocol_version,
            "client": "yunxi-daemon-smoke",
            "capabilities": ["ping", "turn", "follow"],
        },
    )
    frame = recv(sock)
    assert frame["kind"] == "hello_ack", frame
    assert frame["protocol_version"] == protocol_version, frame
    assert frame["max_frame_bytes"] == 24 * 1024 * 1024, frame
    assert {"ping", "turn", "cancel", "follow_resync", "follow_replay", "follow_active", "detached_output_only", "detached_cancel"}.issubset(
        frame["capabilities"]
    ), frame


with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(sock, {"kind": "ping", "request_id": "ping-1"})
    assert recv(sock) == {"kind": "pong", "request_id": "ping-1"}

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "follow",
            "run_id": "missing-run",
            "session_id": None,
            "after_seq": 0,
        },
    )
    frame = recv(sock)
    assert frame["kind"] == "resync_required", frame
    assert frame["run_id"] == "missing-run", frame

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "cancel",
            "request_id": "cancel-missing",
            "run_id": "missing-run",
        },
    )
    frame = recv(sock)
    assert frame["kind"] == "error", frame
    assert "未知或已结束" in frame["message"], frame

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "turn",
            "request_id": "failure-1",
            "cwd": "/tmp",
            "prompt": "deterministic smoke failure",
            "session_id": None,
            "offline": True,
            "live": True,
            "provider": None,
            "model": None,
        },
    )
    accepted = recv(sock)
    assert accepted["kind"] == "run_accepted", accepted
    error = recv(sock)
    assert error["kind"] == "error", error
    assert "--offline" in error["message"] and "--live" in error["message"], error

# Detached output-only runs do not depend on the originating socket. Close the
# client immediately after acceptance, then Follow from a fresh connection.
detached_run_id = None
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "turn",
            "request_id": "detached-1",
            "cwd": "/tmp",
            "prompt": "detached smoke",
            "session_id": None,
            "offline": True,
            "live": False,
            "provider": None,
            "model": None,
            "delivery": "detached_output_only",
        },
    )
    accepted = recv(sock)
    assert accepted["kind"] == "run_accepted", accepted
    detached_run_id = accepted["run_id"]

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    hello(sock)
    send(sock, {"kind": "follow", "run_id": detached_run_id, "after_seq": 0})
    detached_events = []
    while True:
        frame = recv(sock)
        if frame["kind"] == "resync_required":
            raise AssertionError(frame)
        assert frame["kind"] == "event", frame
        assert frame["run_id"] == detached_run_id, frame
        assert frame["frame"]["kind"] in {"thread", "message", "done"}, frame
        detached_events.append(frame)
        if frame["frame"]["kind"] == "done":
            break
    assert detached_events, detached_events

# The offline static runtime gives the replay ring a completed, credential-free
# run.  Reconnect by run_id and verify that the numbered event sequence is
# replayed from cursor zero, including the terminal Done event.
run_id = None
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "turn",
            "request_id": "replay-1",
            "cwd": "/tmp",
            "prompt": "replay smoke",
            "session_id": None,
            "offline": True,
            "live": False,
            "provider": None,
            "model": None,
        },
    )
    accepted = recv(sock)
    assert accepted["kind"] == "run_accepted", accepted
    run_id = accepted["run_id"]
    original_events = []
    while True:
        frame = recv(sock)
        if frame["kind"] != "event":
            raise AssertionError(frame)
        assert frame["run_id"] == run_id, frame
        original_events.append(frame)
        if frame["frame"]["kind"] == "done":
            break
assert original_events, original_events

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "follow",
            "run_id": run_id,
            "session_id": None,
            "after_seq": 0,
        },
    )
    replayed = []
    while len(replayed) < len(original_events):
        frame = recv(sock)
        assert frame["kind"] == "event", frame
        assert frame["run_id"] == run_id, frame
        replayed.append(frame)
        if frame["frame"]["kind"] == "done":
            break
    assert replayed == original_events, (original_events, replayed)


with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "turn",
            "request_id": "oversized-1",
            "cwd": "/tmp",
            "prompt": "x" * (64 * 1024 + 1),
            "session_id": None,
            "offline": True,
            "live": False,
            "provider": None,
            "model": None,
        },
    )
    error = recv(sock)
    assert error["kind"] == "error", error
    assert "prompt" in error["message"], error
    assert "65536" in error["message"], error

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(sock, {"kind": "ping", "request_id": "after-oversized-turn"})
    assert recv(sock) == {"kind": "pong", "request_id": "after-oversized-turn"}

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    try:
        sock.recv(1)
    except socket.timeout:
        raise AssertionError("daemon kept an idle pre-handshake connection open")

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(8)
    sock.connect(socket_path)
    hello(sock)
    try:
        sock.recv(1)
    except socket.timeout:
        raise AssertionError("daemon kept a post-handshake idle connection open")

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    sock.sendall(struct.pack(">I", 24 * 1024 * 1024 + 1))

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    payload = b"{"
    sock.sendall(struct.pack(">I", len(payload)) + payload)

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(socket_path)
    hello(sock)
    send(sock, {"kind": "ping", "request_id": "after-malformed-frames"})
    assert recv(sock) == {"kind": "pong", "request_id": "after-malformed-frames"}

print("daemon-ipc-smoke=ok")
PY

# Exercise the lock guard independently of the ready-socket fast path.  The
# first daemon remains alive, but its temporary socket is removed so a second
# process must consult the lock metadata instead of silently taking over.
rm -f "$SOCKET"
SECOND_LOG="$TMP_ROOT/second-daemon.log"
set +e
"$BINARY" daemon >"$SECOND_LOG" 2>&1 &
SECOND_PID=$!
for _ in $(seq 1 40); do
  kill -0 "$SECOND_PID" 2>/dev/null || break
  sleep 0.05
done
if kill -0 "$SECOND_PID" 2>/dev/null; then
  echo "second daemon did not exit within 2 seconds" >&2
  kill -KILL "$SECOND_PID" 2>/dev/null || true
  wait "$SECOND_PID" 2>/dev/null || true
  SECOND_STATUS=124
else
  wait "$SECOND_PID"
  SECOND_STATUS=$?
fi
set -e
[[ "$SECOND_STATUS" -ne 0 ]] || {
  echo "second daemon unexpectedly acquired the singleton lock" >&2
  cat "$SECOND_LOG" >&2
  exit 1
}
grep -Eq "单例锁|已在运行" "$SECOND_LOG" || {
  echo "second daemon failure did not identify the singleton guard" >&2
  cat "$SECOND_LOG" >&2
  exit 1
}
[[ -f "$LOCK" ]] || {
  echo "first daemon lock disappeared while it was still alive" >&2
  exit 1
}

kill -TERM "$DAEMON_PID"
for _ in $(seq 1 40); do
  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
if kill -0 "$DAEMON_PID" 2>/dev/null; then
  echo "daemon did not exit within 2 seconds after SIGTERM" >&2
  cat "$DAEMON_LOG" >&2
  kill -KILL "$DAEMON_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
  exit 1
fi
set +e
wait "$DAEMON_PID"
STATUS=$?
set -e
DAEMON_PID=""
if [[ "$STATUS" -ne 0 ]]; then
  echo "daemon did not exit cleanly after SIGTERM: status=$STATUS" >&2
  cat "$DAEMON_LOG" >&2
  exit "$STATUS"
fi
[[ ! -e "$SOCKET" ]] || {
  echo "daemon socket remained after SIGTERM" >&2
  exit 1
}
[[ ! -e "$LOCK" ]] || {
  echo "daemon lock remained after SIGTERM" >&2
  exit 1
}

# A hard crash cannot run Drop, so both the socket and lock are expected to be
# left behind.  The next start must prove that the production stale-owner
# path can recover them, and that a concurrent start race still yields one
# actual daemon owner.
CRASH_LOG="$TMP_ROOT/crash-daemon.log"
"$BINARY" daemon >"$CRASH_LOG" 2>&1 &
CRASH_PID=$!
for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" && -f "$LOCK" ]] || {
  echo "crash daemon did not establish socket/lock" >&2
  cat "$CRASH_LOG" >&2
  kill -KILL "$CRASH_PID" 2>/dev/null || true
  wait "$CRASH_PID" 2>/dev/null || true
  exit 1
}
kill -KILL "$CRASH_PID" 2>/dev/null || true
wait "$CRASH_PID" 2>/dev/null || true
[[ -e "$SOCKET" && -e "$LOCK" ]] || {
  echo "SIGKILL unexpectedly removed crash socket/lock; stale-owner path was not exercised" >&2
  exit 1
}

CONCURRENT_PIDS=()
for index in 1 2 3 4; do
  "$BINARY" daemon >"$TMP_ROOT/concurrent-$index.log" 2>&1 &
  CONCURRENT_PIDS+=("$!")
done

# Reap exited contenders before counting live processes; unreaped children
# otherwise remain zombies and make kill -0 look like a second daemon.
pid_is_running() {
  local state
  state="$(ps -o stat= -p "$1" 2>/dev/null | tr -d '[:space:]' || true)"
  [[ -n "$state" && "$state" != Z* ]]
}

OWNER_PID=""
for _ in $(seq 1 60); do
  running=()
  for pid in "${CONCURRENT_PIDS[@]}"; do
    if pid_is_running "$pid"; then
      running+=("$pid")
    else
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -S "$SOCKET" && "${#running[@]}" -eq 1 ]]; then
    OWNER_PID="${running[0]}"
    break
  fi
  sleep 0.05
done
[[ -n "$OWNER_PID" ]] || {
  echo "concurrent daemon start did not converge to exactly one owner" >&2
  for index in 1 2 3 4; do cat "$TMP_ROOT/concurrent-$index.log" >&2 || true; done
  for pid in "${CONCURRENT_PIDS[@]}"; do kill -KILL "$pid" 2>/dev/null || true; done
  exit 1
}

LOCK_OWNER_PID="$(python3 - "$LOCK" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    print(json.load(handle)["pid"])
PY
)"
[[ "$LOCK_OWNER_PID" == "$OWNER_PID" ]] || {
  echo "lock owner pid does not match the sole live daemon: lock=$LOCK_OWNER_PID live=$OWNER_PID" >&2
  exit 1
}

python3 - "$SOCKET" <<'PY'
import socket
import sys

path = sys.argv[1]
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    import json, struct
    def send(frame):
        data = json.dumps(frame, separators=(",", ":")).encode()
        sock.sendall(struct.pack(">I", len(data)) + data)
    def recv():
        size = struct.unpack(">I", sock.recv(4))[0]
        data = bytearray()
        while len(data) < size:
            data.extend(sock.recv(size - len(data)))
        return json.loads(data)
    send({"kind": "hello", "protocol_version": 2, "client": "yunxi-concurrent-smoke", "capabilities": ["ping"]})
    assert recv()["kind"] == "hello_ack"
    send({"kind": "ping", "request_id": "concurrent-owner"})
    assert recv() == {"kind": "pong", "request_id": "concurrent-owner"}
PY

kill -TERM "$OWNER_PID" 2>/dev/null || true
for _ in $(seq 1 40); do
  pid_is_running "$OWNER_PID" || break
  sleep 0.05
done
wait "$OWNER_PID" 2>/dev/null || true
for pid in "${CONCURRENT_PIDS[@]}"; do
  [[ "$pid" == "$OWNER_PID" ]] || wait "$pid" 2>/dev/null || true
done
[[ ! -e "$SOCKET" && ! -e "$LOCK" ]] || {
  echo "concurrent daemon owner did not clean up socket/lock" >&2
  exit 1
}

# A dead owner must not strand the next daemon.  This uses an impossible PID
# rather than touching any real process and verifies recovery through the same
# production lock path used after a crash.
printf '%s\n' '{"pid":4294967294,"start_time_ticks":null}' >"$LOCK"
STALE_LOG="$TMP_ROOT/stale-daemon.log"
"$BINARY" daemon >"$STALE_LOG" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || {
  echo "daemon did not recover from a stale lock" >&2
  cat "$STALE_LOG" >&2
  exit 1
}
kill -TERM "$DAEMON_PID"
for _ in $(seq 1 40); do
  kill -0 "$DAEMON_PID" 2>/dev/null || break
  sleep 0.05
done
if kill -0 "$DAEMON_PID" 2>/dev/null; then
  echo "recovered daemon did not stop within 2 seconds" >&2
  cat "$STALE_LOG" >&2
  kill -KILL "$DAEMON_PID" 2>/dev/null || true
fi
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""
[[ ! -e "$SOCKET" && ! -e "$LOCK" ]] || {
  echo "recovered daemon did not clean up socket/lock" >&2
  exit 1
}
trap - EXIT
rm -rf "$TMP_ROOT"
echo "daemon-lifecycle-smoke=ok"
