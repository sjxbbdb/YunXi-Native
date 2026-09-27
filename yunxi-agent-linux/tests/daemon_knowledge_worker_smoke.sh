#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
OWNER="$("$BINARY" knowledge-principal | python3 -c 'import json,sys; print(json.load(sys.stdin)["principal"])')"

TMP_ROOT="$(mktemp -d)"
RUNTIME_DIR="$TMP_ROOT/runtime"
WORKSPACE="$TMP_ROOT/workspace"
WORKSPACE_B="$TMP_ROOT/workspace-b"
mkdir -p "$RUNTIME_DIR" "$WORKSPACE" "$WORKSPACE_B"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export HOME="$TMP_ROOT/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
SOCKET="$RUNTIME_DIR/yunxi/yunxi.sock"
DAEMON_LOG="$TMP_ROOT/daemon.log"
MEMORY_FILE="$WORKSPACE/.yunxi/memory/fixture.jsonl"
MEMORY_FILE_B="$WORKSPACE_B/.yunxi/memory/fixture.jsonl"
DAEMON_PID=""

cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

mkdir -p "$(dirname "$MEMORY_FILE")"
printf '%s\n' '{"id":"fixture","content":"memory must not change"}' >"$MEMORY_FILE"
mkdir -p "$(dirname "$MEMORY_FILE_B")"
printf '%s\n' '{"id":"fixture-b","content":"memory b must not change"}' >"$MEMORY_FILE_B"
MEMORY_BEFORE="$(sha256sum "$MEMORY_FILE" | awk '{print $1}')"
MEMORY_BEFORE_B="$(sha256sum "$MEMORY_FILE_B" | awk '{print $1}')"

# The daemon validates the explicit fleet before binding its socket. Exercise
# the hard upper bound without creating a competing daemon instance.
TOO_MANY_ROOT="$TMP_ROOT/too-many"
mkdir -p "$TOO_MANY_ROOT"
TOO_MANY_ARGS=()
for index in $(seq 1 33); do
  workspace="$TOO_MANY_ROOT/workspace-$index"
  mkdir -p "$workspace"
  TOO_MANY_ARGS+=(--knowledge-workspace "$workspace")
done
set +e
"$BINARY" daemon "${TOO_MANY_ARGS[@]}" >"$TMP_ROOT/too-many.out" 2>&1
TOO_MANY_RC=$?
set -e
[[ "$TOO_MANY_RC" -ne 0 ]] || {
  echo "daemon accepted more than 32 knowledge workspaces" >&2
  exit 1
}
grep -Eq -- 'knowledge-workspace.*32' "$TMP_ROOT/too-many.out"

"$BINARY" knowledge-space-init --space-id daemon-smoke --kind private \
  --visibility private --owner "$OWNER" --source daemon-smoke \
  --version v1 --cwd "$WORKSPACE" >/dev/null
printf '%s\n' 'daemon-owned knowledge worker should embed this document' |
  "$BINARY" knowledge-import-stdin --space-id daemon-smoke \
  --document-id daemon-document --title daemon-document --source daemon-smoke \
  --version v1 --owner "$OWNER" --visibility private \
  --cwd "$WORKSPACE" >/dev/null
"$BINARY" knowledge-space-init --space-id daemon-smoke-b --kind private \
  --visibility private --owner "$OWNER" --source daemon-smoke \
  --version v1 --cwd "$WORKSPACE_B" >/dev/null
printf '%s\n' 'second explicit workspace must also be embedded' |
  "$BINARY" knowledge-import-stdin --space-id daemon-smoke-b \
  --document-id daemon-document-b --title daemon-document-b --source daemon-smoke \
  --version v1 --owner "$OWNER" --visibility private \
  --cwd "$WORKSPACE_B" >/dev/null

"$BINARY" daemon --knowledge-workspace "$WORKSPACE" \
  --knowledge-workspace "$WORKSPACE_B" \
  --knowledge-max-jobs 1 --knowledge-interval-secs 1 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 80); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || { echo "daemon socket did not appear" >&2; cat "$DAEMON_LOG" >&2; exit 1; }

python3 - "$SOCKET" <<'PY'
import json, socket, struct, sys
path = sys.argv[1]
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    def send(value):
        payload = json.dumps(value, separators=(",", ":")).encode()
        sock.sendall(struct.pack(">I", len(payload)) + payload)
    def recv():
        header = sock.recv(4)
        assert len(header) == 4, header
        size = struct.unpack(">I", header)[0]
        payload = bytearray()
        while len(payload) < size:
            chunk = sock.recv(size - len(payload))
            assert chunk, "daemon closed before response"
            payload.extend(chunk)
        return json.loads(payload)
    send({"kind":"hello","protocol_version":2,"client":"daemon-knowledge-smoke","capabilities":["ping"]})
    hello = recv()
    assert hello["kind"] == "hello_ack", hello
    send({"kind":"ping","request_id":"knowledge-ping"})
    assert recv() == {"kind":"pong","request_id":"knowledge-ping"}
