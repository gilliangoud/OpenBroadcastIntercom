#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="$APP_DIR/gen/apple/build"

if [ ! -d "$BUILD_DIR" ]; then
  exit 0
fi

STALE_DIR="$BUILD_DIR/.stale"
STAMP="$(date +%Y%m%d%H%M%S)-$$"
mkdir -p "$STALE_DIR"

find "$BUILD_DIR" -mindepth 1 -maxdepth 1 \
  \( -name '*.xcarchive' -o -name '*.ipa' -o -name 'arm64*' -o -name 'x86_64*' \) \
  -print0 |
  while IFS= read -r -d '' path; do
    mv "$path" "$STALE_DIR/$(basename "$path").$STAMP"
  done
