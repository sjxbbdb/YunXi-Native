#!/usr/bin/env bash
set -euo pipefail

# Run every allowlisted command-help collector on the real Linux binary.  A
# command may be absent on a minimal distro, but an absent tool must still
# return the structured unavailable result and never turn into arbitrary argv.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"

command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"

python3 - "$BINARY" <<'PY'
import json
import pathlib
import subprocess
import sys
import tempfile

binary = sys.argv[1]
commands = [
    "fish",
    "git",
    "systemctl",
    "pacman",
    "ip",
    "awk",
    "cat",
    "cp",
    "find",
    "grep",
    "ls",
    "rm",
    "sed",
    "tar",
]

with tempfile.TemporaryDirectory(prefix="yunxi-help-smoke-") as directory:
    workspace = pathlib.Path(directory)
    successful = []
    for command in commands:
        completed = subprocess.run(
            [
                binary,
                "knowledge-help",
                command,
                "--source-version",
                "ubuntu-24.04",
                "--cwd",
                str(workspace),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        result = json.loads(completed.stdout)
        assert result["schema_version"] == 1, result
        assert result["collector"] == "linux.command_help", result
        assert result["argv"] == [command, "--help"], result
        assert result["document_id"] == f"system-help:{command}@ubuntu-24.04", result
        assert result["source_version"] == "ubuntu-24.04", result
        assert result["status"] in {"ok", "failed", "unavailable"}, result
        if result["status"] == "ok":
            successful.append(result)

    # A successful collector must survive the complete persistence path: ingest
    # -> durable embedding job -> bounded worker -> FTS/vector retrieval.  Use
    # one real command (cat is available on every supported Linux base image),
    # but keep the initial allowlist loop tolerant of minimal distributions.
    cat = next((item for item in successful if item["document_id"].startswith("system-help:cat@")), None)
    assert cat is not None, "cat --help must be available for provenance verification"
    worker = subprocess.run(
        [
            binary,
            "knowledge-worker",
            "--max-jobs",
            "20",
            "--cwd",
            str(workspace),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    worker_result = json.loads(worker.stdout)
    assert worker_result["status"] == "processed", worker_result
    assert any(job["document_id"] == cat["document_id"] and job["status"] == "completed" for job in worker_result["jobs"]), worker_result

    search = subprocess.run(
        [
            binary,
            "knowledge-search",
            "Concatenate",
            "--source-version",
            "ubuntu-24.04",
            "--cwd",
            str(workspace),
            "--limit",
            "5",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    search_result = json.loads(search.stdout)
    match = next((item for item in search_result["results"] if item["document_id"] == cat["document_id"]), None)
    assert match is not None, search_result
    assert match["version"] == "ubuntu-24.04", match
    metadata = json.loads(match["metadata_json"])
    assert metadata == {
        "argv": ["cat", "--help"],
        "collector": "linux.command_help",
        "command": "cat",
        "risk_level": "read_only_reference",
        "source_type": "command_help",
    }, metadata

    vector_search = subprocess.run(
        [
            binary,
            "knowledge-vector-search",
            "Concatenate",
            "--source-version",
            "ubuntu-24.04",
            "--cwd",
            str(workspace),
            "--limit",
            "5",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    vector_result = json.loads(vector_search.stdout)
    vector_match = next((item for item in vector_result["results"] if item["document_id"] == cat["document_id"]), None)
    assert vector_match is not None, vector_result
    assert vector_match["embedding_model"] == worker_result["embedding_model"], vector_match

    filtered = subprocess.run(
        [
            binary,
            "knowledge-search",
            "Concatenate",
            "--source-version",
            "arch-rolling",
            "--cwd",
            str(workspace),
            "--limit",
            "5",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    filtered_result = json.loads(filtered.stdout)
    assert all(item["document_id"] != cat["document_id"] for item in filtered_result["results"]), filtered_result

    rejected = subprocess.run(
        [
            binary,
            "knowledge-help",
            "grep --help",
            "--source-version",
            "ubuntu-24.04",
            "--cwd",
            str(workspace),
        ],
        capture_output=True,
        text=True,
    )
    assert rejected.returncode != 0, rejected.stdout
    assert "allowlisted" in rejected.stderr or "allowlisted" in rejected.stdout, rejected

print("knowledge-help-smoke=ok commands=" + str(len(commands)))
PY
