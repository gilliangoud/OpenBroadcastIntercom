#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENTS_DIR="$(cd "$APP_DIR/.." && pwd)"
BUNDLE_IDENTIFIER="${IOS_BUNDLE_IDENTIFIER:-com.intercomsuite.client}"

install_xcodebuild_registration_shim() {
  if [ "${IOS_ALLOW_DEVICE_REGISTRATION:-1}" = "0" ]; then
    return
  fi

  local real_xcodebuild shim_dir
  real_xcodebuild="$(xcrun -f xcodebuild 2>/dev/null || command -v xcodebuild)"
  shim_dir="$(mktemp -d "${TMPDIR:-/tmp}/intercom-xcodebuild.XXXXXX")"

  cat > "$shim_dir/xcodebuild" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail

real_xcodebuild="${INTERCOM_REAL_XCODEBUILD:-/usr/bin/xcodebuild}"
has_provisioning_updates=0
has_device_registration=0

for arg in "$@"; do
  case "$arg" in
    -allowProvisioningUpdates)
      has_provisioning_updates=1
      ;;
    -allowProvisioningDeviceRegistration)
      has_device_registration=1
      ;;
  esac
done

if [ "$has_provisioning_updates" = "1" ] && [ "$has_device_registration" = "0" ]; then
  exec "$real_xcodebuild" -allowProvisioningDeviceRegistration "$@"
fi

exec "$real_xcodebuild" "$@"
SHIM
  chmod +x "$shim_dir/xcodebuild"

  export INTERCOM_REAL_XCODEBUILD="$real_xcodebuild"
  export PATH="$shim_dir:$PATH"
}

print_signing_help() {
  cat >&2 <<EOF

error: iOS device signing is not ready for $BUNDLE_IDENTIFIER

Xcode could not create or find an iOS App Development provisioning profile for
this bundle identifier and device. Physical iPhone builds must be signed.

Fix:
  1. Open Xcode > Settings > Accounts and confirm the Apple ID for team $APPLE_DEVELOPMENT_TEAM is signed in.
  2. Confirm the account can manage devices/profiles for that team.
  3. Make sure the target iPhone is registered with that development team.
  4. Re-run:
       APPLE_DEVELOPMENT_TEAM=$APPLE_DEVELOPMENT_TEAM ./app/scripts/ios-device-build.sh

If Xcode later says the bundle identifier is unavailable, change the app
identifier to a unique development value before rerunning.
EOF
}

run_tauri_ios_build() {
  local log_file
  log_file="$(mktemp "${TMPDIR:-/tmp}/intercom-ios-device-build.XXXXXX.log")"

  set +e
  cargo tauri ios build --debug --target=aarch64 --features="${TAURI_IOS_FEATURES:-native}" --ci "$@" 2>&1 | tee "$log_file"
  local status="${PIPESTATUS[0]}"
  set -e

if [ "$status" -ne 0 ]; then
    if grep -Eq "No Accounts|No profiles|provisioning profiles|provisioning profile|requires a provisioning profile|No signing certificate|Signing for|isn't registered|not registered" "$log_file"; then
      print_signing_help
    fi
    exit "$status"
  fi
}

if [ -z "${APPLE_DEVELOPMENT_TEAM:-}" ]; then
  printf 'error: APPLE_DEVELOPMENT_TEAM is required for a physical iOS device build\n' >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx 'aarch64-apple-ios'; then
  printf 'error: missing Rust target aarch64-apple-ios\n' >&2
  printf 'run: rustup target add aarch64-apple-ios\n' >&2
  exit 1
fi

export DEVELOPMENT_TEAM="$APPLE_DEVELOPMENT_TEAM"
export CODE_SIGN_STYLE="${CODE_SIGN_STYLE:-Automatic}"

"$APP_DIR/scripts/ios-clean-build-output.sh"
install_xcodebuild_registration_shim

cd "$CLIENTS_DIR"
run_tauri_ios_build "$@"

APP_PATH="$(
  find "$APP_DIR/gen/apple/build" -path '*/.stale/*' -prune -o -type d -name '*.app' -print |
    sort |
    tail -n 1
)"

if [ -z "$APP_PATH" ]; then
  printf 'error: built iOS .app was not found under %s\n' "$APP_DIR/gen/apple/build" >&2
  exit 1
fi

printf '%s\n' "$APP_PATH"