PY

DB="$WORKSPACE/.yunxi/knowledge/knowledge.sqlite3"
DB_B="$WORKSPACE_B/.yunxi/knowledge/knowledge.sqlite3"
for _ in $(seq 1 80); do
  if python3 - "$DB" "$DB_B" 2>/dev/null <<'PY'
import sqlite3, sys
for path in sys.argv[1:]:
    with sqlite3.connect(path) as connection:
        statuses = [row[0] for row in connection.execute("SELECT status FROM knowledge_embedding_jobs ORDER BY job_id")]
    assert statuses == ["completed"], (path, statuses)
PY
  then
    break
  fi
  sleep 0.1
done
python3 - "$DB" "$DB_B" <<'PY'
import sqlite3, sys
for path in sys.argv[1:]:
    with sqlite3.connect(path) as connection:
        statuses = [row[0] for row in connection.execute("SELECT status FROM knowledge_embedding_jobs ORDER BY job_id")]
    assert statuses == ["completed"], (path, statuses)
PY

"$BINARY" knowledge-vector-search 'daemon-owned knowledge' --space-id daemon-smoke \
  --owner "$OWNER" --visibility private --cwd "$WORKSPACE" |
  python3 -c 'import json,sys; value=json.load(sys.stdin); assert value["results"], value'
"$BINARY" knowledge-vector-search 'second explicit workspace' --space-id daemon-smoke-b \
  --owner "$OWNER" --visibility private --cwd "$WORKSPACE_B" |
  python3 -c 'import json,sys; value=json.load(sys.stdin); assert value["results"], value'

set +e
"$BINARY" daemon --knowledge-workspace "$WORKSPACE" >"$TMP_ROOT/duplicate.out" 2>&1
DUPLICATE_RC=$?
set -e
[[ "$DUPLICATE_RC" -ne 0 ]] || { echo "duplicate worker daemon unexpectedly succeeded" >&2; exit 1; }
grep -q '不能向活动 daemon 附加 knowledge worker' "$TMP_ROOT/duplicate.out"

# A damaged knowledge database is isolated to the worker. The daemon must keep
# its Unix socket alive and record a redacted degraded state rather than exit.
printf '%s' 'not a sqlite database' >"$DB"
sleep 1
python3 - "$SOCKET" <<'PY'
import json, socket, struct, sys
path = sys.argv[1]
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.settimeout(5)
    sock.connect(path)
    def send(value):
        payload = json.dumps(value, separators=(",", ":")).encode()
        sock.sendall(struct.pack(">I", len(payload)) + payload)
    def recv():
        header = sock.recv(4)
        assert len(header) == 4, header
        size = struct.unpack(">I", header)[0]
        payload = bytearray()
        while len(payload) < size:
            chunk = sock.recv(size - len(payload))
            assert chunk, "daemon closed before response"
            payload.extend(chunk)
        return json.loads(payload)
    send({"kind":"hello","protocol_version":2,"client":"daemon-knowledge-corrupt-smoke","capabilities":["ping"]})
    assert recv()["kind"] == "hello_ack"
    send({"kind":"ping","request_id":"corrupt-ping"})
    assert recv() == {"kind":"pong","request_id":"corrupt-ping"}
PY
for _ in $(seq 1 40); do
  if grep -l '"last_status": "degraded"' "$XDG_STATE_HOME"/yunxi/knowledge-worker/*.json >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
grep -l '"last_status": "degraded"' "$XDG_STATE_HOME"/yunxi/knowledge-worker/*.json >/dev/null

MEMORY_AFTER="$(sha256sum "$MEMORY_FILE" | awk '{print $1}')"
[[ "$MEMORY_BEFORE" == "$MEMORY_AFTER" ]] || { echo "memory file changed" >&2; exit 1; }
MEMORY_AFTER_B="$(sha256sum "$MEMORY_FILE_B" | awk '{print $1}')"
[[ "$MEMORY_BEFORE_B" == "$MEMORY_AFTER_B" ]] || { echo "memory file b changed" >&2; exit 1; }
kill -TERM "$DAEMON_PID"
wait "$DAEMON_PID"
DAEMON_PID=""
[[ ! -e "$SOCKET" ]] || { echo "daemon socket was not cleaned up" >&2; exit 1; }

python3 - "$XDG_STATE_HOME/yunxi/knowledge-worker" <<'PY'
import json, pathlib, sys
files = list(pathlib.Path(sys.argv[1]).glob("*.json"))
assert files, files
values = [json.loads(path.read_text()) for path in files]
assert any(value.get("last_status") == "stopped" for value in values), values
PY
"$BINARY" knowledge-worker-health |
  python3 -c 'import json,sys; value=json.load(sys.stdin); assert value["status"] == "ok", value; assert value["last_status"] == "stopped", value; assert value["workspace_count"] == 2, value'
echo "daemon-knowledge-worker-smoke=ok"
