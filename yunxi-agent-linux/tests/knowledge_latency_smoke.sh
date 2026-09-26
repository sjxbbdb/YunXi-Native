#!/usr/bin/env bash
set -euo pipefail

# Measure the opt-in CLI knowledge query diagnostics on a disposable local
# knowledge space.  This is a reproducible observation tool, not a flaky CI
# threshold: absolute latency depends on the host filesystem and CPU.

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
import math
import pathlib
import subprocess
import sys
import tempfile


binary = sys.argv[1]
source_version = "ubuntu-24.04"


def run_json(workspace, *args, diagnostics=False):
    command = [binary, *args, "--cwd", str(workspace)]
    if diagnostics:
        command.extend(["--source-version", source_version, "--limit", "5", "--diagnostics"])
    completed = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(completed.stdout)


def percentile(values, percentile_value):
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile_value * len(ordered))) - 1
    return ordered[rank]


def summary(values):
    return {
        "count": len(values),
        "min_us": min(values),
        "p50_us": percentile(values, 0.50),
        "p95_us": percentile(values, 0.95),
        "max_us": max(values),
    }


with tempfile.TemporaryDirectory(prefix="yunxi-knowledge-latency-") as directory:
    workspace = pathlib.Path(directory)
    collected = []
    for command in ("git", "fish", "systemctl"):
        result = run_json(workspace, "knowledge-help", command)
        if result.get("status") == "ok":
            collected.append(command)
    if not collected:
        raise SystemExit("no allowlisted help command was available")
    run_json(workspace, "knowledge-worker", "--max-jobs", "20")

    fts_queries = [
        "git log",
        "git status",
        "git branch",
        "git remote",
        "git command",
        "git history",
        "git working tree",
        "git repository",
        "git changes",
        "git inspect",
        "git help",
        "git options",
    ]
    vector_queries = [
        "查看 git 提交历史",
        "检查 git 工作区状态",
        "列出 git 分支",
        "查看 git 远程仓库",
        "解释 git 常用命令",
        "如何查看历史记录",
        "当前仓库有哪些变化",
        "git 的帮助和参数",
        "版本控制仓库检查",
        "查看提交和分支信息",
        "工作区是否干净",
        "git 命令行选项",
    ]
    fts = [run_json(workspace, "knowledge-search", query, diagnostics=True) for query in fts_queries]
    vectors = [
        run_json(workspace, "knowledge-vector-search", query, diagnostics=True)
        for query in vector_queries
    ]
    fts_timings = [item["diagnostics"]["retrieval_latency_us"] for item in fts]
    vector_timings = [item["diagnostics"] for item in vectors]
    output = {
        "schema_version": 1,
        "source_version": source_version,
        "workspace": "temporary",
        "sampling": "first query is reported as cold; remaining CLI invocations are warm-ish",
        "collected_help_commands": collected,
        "fts": {
            "cold": summary(fts_timings[:1]),
            "warmish": summary(fts_timings[1:]),
        },
        "vector_embedding": {
            "cold": summary([item["embedding_latency_us"] for item in vector_timings[:1]]),
            "warmish": summary([item["embedding_latency_us"] for item in vector_timings[1:]]),
        },
        "vector_retrieval": {
            "cold": summary([item["retrieval_latency_us"] for item in vector_timings[:1]]),
            "warmish": summary([item["retrieval_latency_us"] for item in vector_timings[1:]]),
        },
        "vector_total": {
            "cold": summary([item["total_latency_us"] for item in vector_timings[:1]]),
            "warmish": summary([item["total_latency_us"] for item in vector_timings[1:]]),
        },
        "result_counts": {
            "fts": [item["diagnostics"]["result_count"] for item in fts],
            "vector": [item["diagnostics"]["result_count"] for item in vectors],
        },
    }
    print(json.dumps(output, ensure_ascii=False, indent=2))
PY
