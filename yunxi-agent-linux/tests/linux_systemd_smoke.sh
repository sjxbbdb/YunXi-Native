#!/usr/bin/env bash
set -euo pipefail

# This smoke intentionally runs the fake-systemctl unit test. The test places
# a temporary `systemctl` at the front of PATH, records argv, and restores PATH
# before returning; no real systemd manager is contacted.
cargo test -p yunxi-agent-tools linux_systemd_fake_runner_records_mutation_audit -- --nocapture
echo "linux_systemd smoke passed (fake systemctl only)"
