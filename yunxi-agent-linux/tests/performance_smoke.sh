#!/usr/bin/env bash
set -euo pipefail

# Reproducible local performance observation for the Linux-native host.  This
# intentionally reports measurements without imposing a host-dependent pass
# threshold; the release gate can compare the JSON across machines/releases.

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
import os
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import time


binary = sys.argv[1]


def percentile(values, percentile_value):
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile_value * len(ordered))) - 1
    return ordered[rank]


def summary(values, unit):
    if not values:
        raise AssertionError(f"no {unit} samples")
    return {
        "count": len(values),
        "unit": unit,
        "min": min(values),
        "p50": percentile(values, 0.50),
        "p95": percentile(values, 0.95),
        "max": max(values),
    }


def frame_send(sock, value):
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)


def recv_exact(sock, size):
    result = bytearray()
    while len(result) < size:
        chunk = sock.recv(size - len(result))
        if not chunk:
            raise AssertionError("daemon closed before completing a frame")
        result.extend(chunk)
    return bytes(result)


def frame_recv(sock):
    size = struct.unpack(">I", recv_exact(sock, 4))[0]
    return json.loads(recv_exact(sock, size))


def daemon_ping(socket_path):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(2)
        sock.connect(socket_path)
        frame_send(
            sock,
            {
                "kind": "hello",
                "protocol_version": 2,
                "client": "yunxi-performance-smoke",
                "capabilities": ["ping"],
            },
        )
        hello = frame_recv(sock)
        assert hello["kind"] == "hello_ack", hello
        frame_send(sock, {"kind": "ping", "request_id": "performance-ping"})
        assert frame_recv(sock) == {"kind": "pong", "request_id": "performance-ping"}


def read_rss_kib(pid):
    status = pathlib.Path(f"/proc/{pid}/status")
    for line in status.read_text(encoding="utf-8").splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    raise AssertionError(f"VmRSS is unavailable for daemon pid {pid}")


def run_daemon_once():
    with tempfile.TemporaryDirectory(prefix="yunxi-performance-") as directory:
        root = pathlib.Path(directory)
        runtime = root / "runtime"
        home = root / "home"
        config = home / ".config"
        state = home / ".local" / "state"
        runtime.mkdir()
        home.mkdir()
        config.mkdir(parents=True)
        state.mkdir(parents=True)
        env = os.environ.copy()
        env.update(
            {
                "HOME": str(home),
                "XDG_RUNTIME_DIR": str(runtime),
                "XDG_CONFIG_HOME": str(config),
                "XDG_STATE_HOME": str(state),
            }
        )
        process = subprocess.Popen(
            [binary, "daemon"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        socket_path = runtime / "yunxi" / "yunxi.sock"
        started = time.perf_counter_ns()
        try:
            deadline = time.perf_counter() + 3.0
            while time.perf_counter() < deadline and not socket_path.exists():
                if process.poll() is not None:
                    raise AssertionError("daemon exited before socket became ready")
                time.sleep(0.005)
            if not socket_path.exists():
                raise AssertionError("daemon socket did not become ready within 3s")
            daemon_ping(str(socket_path))
            ready_ms = (time.perf_counter_ns() - started) / 1_000_000
            rss_samples = []
            for _ in range(20):
                if process.poll() is not None:
                    raise AssertionError("daemon exited during RSS sampling")
                rss_samples.append(read_rss_kib(process.pid))
                time.sleep(0.05)
            return ready_ms, rss_samples
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)


process_start_ms = []
for _ in range(9):
    started = time.perf_counter_ns()
    completed = subprocess.run(
        [binary, "--help"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False
    )
    assert completed.returncode == 0, completed.stderr.decode(errors="replace")
    process_start_ms.append((time.perf_counter_ns() - started) / 1_000_000)

daemon_ready_ms = []
daemon_rss_kib = []
for _ in range(5):
    ready_ms, rss_samples = run_daemon_once()
    daemon_ready_ms.append(ready_ms)
    daemon_rss_kib.extend(rss_samples)

print(
    json.dumps(
        {
            "schema_version": 1,
            "binary": pathlib.Path(binary).name,
            "sampling": {
                "process_start": "nine independent --help processes; filesystem may be warm",
                "daemon_ready": "five disposable user daemons; readiness includes socket and ping",
                "daemon_rss": "twenty /proc VmRSS samples per daemon after readiness",
            },
            "process_start_ms": summary(process_start_ms, "ms"),
            "daemon_ready_ms": summary(daemon_ready_ms, "ms"),
            "daemon_rss_kib": summary(daemon_rss_kib, "KiB"),
        },
        ensure_ascii=False,
        indent=2,
    )
)
PY

