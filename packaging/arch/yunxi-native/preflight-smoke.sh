#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
preflight="$script_dir/preflight.sh"

fail() {
  printf 'preflight-smoke: %s\n' "$1" >&2
  exit 1
}

[[ -f "$preflight" ]] || fail "preflight.sh is missing"
grep -Fq 'status=ready' "$preflight" || fail "preflight must define a ready state"
grep -Fq 'data_preserved true' "$preflight" || fail "preflight must report preserved data"
if grep -Eq 'pacman[[:space:]]+(-S|-R|-U)|systemctl[[:space:]].*(enable|start|stop)|rm[[:space:]]+-rf|makepkg[[:space:]]+-si' "$preflight"; then
  fail "preflight must not contain mutating install/service/data commands"
fi
if grep -Eiq 'long-term-vectors|knowledge\.sqlite3|memory.*content|记忆正文' "$preflight"; then
  fail "preflight must not inspect personal memory or knowledge contents"
fi

output=$(bash "$preflight") || {
  status=$?
  [[ "$status" -eq 1 ]] || fail "preflight exited unexpectedly: $status"
  output=$(bash "$preflight" || true)
}
grep -Eq '^schema_version=1$' <<<"$output" || fail "schema version missing"
grep -Eq '^source_commit=[0-9a-f]{40}$' <<<"$output" || fail "source commit missing"
grep -Eq '^data_preserved=true$' <<<"$output" || fail "data preservation marker missing"
grep -Eq '^status=(ready|warning)$' <<<"$output" || fail "unexpected preflight status"
if grep -Eq '^.*=/.+' <<<"$output"; then
  fail "preflight output must not expose absolute paths"
fi

printf 'preflight-smoke=ok\n'
