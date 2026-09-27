#!/usr/bin/env bash
set -euo pipefail

# Tests place fake kill/renice binaries at the front of PATH and only inspect
# argv; no real process is signalled or reprioritized.
cargo test -p yunxi-agent-tools linux_process -- --nocapture
echo "linux_process smoke passed (fake kill/renice only)"
