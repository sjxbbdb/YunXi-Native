#!/usr/bin/env bash
set -euo pipefail

# Explicit project directory import smoke.  The directory is supplied by the
# caller; no HOME/current-directory discovery is involved.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
OWNER="$($BINARY knowledge-principal | python3 -c 'import json,sys; print(json.load(sys.stdin)["principal"])')"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
WORKSPACE="$TMP_ROOT/workspace"
IMPORT_ROOT="$WORKSPACE/source"
mkdir -p "$IMPORT_ROOT/src" "$IMPORT_ROOT/.yunxi"
printf '%s\n' '# guide' 'systemctl restart requires approval.' >"$IMPORT_ROOT/README.md"
printf '%s\n' 'fn main() { println!("knowledge"); }' >"$IMPORT_ROOT/src/main.rs"
printf '%s\n' 'ignored state' >"$IMPORT_ROOT/.yunxi/ignored.md"
printf '\377\376\n' >"$IMPORT_ROOT/binary.txt"
ln -s README.md "$IMPORT_ROOT/symlink.md"

"$BINARY" knowledge-space-init \
  --space-id project-dir --kind project --visibility owner \
  --owner "$OWNER" --source project-dir --version v1 --cwd "$WORKSPACE" \
  >"$TMP_ROOT/init.json"
"$BINARY" knowledge-import-directory "$IMPORT_ROOT" \
  --space-id project-dir --source project-dir --version v1 \
  --owner "$OWNER" --visibility owner --cwd "$WORKSPACE" \
  >"$TMP_ROOT/import.json"

python3 - "$TMP_ROOT/import.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "imported", value
assert value["imported"] == 2, value
assert value["failed"] == 0, value
assert value["skipped"] >= 3, value
assert all("/tmp/" not in item.get("relative_path", "") for item in value["documents"]), value
PY

"$BINARY" knowledge-worker --max-jobs 10 --cwd "$WORKSPACE" >"$TMP_ROOT/worker.json"
"$BINARY" knowledge-search 'systemctl restart' \
  --space-id project-dir --owner "$OWNER" --visibility owner \
  --cwd "$WORKSPACE" >"$TMP_ROOT/search.json"
python3 - "$TMP_ROOT/search.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["space_id"] == "project-dir", value
assert any(item["document_id"].startswith("dir-") for item in value["results"]), value
PY

[[ ! -f "$WORKSPACE/.yunxi/long-term-vectors.sqlite3" ]] || {
  echo "directory import touched long-term memory storage" >&2
  exit 1
}
echo "knowledge-directory-import-smoke=ok"
