#!/usr/bin/env bash
set -euo pipefail

# Tests place fake kill/renice binaries at the front of PATH; no real process
# is signalled or reprioritized, and cancellation uses a controlled fake runner.
cargo test -p yunxi-agent-tools linux_process -- --nocapture
echo "linux_process smoke passed (fake kill/renice plus cancellation)"
