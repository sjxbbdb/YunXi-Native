#!/usr/bin/env bash
set -euo pipefail

# Read-only Arch packaging preflight. This script intentionally does not run
# package installation, service lifecycle commands, or any data migration. It reports whether
# the current checkout is suitable for a later package transaction.
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
pkgbuild="$script_dir/PKGBUILD"

status=ready
warnings=0
blocked=0

emit() {
  printf '%s=%s\n' "$1" "$2"
}

warn() {
  warnings=$((warnings + 1))
  [[ "$status" == ready ]] && status=warning
  emit "$1" warning
}

block() {
  blocked=$((blocked + 1))
  status=blocked
  emit "$1" blocked
}

emit schema_version 1

if [[ "$(uname -s 2>/dev/null || true)" == Linux ]]; then
  emit platform linux
else
  block platform
fi

for required in git; do
  if command -v "$required" >/dev/null 2>&1; then
    emit "tool_${required}" present
  else
    block "tool_${required}"
  fi
done

for optional in makepkg cargo systemd-analyze fish; do
  if command -v "$optional" >/dev/null 2>&1; then
    emit "tool_${optional}" present
  else
    warn "tool_${optional}"
  fi
done

if [[ -f "$pkgbuild" ]]; then
  emit pkgbuild present
else
  block pkgbuild
fi

if [[ -f "$script_dir/yunxi-linux.service" && -f "$script_dir/yunxi-knowledge-worker@.service" ]]; then
  emit service_templates present
else
  block service_templates
fi

if [[ -f "$pkgbuild" ]]; then
  pkgname=$(sed -n 's/^pkgname=\([^[:space:]]*\)$/\1/p' "$pkgbuild")
  pkgver=$(sed -n 's/^pkgver=\([^[:space:]]*\)$/\1/p' "$pkgbuild")
  pkgrel=$(sed -n 's/^pkgrel=\([^[:space:]]*\)$/\1/p' "$pkgbuild")
  source_commit=$(sed -n "s/^_commit='\([^']*\)'$/\1/p" "$pkgbuild")
  [[ "$pkgname" == yunxi-native ]] && emit package_name match || block package_name
  [[ "$pkgrel" =~ ^[0-9]+$ ]] && emit package_release numeric || block package_release
  if [[ "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
    emit source_commit "$source_commit"
  else
    block source_commit
  fi
else
  source_commit=''
fi

# Compare the package version with the Cargo version at the pinned source
# commit. This prevents a package from advertising a different runtime than
# the exact source revision it builds. Cargo's hyphen is mapped to Arch's dot
# form for the current hotfix versioning scheme.
pinned_cargo_version=''
if [[ -d "$repo_root/.git" && "$source_commit" =~ ^[0-9a-f]{40}$ ]] \
  && git -C "$repo_root" cat-file -e "${source_commit}:Cargo.toml" 2>/dev/null; then
  pinned_cargo_version=$(git -C "$repo_root" show "${source_commit}:Cargo.toml" 2>/dev/null \
    | sed -n '/^\[workspace\.package\]/,/^\[/ { s/^version = "\([^"]*\)".*/\1/p; }')
fi
if [[ -n "$pinned_cargo_version" ]]; then
  expected_pkgver=${pinned_cargo_version//-/.}
  [[ "$pkgver" == "$expected_pkgver" ]] \
    && emit package_version match \
    || block package_version
else
  block package_version_source
fi

if [[ -d "$repo_root/.git" && "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  head_commit=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || true)
  if git -C "$repo_root" cat-file -e "${source_commit}^{commit}" 2>/dev/null; then
    emit source_commit_exists true
  else
    block source_commit_exists
  fi
  if [[ -n "$head_commit" ]] && git -C "$repo_root" merge-base --is-ancestor "$source_commit" "$head_commit"; then
    emit source_commit_ancestor true
  else
    block source_commit_ancestor
  fi
else
  block git_checkout
fi

workspace_state=''
if command -v timeout >/dev/null 2>&1 \
  && workspace_state=$(timeout 8s git -C "$repo_root" status --porcelain 2>/dev/null); then
  [[ -z "$workspace_state" ]] && emit workspace clean || warn workspace
else
  warn workspace
fi

if bash -n "$pkgbuild" "$script_dir/package-smoke.sh" "$script_dir/knowledge-worker-unit-smoke.sh" \
  "$script_dir/systemd-unit-smoke.sh" "$script_dir/preflight.sh"; then
  emit shell_syntax ok
else
  block shell_syntax
fi

if bash "$script_dir/package-smoke.sh" >/dev/null \
  && bash "$script_dir/knowledge-worker-unit-smoke.sh" >/dev/null \
  && bash "$script_dir/systemd-unit-smoke.sh" >/dev/null; then
  emit static_smoke ok
else
  block static_smoke
fi

if command -v systemd-analyze >/dev/null 2>&1; then
  emit service_parse delegated
else
  warn service_parse
fi

data_home=${XDG_DATA_HOME:-${HOME:-}/.local/share}
if [[ -d "$data_home" && -w "$data_home" ]] || [[ -d "$(dirname -- "$data_home")" && -w "$(dirname -- "$data_home")" ]]; then
  emit xdg_data writable
else
  warn xdg_data
fi

# The preflight only observes package metadata and filesystem permissions.
emit data_preserved true
emit warnings "$warnings"
emit blocked "$blocked"
emit status "$status"

[[ "$status" != blocked ]]
