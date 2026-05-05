#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENTS_DIR="$(cd "$APP_DIR/.." && pwd)"
DEVICE_NAME="${IOS_SIMULATOR_NAME:-iPhone 17}"

if [ "${1:-}" != "" ] && [[ "${1:-}" != --* ]]; then
  DEVICE_NAME="$1"
  shift
fi

if ! command -v xcrun >/dev/null 2>&1; then
  printf 'error: xcrun is required for iOS simulator boot management\n' >&2
  exit 1
fi

SIMULATOR_ID="$(
  xcrun simctl list devices available | awk -v name="$DEVICE_NAME" '
    index($0, name " (") {
      if (match($0, /\([0-9A-F-]{36}\)/)) {
        print substr($0, RSTART + 1, RLENGTH - 2)
        exit
      }
    }
  '
)"

if [ -z "$SIMULATOR_ID" ]; then
  printf 'error: iOS simulator not found: %s\n\n' "$DEVICE_NAME" >&2
  xcrun simctl list devices available >&2
  exit 1
fi

if xcrun simctl list devices available | grep "$SIMULATOR_ID" | grep -q "(Booted)"; then
  printf 'ok: simulator already booted: %s (%s)\n' "$DEVICE_NAME" "$SIMULATOR_ID"
else
  printf 'booting simulator: %s (%s)\n' "$DEVICE_NAME" "$SIMULATOR_ID"
  xcrun simctl boot "$SIMULATOR_ID" 2>/dev/null || true
fi

xcrun simctl bootstatus "$SIMULATOR_ID" -b

cd "$CLIENTS_DIR"
exec cargo tauri ios dev "$DEVICE_NAME" --features="${TAURI_IOS_FEATURES:-native}" "$@"
