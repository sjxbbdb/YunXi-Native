#!/usr/bin/env python3
"""Black-box smoke for the explicit multi-workspace knowledge worker.

The fixture is intentionally disposable.  It never opens the user's memory
store and only invokes the release binary supplied as argv[1].
"""

from __future__ import annotations

import json
import os
import pathlib
import sqlite3
import subprocess
import sys
import tempfile
import time
from typing import Any


TIMEOUT_SECONDS = 20
OWNER = "fleet-smoke-owner"
SOURCE = "fleet-smoke"
VERSION = "v1"


def run(binary: str, *args: str, input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [binary, *args],
        input=input_text,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )


def run_ok(binary: str, *args: str, input_text: str | None = None) -> dict[str, Any]:
    completed = run(binary, *args, input_text=input_text)
    if completed.returncode != 0:
        raise AssertionError(
            f"command failed ({completed.returncode}): {args}\n"
            f"stdout={completed.stdout}\nstderr={completed.stderr}"
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise AssertionError(f"command did not emit JSON: {args}\n{completed.stdout}") from error


def run_failed(binary: str, *args: str) -> subprocess.CompletedProcess[str]:
    completed = run(binary, *args)
    if completed.returncode == 0:
        raise AssertionError(f"command unexpectedly succeeded: {args}\n{completed.stdout}")
    return completed


def decode_json_stream(output: str) -> list[dict[str, Any]]:
    decoder = json.JSONDecoder()
    values: list[dict[str, Any]] = []
    offset = 0
    while offset < len(output):
        while offset < len(output) and output[offset].isspace():
            offset += 1
        if offset == len(output):
            break
        value, end = decoder.raw_decode(output, offset)
        if not isinstance(value, dict):
            raise AssertionError(f"expected object in JSON stream: {value!r}")
        values.append(value)
        offset = end
    return values


def init_space(binary: str, workspace: pathlib.Path, space_id: str) -> None:
    run_ok(
        binary,
        "knowledge-space-init",
        "--space-id",
        space_id,
        "--kind",
        "private",
        "--visibility",
        "private",
        "--owner",
        OWNER,
        "--source",
        SOURCE,
        "--version",
        VERSION,
        "--cwd",
        str(workspace),
    )


def import_document(
    binary: str,
    workspace: pathlib.Path,
    space_id: str,
    document_id: str,
    content: str,
) -> None:
    result = run_ok(
        binary,
        "knowledge-import-stdin",
        "--space-id",
        space_id,
        "--document-id",
        document_id,
        "--title",
        document_id,
        "--source",
        SOURCE,
        "--version",
        VERSION,
        "--owner",
        OWNER,
        "--visibility",
        "private",
        "--cwd",
        str(workspace),
        input_text=content + "\n",
    )
    assert result["status"] == "imported", result
    assert result["embedding_job"]["status"] == "pending", result


def worker_db(workspace: pathlib.Path) -> pathlib.Path:
    return workspace / ".yunxi" / "knowledge" / "knowledge.sqlite3"


def memory_fixture(workspace: pathlib.Path) -> pathlib.Path:
    path = workspace / ".yunxi" / "memory" / "workspace-memory.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b'{"id":"fixture","content":"must not change"}\n')
    return path


def assert_fleet_shape(value: dict[str, Any], count: int, max_jobs: int) -> None:
    assert value["schema_version"] == 1, value
    assert value["scheduler"] == "explicit_round_robin", value
    assert value["workspace_count"] == count, value
    assert value["max_jobs"] == max_jobs, value
    assert 0 <= value["next_workspace_index"] < count, value
    assert len(value["workspaces"]) == count, value
    for expected_index, item in enumerate(value["workspaces"]):
        assert item["workspace_index"] == expected_index, value
        assert item["status"] in {"idle", "processed", "error"}, value
        assert "workspace" not in item, value
        if item["status"] == "error":
            assert set(item) <= {
                "workspace_index",
                "status",
                "error_code",
                "retry_after_secs",
                "jobs",
            }, value
            assert isinstance(item["error_code"], str) and item["error_code"], value
        else:
            assert isinstance(item["jobs"], list), value


def assert_vector_hit(binary: str, workspace: pathlib.Path, space_id: str, phrase: str) -> None:
    value = run_ok(
        binary,
        "knowledge-vector-search",
        phrase,
        "--space-id",
        space_id,
        "--owner",
        OWNER,
        "--visibility",
        "private",
        "--cwd",
        str(workspace),
    )
    assert value["results"], value
    assert any(phrase in result.get("content", "") for result in value["results"]), value


def sqlite_job_statuses(path: pathlib.Path) -> list[str]:
    with sqlite3.connect(path) as connection:
        return [row[0] for row in connection.execute("SELECT status FROM knowledge_embedding_jobs ORDER BY job_id")]


def assert_rejected_without_write(binary: str, args: list[str], paths: list[pathlib.Path]) -> None:
    before = {path: path.read_bytes() if path.is_file() else None for path in paths}
    completed = run_failed(binary, *args)
    after = {path: path.read_bytes() if path.is_file() else None for path in paths}
    assert before == after, (args, completed.stdout, completed.stderr)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} PATH_TO_YUNXI_LINUX")
    binary = os.path.abspath(sys.argv[1])
    if not os.access(binary, os.X_OK):
        raise SystemExit(f"release binary is not executable: {binary}")

    with tempfile.TemporaryDirectory(prefix="yunxi-knowledge-fleet-") as root_text:
        root = pathlib.Path(root_text)
        workspaces = [root / name for name in ("alpha", "beta", "unselected")]
        for workspace in workspaces:
            workspace.mkdir()

        memory_paths = [memory_fixture(workspace) for workspace in workspaces]
        memory_snapshots = {path: path.read_bytes() for path in memory_paths}
        for workspace in workspaces:
            init_space(binary, workspace, "shared-private")

        # The same document id is deliberately used in two isolated databases;
        # content proves that the worker never merges workspace state.
        import_document(binary, workspaces[0], "shared-private", "same-document", "alpha-only knowledge")
        import_document(binary, workspaces[1], "shared-private", "same-document", "beta-only knowledge")
        import_document(binary, workspaces[2], "shared-private", "same-document", "unselected knowledge")
        untouched_third_db = worker_db(workspaces[2]).read_bytes()

        fleet = run_ok(
            binary,
            "knowledge-worker",
            "--workspace",
            str(workspaces[0]),
            "--workspace",
            str(workspaces[1]),
            "--max-jobs",
            "2",
        )
        assert_fleet_shape(fleet, 2, 2)
        assert fleet["jobs_processed"] == 2, fleet
        assert [item["status"] for item in fleet["workspaces"]] == ["processed", "processed"], fleet
        assert all(len(item["jobs"]) == 1 for item in fleet["workspaces"]), fleet
        assert worker_db(workspaces[2]).read_bytes() == untouched_third_db
        assert all(path.read_bytes() == memory_snapshots[path] for path in memory_paths)
        assert_vector_hit(binary, workspaces[0], "shared-private", "alpha-only knowledge")
        assert_vector_hit(binary, workspaces[1], "shared-private", "beta-only knowledge")
        assert sqlite_job_statuses(worker_db(workspaces[2])) == ["pending"]

        # Canonical path de-duplication must not spend the budget twice on one
        # database.  The symlink path points to alpha on Unix/WSL.
        alias = root / "alpha-alias"
        os.symlink(workspaces[0], alias, target_is_directory=True)
        import_document(binary, workspaces[0], "shared-private", "alias-document", "alpha alias knowledge")
        dedup = run_ok(
            binary,
            "knowledge-worker",
            "--workspace",
            str(workspaces[0]),
            "--workspace",
            str(alias),
            "--max-jobs",
            "1",
        )
        assert_fleet_shape(dedup, 1, 1)
        assert dedup["workspace_count"] == 1 and dedup["jobs_processed"] == 1, dedup

        # A corrupt selected database becomes an item-scoped error; it must not
        # prevent a later workspace from receiving its one job.
        bad = root / "bad"
        good = root / "good"
        bad_db = bad / ".yunxi" / "knowledge" / "knowledge.sqlite3"
        bad_db.parent.mkdir(parents=True)
        bad_db.write_bytes(b"not a sqlite database")
        good.mkdir()
        init_space(binary, good, "shared-private")
        import_document(binary, good, "shared-private", "good-document", "good database knowledge")
        bad_fleet = run_ok(
            binary,
            "knowledge-worker",
            "--workspace",
            str(bad),
            "--workspace",
            str(good),
            "--max-jobs",
            "1",
        )
        assert_fleet_shape(bad_fleet, 2, 1)
        assert bad_fleet["workspaces"][0]["status"] == "error", bad_fleet
        assert bad_fleet["workspaces"][1]["status"] == "processed", bad_fleet
        assert str(bad) not in json.dumps(bad_fleet), bad_fleet
        assert_vector_hit(binary, good, "shared-private", "good database knowledge")

        # Validation must happen before opening or creating any workspace DB.
        conflict_a = root / "conflict-a"
        conflict_b = root / "conflict-b"
        conflict_a.mkdir()
        conflict_b.mkdir()
        conflict_paths = [worker_db(conflict_a), worker_db(conflict_b)]
        assert_rejected_without_write(
            binary,
            [
                "knowledge-worker",
                "--cwd",
                str(conflict_a),
                "--workspace",
                str(conflict_b),
                "--max-jobs",
                "1",
            ],
            conflict_paths,
        )

        file_path = root / "not-a-directory"
        file_path.write_text("fixture", encoding="utf-8")
        assert_rejected_without_write(
            binary,
            ["knowledge-worker", "--workspace", str(file_path), "--max-jobs", "1"],
            [file_path],
        )
        missing = root / "does-not-exist"
        assert_rejected_without_write(
            binary,
            ["knowledge-worker", "--workspace", str(missing), "--max-jobs", "1"],
            [missing],
        )
        assert_rejected_without_write(
            binary,
            ["knowledge-worker", "--workspace", str(conflict_a), "--max-jobs", "0"],
            conflict_paths,
        )
        assert_rejected_without_write(
            binary,
            ["knowledge-worker", "--workspace", str(conflict_a), "--max-jobs", "1001"],
            conflict_paths,
        )
        too_many = [root / f"bound-{index}" for index in range(33)]
        for workspace in too_many:
            workspace.mkdir()
        assert_rejected_without_write(
            binary,
            [
                "knowledge-worker",
                "--max-jobs",
                "1",
                *sum((["--workspace", str(workspace)] for workspace in too_many), []),
            ],
            [worker_db(workspace) for workspace in too_many],
        )
        assert_rejected_without_write(
            binary,
            [
                "knowledge-worker",
                "--watch",
                "--interval-secs",
                "0",
                "--workspace",
                str(conflict_a),
                "--max-jobs",
                "1",
            ],
            conflict_paths,
        )
        assert_rejected_without_write(
            binary,
            [
                "knowledge-worker",
                "--watch",
                "--interval-secs",
                "3601",
                "--workspace",
                str(conflict_a),
                "--max-jobs",
                "1",
            ],
            conflict_paths,
        )

        # A watch round has a global budget of one.  The cursor must advance so
        # that the second round visits beta instead of starving it behind
        # alpha's remaining backlog.
        watch_alpha = root / "watch-alpha"
        watch_beta = root / "watch-beta"
        watch_alpha.mkdir()
        watch_beta.mkdir()
        init_space(binary, watch_alpha, "watch-private")
        init_space(binary, watch_beta, "watch-private")
        for index in range(3):
            import_document(binary, watch_alpha, "watch-private", f"alpha-{index}", f"alpha backlog {index}")
        import_document(binary, watch_beta, "watch-private", "beta-0", "beta fairness document")

        watch = subprocess.Popen(
            [
                binary,
                "knowledge-worker",
                "--watch",
                "--interval-secs",
                "1",
                "--workspace",
                str(watch_alpha),
                "--workspace",
                str(watch_beta),
                "--max-jobs",
                "1",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            time.sleep(2.3)
            watch.terminate()
            stdout, stderr = watch.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            watch.kill()
            stdout, stderr = watch.communicate(timeout=5)
            raise AssertionError(f"watch worker did not stop within 5 seconds: {stderr}")
        finally:
            if watch.poll() is None:
                watch.kill()
                watch.wait(timeout=5)
        assert watch.returncode == 0, (watch.returncode, stdout, stderr)
        stream = decode_json_stream(stdout)
        assert len(stream) >= 3, (stdout, stderr)
        rounds = [item for item in stream if item.get("status") in {"idle", "processed", "error"}]
        stopped = [item for item in stream if item.get("status") == "stopped"]
        assert stopped and stopped[-1]["reason"] == "terminate", stream
        assert rounds[0]["workspaces"][0]["jobs"], stream
        assert rounds[1]["workspaces"][1]["jobs"], stream
        assert rounds[0]["jobs_processed"] == 1 and rounds[1]["jobs_processed"] == 1, stream

    print("knowledge-worker-fleet-smoke=ok")


if __name__ == "__main__":
    main()
