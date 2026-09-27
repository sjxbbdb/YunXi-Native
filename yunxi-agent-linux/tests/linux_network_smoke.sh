#!/usr/bin/env bash
set -euo pipefail

# The test creates a temporary fake `ip` at the front of PATH, records argv,
# and restores PATH. No host network configuration is changed.
cargo test -p yunxi-agent-tools linux_network -- --nocapture
echo "linux_network smoke passed (fake ip only)"
