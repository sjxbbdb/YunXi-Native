#!/usr/bin/env bash
set -euo pipefail

# Exercise the real man collector without requiring a particular distro to
# ship every page.  Missing man/pages are valid structured outcomes; malformed
# topics/sections must still be rejected before a process is spawned.
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
topics = ["fish", "git", "systemctl", "bash"]

with tempfile.TemporaryDirectory(prefix="yunxi-man-smoke-") as directory:
    workspace = pathlib.Path(directory)
    for topic in topics:
        completed = subprocess.run(
            [
                binary,
                "knowledge-man",
                topic,
                "--section",
                "1",
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
        assert result["collector"] == "linux.man", result
        assert result["argv"] == ["man", "--locale=C", "-P", "cat", "1", topic], result
        assert result["document_id"] == f"system-man:1:{topic}@ubuntu-24.04", result
        assert result["source_version"] == "ubuntu-24.04", result
        assert result["status"] in {"ok", "failed", "unavailable"}, result

    for args in [
        ["../etc/passwd"],
        ["fish", "--section", "1;id"],
    ]:
        rejected = subprocess.run(
            [
                binary,
                "knowledge-man",
                *args,
                "--source-version",
                "ubuntu-24.04",
                "--cwd",
                str(workspace),
            ],
            capture_output=True,
            text=True,
        )
        assert rejected.returncode != 0, rejected.stdout
        assert "unsupported" in rejected.stderr or "invalid" in rejected.stderr, rejected

print("knowledge-man-smoke=ok topics=" + str(len(topics)))
PY
