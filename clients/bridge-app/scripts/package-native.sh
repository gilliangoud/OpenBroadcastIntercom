#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ $# -eq 0 ]]; then
  case "$(uname -s)" in
    Darwin)
      set -- --bundles app
      ;;
    Linux)
      set -- --bundles appimage deb
      ;;
    MINGW*|MSYS*|CYGWIN*)
      set -- --bundles nsis
      ;;
  esac
fi

echo "Building RedLine Bridge native release binary..."
cargo build --features native --bin bridge-app-native --release

if cargo tauri --version >/dev/null 2>&1; then
  echo "Building Tauri bundles with clients/bridge-app/tauri.conf.json..."
  cargo tauri build --features native "$@"
else
  cat <<'MSG'
Tauri CLI is not installed, so installer bundles were not created.

The release binary was built successfully. Install the Tauri v2 CLI to produce
platform bundles:

  cargo install tauri-cli --version '^2'
  clients/bridge-app/scripts/package-native.sh

MSG
fi
