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
