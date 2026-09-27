#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
pkgbuild="$script_dir/PKGBUILD"
repo_root=$(cd -- "$script_dir/../../.." && pwd)
service="$script_dir/yunxi-linux.service"
worker_service="$script_dir/yunxi-knowledge-worker@.service"
preflight="$script_dir/preflight.sh"
lifecycle_smoke="$script_dir/lifecycle-smoke.sh"

fail() {
  printf 'package-smoke: %s\n' "$1" >&2
  exit 1
}

[[ -f "$pkgbuild" ]] || fail "missing PKGBUILD"
[[ -f "$service" ]] || fail "missing systemd user service"
[[ -f "$worker_service" ]] || fail "missing knowledge worker template"
[[ -f "$preflight" ]] || fail "missing packaging preflight"
[[ -f "$lifecycle_smoke" ]] || fail "missing lifecycle smoke"

grep -Eq "^pkgname=yunxi-native$" "$pkgbuild" \
  || fail "PKGBUILD package name must be yunxi-native"
grep -Eq "^pkgver=2\\.3\\.3\\.hotfix\\.32$" "$pkgbuild" \
  || fail "PKGBUILD package version is unexpected"
grep -Eq "^pkgrel=[0-9]+$" "$pkgbuild" \
  || fail "PKGBUILD package release must be numeric"
grep -Eq "^arch=\\('x86_64' 'aarch64'\\)$" "$pkgbuild" \
  || fail "PKGBUILD architecture list is incomplete"
grep -Eq "^_commit='[0-9a-f]{40}'$" "$pkgbuild" \
  || fail "PKGBUILD must pin a full 40-character hexadecimal source commit"
grep -Fq 'source=("${pkgname}::git+${url}.git#commit=${_commit}")' "$pkgbuild" \
  || fail "PKGBUILD source must use the pinned commit"
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
grep -Fq '"${pkgdir}/usr/lib/systemd/user/yunxi-knowledge-worker@.service"' "$pkgbuild" \
  || fail "knowledge worker template install path is missing"
grep -Fq '"${pkgdir}/usr/share/doc/${pkgname}/README.md"' "$pkgbuild" \
  || fail "README install target is missing"
grep -Fq '"${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"' "$pkgbuild" \
  || fail "LICENSE install target is missing"

# Keep the Arch package version tied to the workspace Cargo version. Cargo
# uses a hyphen for the hotfix channel while Arch pkgver uses dots, so the
# conversion is intentionally explicit for the current packaging format.
cargo_version=$(sed -n '/^\[workspace\.package\]/,/^\[/ { s/^version = "\([^"]*\)".*/\1/p; }' \
  "$repo_root/Cargo.toml")
[[ -n "$cargo_version" ]] || fail "workspace Cargo version is missing"
expected_pkgver=${cargo_version//-/.}
actual_pkgver=$(sed -n 's/^pkgver=\([^[:space:]]*\)$/\1/p' "$pkgbuild")
[[ "$actual_pkgver" == "$expected_pkgver" ]] \
  || fail "PKGBUILD pkgver=$actual_pkgver does not match Cargo pkgver=$expected_pkgver"

grep -Fq 'ExecStart=/usr/bin/yunxi-linux daemon' "$service" \
  || fail "service must use the packaged absolute binary"
grep -Fq 'NoNewPrivileges=yes' "$service" \
  || fail "service must set NoNewPrivileges"
grep -Fq 'MemoryHigh=1536M' "$service" \
  || fail "service must set the memory soft limit"
grep -Fq 'MemoryMax=2G' "$service" \
  || fail "service must set the memory hard limit"
grep -Fq 'TasksMax=128' "$service" \
  || fail "service must cap daemon task count"
grep -Fq 'LimitNOFILE=4096' "$service" \
  || fail "service must cap open files"
grep -Fq 'OOMPolicy=stop' "$service" \
  || fail "service must define OOM behavior"
grep -Fq 'UMask=0077' "$service" \
  || fail "service must set a private umask"
grep -Fq 'WantedBy=default.target' "$service" \
  || fail "service install target is missing"

if grep -Eq 'systemctl[[:space:]]+--user[[:space:]]+enable|systemctl[[:space:]]+enable' "$pkgbuild"; then
  fail "PKGBUILD must not auto-enable the user service"
fi

grep -Fq -- '--workspace %I' "$worker_service" \
  || fail "knowledge worker must require an explicit workspace"
if grep -Eq '^WantedBy=' "$worker_service"; then
  fail "knowledge worker template must not auto-enable unknown workspaces"
fi

printf 'package-smoke=ok commit=%s\n' "$(sed -n "s/^_commit='\([^']*\)'$/\1/p" "$pkgbuild")"
