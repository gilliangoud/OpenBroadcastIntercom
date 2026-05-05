#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENTS_DIR="$(cd "$APP_DIR/.." && pwd)"

"$APP_DIR/scripts/ios-clean-build-output.sh"

cd "$CLIENTS_DIR"
exec cargo tauri ios build --debug --target=aarch64-sim --features="${TAURI_IOS_FEATURES:-native}" --ci "$@"
