#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
unit="$script_dir/yunxi-knowledge-worker@.service"

fail() {
  printf 'knowledge-worker-unit-smoke: %s\n' "$1" >&2
  exit 1
}

[[ -f "$unit" ]] || fail "missing knowledge worker template"
grep -Fq 'ExecStart=/usr/bin/yunxi-linux knowledge-worker --watch' "$unit" \
  || fail "template must use the packaged worker binary"
grep -Fq -- '--workspace %I' "$unit" \
  || fail "template must require one explicit escaped workspace instance"
grep -Fq 'ProtectSystem=strict' "$unit" \
  || fail "template must protect the system tree"
grep -Fq 'ProtectHome=read-only' "$unit" \
  || fail "template must protect home by default"
grep -Fq 'ReadWritePaths=%I/.yunxi' "$unit" \
  || fail "template must narrow workspace writes"
grep -Fq 'ReadWritePaths=%I/.yunxi %h/.local/state/yunxi/knowledge-worker' "$unit" \
  || fail "template must narrow scheduler state writes"
grep -Fq 'NoNewPrivileges=yes' "$unit" \
  || fail "template must set NoNewPrivileges"
grep -Fq 'KillSignal=SIGTERM' "$unit" \
  || fail "template must use graceful stop"
grep -Fq 'UMask=0077' "$unit" \
  || fail "template must set a private umask"
if grep -Eq '^WantedBy=' "$unit"; then
  fail "template must not auto-enable unknown workspaces"
fi

printf 'knowledge-worker-unit-smoke=ok\n'
