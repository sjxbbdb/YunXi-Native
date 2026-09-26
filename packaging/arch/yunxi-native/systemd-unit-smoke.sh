#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
service="$script_dir/yunxi-linux.service"

command -v systemd-analyze >/dev/null 2>&1 || {
  printf 'systemd-unit-smoke=unavailable reason=systemd-analyze-missing\n'
  exit 0
}

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/usr/bin" "$tmp_root/usr/lib/systemd/system"
# systemd-analyze's --root verifier searches the system unit directory.  The
# packaged unit is user-scoped, but its service directives are identical and
# parsing it from this temporary system directory avoids touching /usr.
cp "$service" "$tmp_root/usr/lib/systemd/system/yunxi-linux.service"
chmod 0644 "$tmp_root/usr/lib/systemd/system/yunxi-linux.service"
for target in default.target sysinit.target; do
  printf '[Unit]\nDescription=temporary verification target\n' \
    >"$tmp_root/usr/lib/systemd/system/$target"
done
# The packaged binary is not installed in a source checkout.  A harmless
# executable placeholder lets systemd-analyze validate the unit's absolute
# ExecStart path without starting a daemon or requiring root.
true_binary=$(type -P true 2>/dev/null || true)
[[ -n "$true_binary" && -x "$true_binary" ]] || {
  printf 'systemd-unit-smoke=unavailable reason=true-binary-missing\n'
  exit 0
}
cp "$true_binary" "$tmp_root/usr/bin/yunxi-linux"
chmod 0755 "$tmp_root/usr/bin/yunxi-linux"

systemd-analyze --root="$tmp_root" verify \
  yunxi-linux.service
printf 'systemd-unit-smoke=ok\n'
