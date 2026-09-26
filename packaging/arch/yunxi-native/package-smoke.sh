#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
pkgbuild="$script_dir/PKGBUILD"
service="$script_dir/yunxi-linux.service"

fail() {
  printf 'package-smoke: %s\n' "$1" >&2
  exit 1
}

[[ -f "$pkgbuild" ]] || fail "missing PKGBUILD"
[[ -f "$service" ]] || fail "missing systemd user service"

grep -Eq "^_commit='[0-9a-f]{7,40}'$" "$pkgbuild" \
  || fail "PKGBUILD must pin a full hexadecimal source commit"
grep -Fq 'cargo build --release --locked -p yunxi-agent-linux' "$pkgbuild" \
  || fail "release build command is missing"
grep -Fq 'cargo test --release --locked -p yunxi-agent-linux' "$pkgbuild" \
  || fail "package test command is missing"
grep -Fq 'target/release/yunxi-linux' "$pkgbuild" \
  || fail "yunxi-linux release artifact is not packaged"
grep -Fq '"${pkgdir}/usr/bin/yunxi-linux"' "$pkgbuild" \
  || fail "binary install path must be /usr/bin/yunxi-linux"
grep -Fq '"${pkgdir}/usr/lib/systemd/user/yunxi-linux.service"' "$pkgbuild" \
  || fail "user service install path is missing"
grep -Fq '"${pkgdir}/usr/share/doc/${pkgname}/README.md"' "$pkgbuild" \
  || fail "README install target is missing"
grep -Fq '"${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"' "$pkgbuild" \
  || fail "LICENSE install target is missing"

grep -Fq 'ExecStart=/usr/bin/yunxi-linux daemon' "$service" \
  || fail "service must use the packaged absolute binary"
grep -Fq 'NoNewPrivileges=yes' "$service" \
  || fail "service must set NoNewPrivileges"
grep -Fq 'UMask=0077' "$service" \
  || fail "service must set a private umask"
grep -Fq 'WantedBy=default.target' "$service" \
  || fail "service install target is missing"

if grep -Eq 'systemctl[[:space:]]+--user[[:space:]]+enable|systemctl[[:space:]]+enable' "$pkgbuild"; then
  fail "PKGBUILD must not auto-enable the user service"
fi

printf 'package-smoke=ok commit=%s\n' "$(sed -n "s/^_commit='\([^']*\)'$/\1/p" "$pkgbuild")"
