#!/usr/bin/env bash
set -euo pipefail

# Black-box acceptance for the candidate-generation publication boundary:
# staging must be invisible until worker + seal + explicit activation, while
# the active generation and the separate long-term memory vector database stay
# untouched until the final atomic switch.
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
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"

python3 - "$BINARY" <<'PY'
import json
import pathlib
import subprocess
import sys
import tempfile

binary = sys.argv[1]


def run(*args, check=True):
    completed = subprocess.run(
        [binary, *args],
        check=False,
        capture_output=True,
        text=True,
    )
    if check and completed.returncode != 0:
        raise AssertionError(
            f"command failed: {args}\nstdout={completed.stdout}\nstderr={completed.stderr}"
        )
    if completed.stdout.strip():
        return json.loads(completed.stdout)
    return None


with tempfile.TemporaryDirectory(prefix="yunxi-generation-smoke-") as directory:
    workspace = pathlib.Path(directory)
    memory_path = workspace / ".yunxi" / "memory" / "long-term-vectors.sqlite3"
    memory_before = memory_path.read_bytes() if memory_path.exists() else None

    begin = run(
        "knowledge-generation-begin",
        "--cwd",
        str(workspace),
    )
    assert begin["status"] == "building", begin
    generation = begin["generation"]
    assert isinstance(generation, int) and generation > 0, begin

    initial = run(
        "knowledge-generation-readiness",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    active_generation = initial["active_generation"]
    assert active_generation != generation, initial
    assert initial["candidate_sealed"] is False, initial

    staged = run(
        "knowledge-stage-help",
        "cat",
        "--generation",
        str(generation),
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
    )
    assert staged["status"] == "ok", staged
    document_id = staged["document_id"]
    assert staged["embedding_job"]["status"] == "pending", staged

    # The candidate is not visible through either active search surface.
    for command in ("knowledge-search", "knowledge-vector-search"):
        result = run(
            command,
            "Concatenate",
            "--source-version",
            "ubuntu-24.04",
            "--cwd",
            str(workspace),
            "--limit",
            "20",
        )
        assert all(item["document_id"] != document_id for item in result["results"]), result

    # Activation before processing/sealing must fail without moving the active pointer.
    rejected = run(
        "knowledge-generation-activate",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
        check=False,
    )
    assert rejected is None, rejected
    after_rejected = run(
        "knowledge-generation-readiness",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert after_rejected["active_generation"] == active_generation, after_rejected

    worker = run(
        "knowledge-generation-worker",
        "--max-jobs",
        "1",
        "--cwd",
        str(workspace),
    )
    assert worker["status"] == "processed", worker
    assert worker["jobs"] and worker["jobs"][0]["status"] == "completed", worker

    ready = run(
        "knowledge-generation-readiness",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    # A building manifest starts with an empty expected-document count.  The
    # readiness inspection must report the staged coverage, but sealing is the
    # explicit operation that records that count and digest in the manifest.
    assert ready["readiness"]["ready"] is False, ready
    assert ready["readiness"]["actual_documents"] == 1, ready
    assert ready["readiness"]["chunks"] > 0, ready
    assert ready["readiness"]["vectors"] > 0, ready
    assert ready["readiness"]["pending_jobs"] == 0, ready
    assert ready["readiness"]["running_jobs"] == 0, ready
    assert ready["readiness"]["failed_jobs"] == 0, ready
    assert ready["active_generation"] == active_generation, ready
    assert ready["candidate_sealed"] is False, ready

    sealed = run(
        "knowledge-generation-seal",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert sealed["status"] == "ready", sealed
    sealed_readiness = run(
        "knowledge-generation-readiness",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert sealed_readiness["candidate_sealed"] is True, sealed_readiness
    assert sealed_readiness["active_generation"] == active_generation, sealed_readiness

    activated = run(
        "knowledge-generation-activate",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert activated["status"] == "activated", activated
    assert activated["active_generation"] == generation, activated

    # The atomic switch exposes both FTS and vector evidence from the same
    # generation, with the document's explicit source-version provenance.
    keyword = run(
        "knowledge-search",
        "Concatenate",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "20",
    )
    keyword_match = next(
        item for item in keyword["results"] if item["document_id"] == document_id
    )
    assert keyword_match["generation"] == generation, keyword_match
    assert keyword_match["version"] == "ubuntu-24.04", keyword_match

    vector = run(
        "knowledge-vector-search",
        "Concatenate",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "20",
    )
    vector_match = next(
        item for item in vector["results"] if item["document_id"] == document_id
    )
    assert vector_match["generation"] == generation, vector_match
    assert vector_match["embedding_model"], vector_match

    memory_after = memory_path.read_bytes() if memory_path.exists() else None
    assert memory_after == memory_before, "knowledge generation touched long-term memory vectors"
    assert (workspace / ".yunxi" / "knowledge" / "knowledge.sqlite3").exists()

print("knowledge-generation-smoke=ok")
PY
