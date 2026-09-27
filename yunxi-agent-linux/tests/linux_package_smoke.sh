#!/usr/bin/env bash
set -euo pipefail

# This smoke uses the repository's fake pacman runner. It never invokes the
# host package manager and therefore does not require root or an Arch host.
cargo test -p yunxi-agent-tools linux_package_ -- --nocapture
echo "linux_package smoke passed (fake pacman only)"
