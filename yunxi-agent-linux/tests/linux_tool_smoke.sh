#!/usr/bin/env bash
set -euo pipefail

# Real Linux acceptance test for the read-only ToolSpec CLI probes.  The host
# may not have every optional utility installed, so `unavailable` is valid;
# malformed arguments and shell syntax must still be rejected before spawn.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"

command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

"$BINARY" linux-tool describe >"$TMP_ROOT/describe.json"
python3 - "$TMP_ROOT/describe.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    specs = json.load(handle)
assert {item["name"] for item in specs} == {
    "linux.systemd_status",
    "linux.man",
    "linux.processes",
    "linux.network",
}, specs
assert all(
    item["risk_class"] == "read_only"
    and item["requires_root"] is False
    and item["mutates_system"] is False
    for item in specs
), specs
PY

run_probe() {
  local output="$1"
  local expected_tool="$2"
  shift 2
  "$BINARY" linux-tool "$@" >"$output"
  python3 - "$output" "$expected_tool" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    result = json.load(handle)
assert result["tool"] == sys.argv[2], result
assert result["status"] in {"ok", "failed", "unavailable"}, result
assert result["risk_class"] == "read_only", result
assert result["requires_root"] is False, result
assert result["mutates_system"] is False, result
assert isinstance(result["available"], bool), result
assert isinstance(result["command"], list) and result["command"], result
assert isinstance(result["stdout"], str) and isinstance(result["stderr"], str), result
assert len(result["stdout"].encode()) <= 64 * 1024, result
assert len(result["stderr"].encode()) <= 64 * 1024, result
PY
}

run_probe "$TMP_ROOT/processes.json" linux.processes processes --limit 0
run_probe "$TMP_ROOT/network.json" linux.network network
run_probe "$TMP_ROOT/systemd.json" linux.systemd_status systemd-status --unit yunxi-linux.service
run_probe "$TMP_ROOT/man.json" linux.man man fish

if "$BINARY" linux-tool systemd-status --unit 'yunxi.service; touch /tmp/yunxi-smoke' >/dev/null 2>&1; then
  echo "systemd token injection unexpectedly accepted" >&2
  exit 1
fi
if "$BINARY" linux-tool man 'fish --pager' >/dev/null 2>&1; then
  echo "man token injection unexpectedly accepted" >&2
  exit 1
fi

echo "linux-tool-smoke=ok"
