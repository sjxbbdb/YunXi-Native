#!/usr/bin/env bash
set -euo pipefail

# Real Linux P0 catalog acceptance: collect several allowlisted help pages and
# one man page into a candidate generation, then verify provenance, risk labels,
# exact source-version filtering, and the shared FTS/vector publication boundary.
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
    if completed.returncode != 0:
        return None
    if completed.stdout.strip():
        return json.loads(completed.stdout)
    return None


def result_for(command, results):
    return next(
        (item for item in results if item["document_id"].startswith(f"system-help:{command}@")),
        None,
    )


with tempfile.TemporaryDirectory(prefix="yunxi-catalog-smoke-") as directory:
    workspace = pathlib.Path(directory)
    begin = run("knowledge-generation-begin", "--cwd", str(workspace))
    generation = begin["generation"]
    assert begin["status"] == "building", begin

    # These commands are present in the supported Ubuntu/Arch base images and
    # exercise both read-only and mutating/destructive advisory labels.
    help_commands = ("cat", "ls", "grep", "rm")
    staged = {}
    catalog_args = [
        "knowledge-stage-catalog",
        "--generation",
        str(generation),
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
    ]
    for command in help_commands:
        catalog_args.extend(["--command", command])
    catalog = run(*catalog_args)
    assert catalog["summary"] == {"failed": 0, "ok": 4, "total": 4, "unavailable": 0}, catalog
    for item in catalog["results"]:
        assert item["status"] == "ok", item
        staged[item["command"]] = item["document_id"]

    # The same command is collected for a second explicit version so exact
    # source-version filtering has two real documents to distinguish.
    second_version = run(
        "knowledge-stage-help",
        "cat",
        "--generation",
        str(generation),
        "--source-version",
        "arch-rolling",
        "--cwd",
        str(workspace),
    )
    assert second_version["status"] == "ok", second_version
    staged["cat-arch"] = second_version["document_id"]

    man = run(
        "knowledge-stage-man",
        "cat",
        "--section",
        "1",
        "--generation",
        str(generation),
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
    )
    assert man["status"] == "ok", man
    staged["man-cat"] = man["document_id"]

    worker = run(
        "knowledge-generation-worker",
        "--max-jobs",
        "20",
        "--cwd",
        str(workspace),
    )
    assert worker["status"] == "processed", worker
    assert len(worker["jobs"]) == len(staged), worker
    assert all(job["status"] == "completed" for job in worker["jobs"]), worker

    readiness = run(
        "knowledge-generation-readiness",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert readiness["readiness"]["actual_documents"] == len(staged), readiness
    assert readiness["readiness"]["vectors"] > 0, readiness
    assert readiness["readiness"]["pending_jobs"] == 0, readiness
    assert readiness["readiness"]["failed_jobs"] == 0, readiness

    sealed = run(
        "knowledge-generation-seal",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert sealed["status"] == "ready", sealed
    activated = run(
        "knowledge-generation-activate",
        "--generation",
        str(generation),
        "--cwd",
        str(workspace),
    )
    assert activated["status"] == "activated", activated

    keyword = run(
        "knowledge-search",
        "usage",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "50",
    )
    assert keyword["results"], keyword
    assert all(item["generation"] == generation for item in keyword["results"]), keyword
    assert all(item["version"] == "ubuntu-24.04" for item in keyword["results"]), keyword

    vectors = run(
        "knowledge-vector-search",
        "remove files",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "50",
    )
    assert vectors["results"], vectors
    assert all(item["generation"] == generation for item in vectors["results"]), vectors

    cat_ubuntu = run(
        "knowledge-search",
        "Concatenate",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "50",
    )
    cat_arch = run(
        "knowledge-search",
        "Concatenate",
        "--source-version",
        "arch-rolling",
        "--cwd",
        str(workspace),
        "--limit",
        "50",
    )
    ubuntu_match = result_for("cat", cat_ubuntu["results"])
    arch_match = result_for("cat", cat_arch["results"])
    assert ubuntu_match and ubuntu_match["version"] == "ubuntu-24.04", cat_ubuntu
    assert arch_match and arch_match["version"] == "arch-rolling", cat_arch
    assert ubuntu_match["document_id"] != arch_match["document_id"]

    # Risk labels are provenance only; they must survive both FTS and vector
    # publication without becoming an execution shortcut.
    rm_match = result_for("rm", keyword["results"])
    if rm_match is None:
        rm_match = result_for(
            "rm",
            run(
                "knowledge-search",
                "remove",
                "--source-version",
                "ubuntu-24.04",
                "--cwd",
                str(workspace),
                "--limit",
                "50",
            )["results"],
        )
    assert rm_match, "rm help was not searchable"
    metadata = json.loads(rm_match["metadata_json"])
    assert metadata["risk_class"] == "destructive", metadata
    assert metadata["risk_level"] == "read_only_reference", metadata

    man_results = run(
        "knowledge-search",
        "SYNOPSIS",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--limit",
        "50",
    )["results"]
    man_match = next(
        (item for item in man_results if item["document_id"] == staged["man-cat"]),
        None,
    )
    assert man_match, man_results
    man_metadata = json.loads(man_match["metadata_json"])
    assert man_metadata["source_type"] == "man", man_metadata
    assert man_metadata["risk_class"] == "read_only", man_metadata

    # A rejected command must not abort the batch or create a staging document;
    # successful siblings still remain queued in the new candidate generation.
    failed_begin = run("knowledge-generation-begin", "--cwd", str(workspace))
    failed_generation = failed_begin["generation"]
    mixed = run(
        "knowledge-stage-catalog",
        "--generation",
        str(failed_generation),
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        str(workspace),
        "--command",
        "cat",
        "--command",
        "not-allowlisted",
    )
    assert mixed["summary"] == {"failed": 1, "ok": 1, "total": 2, "unavailable": 0}, mixed
    failed_item = next(item for item in mixed["results"] if item["command"] == "not-allowlisted")
    assert failed_item["status"] == "failed", failed_item
    assert "embedding_job" not in failed_item, failed_item
    failed_readiness = run(
        "knowledge-generation-readiness",
        "--generation",
        str(failed_generation),
        "--cwd",
        str(workspace),
    )
    assert failed_readiness["readiness"]["actual_documents"] == 1, failed_readiness

    print(
        "knowledge-catalog-smoke=ok "
        f"documents={len(staged)} generation={generation}"
    )
PY
