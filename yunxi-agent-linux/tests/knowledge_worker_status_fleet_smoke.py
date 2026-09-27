#!/usr/bin/env python3
"""Black-box smoke for the read-only multi-workspace queue snapshot."""

import json
import os
import pathlib
import subprocess
import sys
import tempfile


binary = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else pathlib.Path(
    "target/release/yunxi-linux"
).resolve()
if not binary.is_file():
    raise SystemExit(f"release binary not found: {binary}")


def run(*args, check=True):
    completed = subprocess.run(
        [str(binary), *map(str, args)], capture_output=True, text=True, check=False
    )
    if check and completed.returncode != 0:
        raise AssertionError((completed.args, completed.stdout, completed.stderr))
    return completed


def run_json(*args):
    completed = run(*args)
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise AssertionError((completed.stdout, completed.stderr)) from error


def stable_snapshot(value):
    result = json.loads(json.dumps(value))
    for workspace in result["workspaces"]:
        workspace.pop("observed_at_millis", None)
    return result


with tempfile.TemporaryDirectory(prefix="yunxi-status-fleet-") as directory:
    root = pathlib.Path(directory)
    pending = root / "pending"
    completed = root / "completed"
    missing = root / "missing"
    corrupt = root / "corrupt"
    for workspace in (pending, completed, missing, corrupt):
        workspace.mkdir()

    pending_doc = run_json(
        "knowledge-help",
        "cat",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        pending,
    )
    completed_doc = run_json(
        "knowledge-help",
        "cat",
        "--source-version",
        "ubuntu-24.04",
        "--cwd",
        completed,
    )
    assert pending_doc["status"] == "ok", pending_doc
    assert completed_doc["status"] == "ok", completed_doc

    processed = run_json("knowledge-worker", "--max-jobs", "1", "--cwd", completed)
    assert processed["status"] == "processed", processed

    failed_job = run_json(
        "knowledge-enqueue",
        completed_doc["document_id"],
        "--embedding-model",
        "deliberately-unsupported-model",
        "--cwd",
        completed,
    )
    assert failed_job["status"] == "pending", failed_job
    failed = run_json("knowledge-worker", "--max-jobs", "10", "--cwd", completed)
    assert any(job["status"] == "failed" for job in failed["jobs"]), failed

    database = corrupt / ".yunxi" / "knowledge"
    database.mkdir(parents=True)
    (database / "knowledge.sqlite3").write_bytes(b"not a sqlite database")

    memory_file = pending / ".yunxi" / "memory" / "long-term-vectors.sqlite3"
    memory_file.parent.mkdir(parents=True)
    memory_bytes = b"memory-store-must-not-change"
    memory_file.write_bytes(memory_bytes)

    link = root / "pending-link"
    os.symlink(pending, link, target_is_directory=True)
    before = run_json(
        "knowledge-worker-status",
        "--workspace",
        pending,
        "--workspace",
        link,
        "--workspace",
        completed,
        "--workspace",
        missing,
        "--workspace",
        corrupt,
    )
    assert before["scheduler"] == "read_only_snapshot", before
    assert before["workspace_count"] == 4, before
    assert before["status"] == "degraded", before
    items = before["workspaces"]
    assert items[0]["status"] == "ready" and items[0]["active"]["pending"] >= 1, items
    assert items[1]["status"] == "degraded", items
    assert items[2]["database_present"] is False and items[2]["status"] == "idle", items
    assert items[3]["status"] == "error" and items[3]["error_code"] == "knowledge_store_unavailable", items
    assert all(str(root) not in json.dumps(item) for item in items), items
    assert not (missing / ".yunxi" / "knowledge" / "knowledge.sqlite3").exists()
    assert memory_file.read_bytes() == memory_bytes

    after = run_json(
        "knowledge-worker-status",
        "--workspace",
        pending,
        "--workspace",
        completed,
        "--workspace",
        missing,
        "--workspace",
        corrupt,
    )
    assert stable_snapshot(after) == stable_snapshot(before), (before, after)
    assert memory_file.read_bytes() == memory_bytes

    invalid = run(
        "knowledge-worker-status",
        "--cwd",
        pending,
        "--workspace",
        completed,
        check=False,
    )
    assert invalid.returncode != 0, invalid

print("knowledge-worker-status-fleet-smoke=ok")
