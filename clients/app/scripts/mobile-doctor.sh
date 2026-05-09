#!/usr/bin/env bash
set -u

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
NDK_HOME="${NDK_HOME:-$ANDROID_HOME/ndk/29.0.14206865}"
ANDROID_NDK="${ANDROID_NDK:-$NDK_HOME}"
CMAKE_MAKE_PROGRAM="${CMAKE_MAKE_PROGRAM:-/usr/bin/make}"

ok() { printf 'ok: %s\n' "$1"; }
warn() { printf 'warn: %s\n' "$1"; }

check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    ok "$1 found at $(command -v "$1")"
  else
    warn "$1 is not on PATH"
  fi
}

check_target() {
  if rustup target list --installed | grep -qx "$1"; then
    ok "Rust target installed: $1"
  else
    warn "Rust target missing: $1"
  fi
}

version_ge() {
  local current="$1"
  local required="$2"
  local current_major current_minor current_patch
  local required_major required_minor required_patch

  IFS=. read -r current_major current_minor current_patch <<EOF
$current
EOF
  IFS=. read -r required_major required_minor required_patch <<EOF
$required
EOF

  current_major="${current_major:-0}"
  current_minor="${current_minor:-0}"
  current_patch="${current_patch:-0}"
  required_major="${required_major:-0}"
  required_minor="${required_minor:-0}"
  required_patch="${required_patch:-0}"

  [ "$current_major" -gt "$required_major" ] && return 0
  [ "$current_major" -lt "$required_major" ] && return 1
  [ "$current_minor" -gt "$required_minor" ] && return 0
  [ "$current_minor" -lt "$required_minor" ] && return 1
  [ "$current_patch" -ge "$required_patch" ]
}

check_tauri_cli_version() {
  local version

  if ! command -v cargo-tauri >/dev/null 2>&1; then
    return
  fi

  version="$(cargo tauri --version 2>/dev/null | awk '{print $2}')"
  if [ -z "$version" ]; then
    warn "could not determine cargo-tauri version"
    return
  fi

  if version_ge "$version" "2.11.0"; then
    ok "cargo-tauri version $version"
  else
    warn "cargo-tauri $version is old; use 2.11.0+ for Xcode 26 iOS simulator builds"
  fi
}

printf 'RedLine mobile doctor\n'
printf 'app: %s\n' "$APP_DIR"
printf 'ANDROID_HOME=%s\n' "$ANDROID_HOME"
printf 'NDK_HOME=%s\n' "$NDK_HOME"
printf 'ANDROID_NDK=%s\n' "$ANDROID_NDK"
printf 'CMAKE_MAKE_PROGRAM=%s\n\n' "$CMAKE_MAKE_PROGRAM"

check_cmd cargo
check_cmd cargo-tauri
check_tauri_cli_version
check_cmd xcodegen
check_cmd idevicesyslog
check_cmd pod
check_cmd sdkmanager
check_cmd adb

printf '\n'
for target in \
  aarch64-apple-ios \
  aarch64-apple-ios-sim \
  aarch64-linux-android \
  armv7-linux-androideabi \
  i686-linux-android \
  x86_64-linux-android
do
  check_target "$target"
done

printf '\n'
if [ -d "$ANDROID_HOME" ]; then ok "Android SDK root exists"; else warn "Android SDK root missing"; fi
if [ -d "$NDK_HOME" ]; then ok "Android NDK exists"; else warn "Android NDK missing"; fi
if [ -f "$APP_DIR/gen/android/app/src/main/AndroidManifest.xml" ]; then ok "Tauri Android project generated"; else warn "Tauri Android project missing"; fi
if [ -f "$APP_DIR/gen/apple/project.yml" ]; then ok "Tauri iOS project generated"; else warn "Tauri iOS project missing"; fi
if [ -x "$APP_DIR/scripts/ios-dev.sh" ]; then ok "iOS dev wrapper is executable"; else warn "iOS dev wrapper is not executable"; fi
if [ -x "$APP_DIR/scripts/ios-clean-build-output.sh" ]; then ok "iOS build cleanup script is executable"; else warn "iOS build cleanup script is not executable"; fi
if [ -x "$APP_DIR/scripts/ios-build-sim.sh" ]; then ok "iOS simulator build wrapper is executable"; else warn "iOS simulator build wrapper is not executable"; fi
if [ -x "$APP_DIR/scripts/ios-device-build.sh" ]; then ok "iOS device build wrapper is executable"; else warn "iOS device build wrapper is not executable"; fi
if [ -x "$APP_DIR/scripts/ios-device-dev.sh" ]; then ok "iOS device dev wrapper is executable"; else warn "iOS device dev wrapper is not executable"; fi
if [ -n "${APPLE_DEVELOPMENT_TEAM:-}" ]; then ok "APPLE_DEVELOPMENT_TEAM is set"; else warn "APPLE_DEVELOPMENT_TEAM is not set for physical iOS device builds"; fi

cat <<EOF

Useful commands:
  export ANDROID_HOME="$ANDROID_HOME"
  export NDK_HOME="$NDK_HOME"
  export ANDROID_NDK="$ANDROID_NDK"
  export ANDROID_NDK_HOME="$NDK_HOME"
  export ANDROID_NDK_ROOT="$NDK_HOME"
  export CMAKE_MAKE_PROGRAM="$CMAKE_MAKE_PROGRAM"
  cargo tauri android dev
  clients/app/scripts/ios-dev.sh
  clients/app/scripts/ios-build-sim.sh
  APPLE_DEVELOPMENT_TEAM=TEAMID clients/app/scripts/ios-device-build.sh
  APPLE_DEVELOPMENT_TEAM=TEAMID clients/app/scripts/ios-device-dev.sh

EOF
