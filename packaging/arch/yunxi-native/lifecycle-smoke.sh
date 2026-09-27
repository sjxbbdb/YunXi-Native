#!/usr/bin/env bash
set -euo pipefail

# This is a transaction-contract smoke, not a pacman replacement. It uses a
# temporary package root to prove that package files can be installed,
# upgraded, rolled back and removed without touching user data or enabling a
# service implicitly.

fail() {
  printf 'lifecycle-smoke: %s\n' "$1" >&2
  exit 1
}

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

PACKAGE_ROOT="$TMP_ROOT/package-root"
STATE_ROOT="$TMP_ROOT/state"
DATA_ROOT="$TMP_ROOT/workspace"
FISH_HOOK="$TMP_ROOT/fish/conf.d/yunxi.fish"
V1="$TMP_ROOT/pkg-v1"
V2="$TMP_ROOT/pkg-v2"
BACKUP="$TMP_ROOT/backup"
mkdir -p "$PACKAGE_ROOT" "$STATE_ROOT" "$DATA_ROOT/.yunxi/memory" \
  "$(dirname "$FISH_HOOK")" "$V1/usr/bin" "$V1/usr/lib/systemd/user" \
  "$V1/usr/share/doc/yunxi-native" "$V1/usr/share/licenses/yunxi-native" \
  "$V2/usr/bin" "$V2/usr/lib/systemd/user" \
  "$V2/usr/share/doc/yunxi-native" "$V2/usr/share/licenses/yunxi-native"

printf 'fixture-memory\n' >"$DATA_ROOT/.yunxi/memory/long-term-vectors.sqlite3"
printf 'fixture-knowledge\n' >"$DATA_ROOT/.yunxi/knowledge.sqlite3"
printf 'fixture-scheduler\n' >"$STATE_ROOT/scheduler.json"
printf '# YunXi fish hook\n' >"$FISH_HOOK"

printf '#!/bin/sh\nprintf "yunxi v1\\n"\n' >"$V1/usr/bin/yunxi-linux"
printf 'v1\n' >"$V1/usr/lib/systemd/user/yunxi-linux.service"
printf 'v1\n' >"$V1/usr/share/doc/yunxi-native/README.md"
printf 'license\n' >"$V1/usr/share/licenses/yunxi-native/LICENSE"
chmod 0755 "$V1/usr/bin/yunxi-linux"

printf '#!/bin/sh\nprintf "yunxi v2\\n"\n' >"$V2/usr/bin/yunxi-linux"
printf 'v2\n' >"$V2/usr/lib/systemd/user/yunxi-linux.service"
printf 'v2\n' >"$V2/usr/share/doc/yunxi-native/README.md"
printf 'license\n' >"$V2/usr/share/licenses/yunxi-native/LICENSE"
printf 'migration-fails\n' >"$V2/migration.fail"
chmod 0755 "$V2/usr/bin/yunxi-linux"

install_fixture() {
  local source="$1"
  rm -rf "$PACKAGE_ROOT"
  mkdir -p "$PACKAGE_ROOT"
  cp -a "$source/." "$PACKAGE_ROOT/"
  printf '%s\n' "$(basename "$source")" >"$STATE_ROOT/package-manifest"
}

upgrade_fixture() {
  local source="$1"
  rm -rf "$BACKUP"
  cp -a "$PACKAGE_ROOT" "$BACKUP"
  local previous_manifest
  previous_manifest="$(cat "$STATE_ROOT/package-manifest")"
  rm -rf "$PACKAGE_ROOT"
  mkdir -p "$PACKAGE_ROOT"
  cp -a "$source/." "$PACKAGE_ROOT/"
  if [[ -f "$PACKAGE_ROOT/migration.fail" ]]; then
    rm -rf "$PACKAGE_ROOT"
    cp -a "$BACKUP" "$PACKAGE_ROOT"
    printf '%s\n' "$previous_manifest" >"$STATE_ROOT/package-manifest"
    return 1
  fi
  rm -f "$PACKAGE_ROOT/migration.fail"
  printf '%s\n' "$(basename "$source")" >"$STATE_ROOT/package-manifest"
}

rollback_fixture() {
  [[ -d "$BACKUP" ]] || fail "rollback backup is missing"
  rm -rf "$PACKAGE_ROOT"
  cp -a "$BACKUP" "$PACKAGE_ROOT"
  printf '%s\n' "pkg-v1" >"$STATE_ROOT/package-manifest"
}

uninstall_fixture() {
  rm -rf "$PACKAGE_ROOT/usr"
  rm -f "$STATE_ROOT/package-manifest"
}

install_fixture "$V1"
[[ "$(cat "$STATE_ROOT/package-manifest")" == "pkg-v1" ]] || fail "initial manifest"
[[ "$(cat "$PACKAGE_ROOT/usr/lib/systemd/user/yunxi-linux.service")" == "v1" ]] \
  || fail "initial unit"
[[ "$(stat -c '%a' "$PACKAGE_ROOT/usr/bin/yunxi-linux")" == "755" ]] \
  || fail "initial binary mode"

if upgrade_fixture "$V2"; then
  fail "migration failure was accepted"
fi
[[ "$(cat "$STATE_ROOT/package-manifest")" == "pkg-v1" ]] \
  || fail "failed upgrade changed manifest"
[[ "$(cat "$PACKAGE_ROOT/usr/bin/yunxi-linux")" == *"v1"* ]] \
  || fail "failed upgrade changed binary"

rm -f "$V2/migration.fail"
upgrade_fixture "$V2"
[[ "$(cat "$STATE_ROOT/package-manifest")" == "pkg-v2" ]] \
  || fail "successful upgrade did not publish manifest"
[[ "$(cat "$PACKAGE_ROOT/usr/lib/systemd/user/yunxi-linux.service")" == "v2" ]] \
  || fail "successful upgrade did not publish unit"

rollback_fixture
[[ "$(cat "$STATE_ROOT/package-manifest")" == "pkg-v1" ]] || fail "rollback manifest"
[[ "$(cat "$PACKAGE_ROOT/usr/bin/yunxi-linux")" == *"v1"* ]] || fail "rollback binary"

uninstall_fixture
[[ ! -e "$PACKAGE_ROOT/usr/bin/yunxi-linux" ]] || fail "package binary survived uninstall"
[[ ! -e "$PACKAGE_ROOT/usr/lib/systemd/user/yunxi-linux.service" ]] \
  || fail "package unit survived uninstall"
[[ -f "$DATA_ROOT/.yunxi/memory/long-term-vectors.sqlite3" ]] \
  || fail "uninstall removed long-term memory"
[[ -f "$DATA_ROOT/.yunxi/knowledge.sqlite3" ]] || fail "uninstall removed knowledge db"
[[ -f "$STATE_ROOT/scheduler.json" ]] || fail "uninstall removed scheduler state"
[[ -f "$FISH_HOOK" ]] || fail "uninstall removed fish hook"
[[ ! -e "$STATE_ROOT/package-manifest" ]] || fail "package manifest survived uninstall"
[[ ! -e "$TMP_ROOT/systemd-enabled" ]] || fail "smoke implicitly enabled a service"

printf 'lifecycle-smoke=ok\n'
