#!/usr/bin/env bash
set -euo pipefail

# Real Linux acceptance for the offline knowledge-pack boundary.  The pack is
# explicit, local data: no network, HOME scan, command execution, or system
# space write is involved.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"

python3 - "$BINARY" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile

binary = sys.argv[1]


def run(*args, check=True):
    completed = subprocess.run(
        [binary, *args], check=False, capture_output=True, text=True
    )
    if check and completed.returncode != 0:
        raise AssertionError(
            f"command failed: {args}\nstdout={completed.stdout}\nstderr={completed.stderr}"
        )
    if completed.returncode != 0:
        return completed
    return json.loads(completed.stdout) if completed.stdout.strip() else None


def fail(*args):
    result = run(*args, check=False)
    assert result.returncode != 0, args


def manifest(pack, *, schema=1, documents=None):
    data = {
        "schema_version": schema,
        "pack_id": "linux-pack-smoke",
        "title": "Offline Linux smoke pack",
        "source": "linux-pack-smoke",
        "version": "2026.09",
        "license": "CC-BY-4.0",
        "verified_at": "2026-09-27T00:00:00Z",
        "documents": documents or [
            {"document_id": "doc-b", "path": "docs/b.md", "title": "B", "topic": "shell", "risk_class": "read_only"},
            {"document_id": "doc-a", "path": "docs/a.md", "title": "A", "topic": "terminal", "risk_class": "read_only"},
        ],
    }
    (pack / "manifest.json").write_text(json.dumps(data), encoding="utf-8")


with tempfile.TemporaryDirectory(prefix="yunxi-knowledge-pack-") as directory:
    workspace = pathlib.Path(directory) / "workspace"
    workspace.mkdir()
    memory = workspace / ".yunxi" / "memory"
    memory.mkdir(parents=True)
    memory_file = memory / "long-term-vectors.sqlite3"
    memory_file.write_bytes(b"memory-domain-sentinel\x00")
    before_memory = hashlib.sha256(memory_file.read_bytes()).hexdigest()

    owner = run("knowledge-principal")["principal"]
    init = run(
        "knowledge-space-init",
        "--space-id", "pack-smoke",
        "--kind", "project",
        "--visibility", "owner",
        "--owner", owner,
        "--source", "linux-pack-smoke",
        "--version", "2026.09",
        "--cwd", str(workspace),
    )
    assert init["status"] == "created", init

    pack = workspace / "pack"
    (pack / "docs").mkdir(parents=True)
    (pack / "docs" / "a.md").write_text(
        "YunXi offline pack marker. Do not execute $(touch injected-marker).", encoding="utf-8"
    )
    (pack / "docs" / "b.md").write_text(
        "A local Linux terminal reference with a stable retrieval phrase.", encoding="utf-8"
    )
    manifest(pack)
    imported = run(
        "knowledge-import-pack", str(pack),
        "--space-id", "pack-smoke", "--owner", owner, "--visibility", "owner",
        "--cwd", str(workspace),
    )
    assert imported["status"] == "imported", imported
    assert [item["document_id"] for item in imported["documents"]] == ["doc-a", "doc-b"], imported
    assert not (workspace / "injected-marker").exists()
    assert hashlib.sha256(memory_file.read_bytes()).hexdigest() == before_memory
    worker = run("knowledge-worker", "--max-jobs", "10", "--cwd", str(workspace))
    assert worker["status"] == "processed", worker
    assert len(worker["jobs"]) == 2, worker

    keyword = run("knowledge-search", "stable retrieval phrase", "--space-id", "pack-smoke",
                  "--owner", owner, "--visibility", "owner", "--cwd", str(workspace), "--limit", "20")
    assert any(row["document_id"] == "doc-b" for row in keyword["results"]), keyword
    metadata = json.loads(next(row for row in keyword["results"] if row["document_id"] == "doc-b")["metadata_json"])
    for key in ("pack_id", "pack_version", "verified_at", "license", "topic", "risk_class"):
        assert key in metadata, metadata
    vectors = run("knowledge-vector-search", "stable retrieval phrase", "--space-id", "pack-smoke",
                  "--owner", owner, "--visibility", "owner", "--cwd", str(workspace), "--limit", "20")
    assert any(row["document_id"] == "doc-b" for row in vectors["results"]), vectors

    # A caller cannot redirect a pack into another owner, and system spaces are
    # rejected by the existing import boundary.
    fail("knowledge-import-pack", str(pack), "--space-id", "pack-smoke", "--owner", "someone-else",
         "--visibility", "owner", "--cwd", str(workspace))
    system = run("knowledge-generation-begin", "--cwd", str(workspace))
    assert system["status"] == "building", system
    fail("knowledge-import-pack", str(pack), "--space-id", "system-linux", "--owner", owner,
         "--visibility", "public", "--cwd", str(workspace))

    # Every malformed pack fails before any document is written.
    cases = []
    def case(name, docs=None, schema=1, setup=None):
        root = workspace / name
        root.mkdir()
        if setup:
            setup(root)
        else:
            (root / "docs").mkdir()
            (root / "docs" / "a.md").write_text("invalid-case", encoding="utf-8")
        manifest(root, schema=schema, documents=docs)
        cases.append(root)

    case("bad-schema", schema=2)
    case("missing-file", docs=[{"document_id": "missing", "path": "docs/no.md", "title": "N", "topic": "t", "risk_class": "read_only"}])
    case("duplicate-id", docs=[{"document_id": "dup", "path": "docs/a.md", "title": "A", "topic": "t", "risk_class": "read_only"}, {"document_id": "dup", "path": "docs/b.md", "title": "B", "topic": "t", "risk_class": "read_only"}])
    case("parent-path", docs=[{"document_id": "parent", "path": "../escape.md", "title": "P", "topic": "t", "risk_class": "read_only"}])
    case("absolute-path", docs=[{"document_id": "absolute", "path": "/tmp/escape.md", "title": "P", "topic": "t", "risk_class": "read_only"}])
    case("yunxi-path", docs=[{"document_id": "state", "path": ".yunxi/state.md", "title": "S", "topic": "t", "risk_class": "read_only"}])
    def symlink_setup(root):
        (root / "docs").mkdir()
        target = root / "outside.md"
        target.write_text("outside", encoding="utf-8")
        (root / "docs" / "link.md").symlink_to(target)
    case("symlink", docs=[{"document_id": "link", "path": "docs/link.md", "title": "L", "topic": "t", "risk_class": "read_only"}], setup=symlink_setup)
    def non_utf8_setup(root):
        (root / "docs").mkdir()
        (root / "docs" / "bad.md").write_bytes(b"\xff\xfe")
    case("non-utf8", docs=[{"document_id": "utf8", "path": "docs/bad.md", "title": "U", "topic": "t", "risk_class": "read_only"}], setup=non_utf8_setup)
    for malformed in cases:
        fail("knowledge-import-pack", str(malformed), "--space-id", "pack-smoke", "--owner", owner,
             "--visibility", "owner", "--cwd", str(workspace))

    retract = run("knowledge-retract", "doc-a", "--space-id", "pack-smoke", "--owner", owner,
                   "--visibility", "owner", "--cwd", str(workspace))
    assert retract["status"] == "retracted", retract
    gone = run("knowledge-search", "YunXi offline pack marker", "--space-id", "pack-smoke",
               "--owner", owner, "--visibility", "owner", "--cwd", str(workspace), "--limit", "20")
    assert not any(row["document_id"] == "doc-a" for row in gone["results"]), gone
    assert hashlib.sha256(memory_file.read_bytes()).hexdigest() == before_memory

print("knowledge-import-pack-smoke=ok")
PY
