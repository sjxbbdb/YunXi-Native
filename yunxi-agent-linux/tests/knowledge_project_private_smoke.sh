#!/usr/bin/env bash
set -euo pipefail

# Explicit project/private knowledge boundary smoke.  It only imports stdin,
# never scans a path, and uses the real release binary plus a temporary
# workspace so the test cannot touch the user's knowledge or memory stores.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
test -x "$BINARY" || {
  echo "release binary not found: $BINARY" >&2
  exit 77
}

TMP_ROOT="$(mktemp -d)"
WORKSPACE="$TMP_ROOT/workspace"
mkdir -p "$WORKSPACE"
trap 'rm -rf "$TMP_ROOT"' EXIT

run_json() {
  "$BINARY" "$@"
}

run_json knowledge-space-init \
  --space-id project-demo \
  --kind project \
  --visibility owner \
  --owner local-user \
  --source project-notes \
  --version v1 \
  --cwd "$WORKSPACE" >"$TMP_ROOT/project-init.json"
run_json knowledge-space-init \
  --space-id private-demo \
  --kind private \
  --visibility private \
  --owner local-user \
  --source private-notes \
  --version v1 \
  --cwd "$WORKSPACE" >"$TMP_ROOT/private-init.json"
run_json knowledge-space-init \
  --space-id project-demo \
  --kind project \
  --visibility owner \
  --owner local-user \
  --source project-notes \
  --version v1 \
  --cwd "$WORKSPACE" >"$TMP_ROOT/project-existing.json"

python3 - "$TMP_ROOT/project-init.json" "$TMP_ROOT/private-init.json" "$TMP_ROOT/project-existing.json" <<'PY'
import json
import sys

project, private, existing = [json.load(open(path, encoding="utf-8")) for path in sys.argv[1:]]
assert project["status"] == "created", project
assert private["status"] == "created", private
assert existing["status"] == "existing", existing
assert project["kind"] == "project" and project["visibility"] == "owner", project
assert private["kind"] == "private" and private["visibility"] == "private", private
PY

set +e
run_json knowledge-space-init \
  --space-id project-demo \
  --kind project \
  --visibility owner \
  --owner local-user \
  --source changed \
  --version v2 \
  --cwd "$WORKSPACE" >"$TMP_ROOT/conflict.out" 2>&1
CONFLICT_STATUS=$?
set -e
[[ "$CONFLICT_STATUS" -ne 0 ]] || { echo "metadata conflict unexpectedly succeeded" >&2; exit 1; }
grep -q "metadata 不一致" "$TMP_ROOT/conflict.out" || {
  cat "$TMP_ROOT/conflict.out" >&2
  exit 1
}

printf '%s\n' '项目知识：systemctl restart 会重启服务，但可能中断当前连接。' | \
  run_json knowledge-import-stdin \
    --space-id project-demo \
    --document-id project-guide \
    --title 'Project Guide' \
    --source project-notes \
    --version v1 \
    --owner local-user \
    --visibility owner \
    --cwd "$WORKSPACE" >"$TMP_ROOT/project-import.json"
printf '%s\n' '私有知识：我的部署约定是先执行 dry-run，再申请审批。' | \
  run_json knowledge-import-stdin \
    --space-id private-demo \
    --document-id private-guide \
    --title 'Private Guide' \
    --source private-notes \
    --version v1 \
    --owner local-user \
    --visibility private \
    --cwd "$WORKSPACE" >"$TMP_ROOT/private-import.json"

python3 - "$TMP_ROOT/project-import.json" "$TMP_ROOT/private-import.json" <<'PY'
import json
import sys

for path, expected in zip(sys.argv[1:], ("project-demo", "private-demo")):
    value = json.load(open(path, encoding="utf-8"))
    assert value["status"] == "imported", value
    assert value["space_id"] == expected, value
    assert value["chunks_written"] > 0, value
    assert value["embedding_job"]["status"] == "pending", value
PY

run_json knowledge-search 'systemctl restart' \
  --space-id project-demo --owner local-user --visibility owner \
  --cwd "$WORKSPACE" >"$TMP_ROOT/project-search.json"
run_json knowledge-search 'dry-run' \
  --space-id private-demo --owner local-user --visibility private \
  --cwd "$WORKSPACE" >"$TMP_ROOT/private-search.json"

python3 - "$TMP_ROOT/project-search.json" "$TMP_ROOT/private-search.json" <<'PY'
import json
import sys

project, private = [json.load(open(path, encoding="utf-8")) for path in sys.argv[1:]]
assert project["space_id"] == "project-demo" and project["results"], project
assert all(item["space_id"] == "project-demo" for item in project["results"]), project
assert private["space_id"] == "private-demo" and private["results"], private
assert all(item["space_id"] == "private-demo" for item in private["results"]), private
PY

set +e
run_json knowledge-search 'systemctl restart' \
  --space-id private-demo --owner wrong-owner --visibility private \
  --cwd "$WORKSPACE" >"$TMP_ROOT/wrong-owner.out" 2>&1
WRONG_OWNER_STATUS=$?
set -e
[[ "$WRONG_OWNER_STATUS" -ne 0 ]] || { echo "wrong owner unexpectedly queried private space" >&2; exit 1; }

run_json knowledge-worker --max-jobs 10 --cwd "$WORKSPACE" >"$TMP_ROOT/worker.json"
run_json knowledge-vector-search 'dry-run' \
  --space-id private-demo --owner local-user --visibility private \
  --cwd "$WORKSPACE" >"$TMP_ROOT/private-vector.json"
python3 - "$TMP_ROOT/private-vector.json" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["space_id"] == "private-demo" and value["results"], value
assert all(item["space_id"] == "private-demo" for item in value["results"]), value
PY

printf '%s\n' '项目知识更新：systemctl restart 需要审批。' | \
  run_json knowledge-import-stdin \
    --space-id project-demo \
    --document-id project-guide \
    --title 'Project Guide' \
    --source project-notes \
    --version v1 \
    --owner local-user \
    --visibility owner \
    --cwd "$WORKSPACE" >"$TMP_ROOT/project-reimport.json"
run_json knowledge-worker --max-jobs 10 --cwd "$WORKSPACE" >"$TMP_ROOT/worker-reimport.json"

python3 - "$WORKSPACE/.yunxi/knowledge/knowledge.sqlite3" <<'PY'
import sqlite3
import sys

db = sqlite3.connect(sys.argv[1])
spaces = dict(db.execute("SELECT space_id, kind FROM knowledge_spaces"))
assert spaces == {"project-demo": "project", "private-demo": "private"}, spaces
documents = dict(db.execute("SELECT document_id, space_id FROM knowledge_documents"))
assert documents == {"project-guide": "project-demo", "private-guide": "private-demo"}, documents
jobs = db.execute("SELECT COUNT(*) FROM knowledge_embedding_jobs").fetchone()[0]
vectors = db.execute("SELECT COUNT(*) FROM knowledge_vectors").fetchone()[0]
assert jobs == 2 and vectors > 0, (jobs, vectors)
PY

[[ -f "$WORKSPACE/.yunxi/knowledge/knowledge.sqlite3" ]] || exit 1
[[ ! -f "$WORKSPACE/.yunxi/long-term-vectors.sqlite3" ]] || {
  echo "knowledge smoke touched the long-term memory database" >&2
  exit 1
}
echo "knowledge-project-private-smoke=ok"
