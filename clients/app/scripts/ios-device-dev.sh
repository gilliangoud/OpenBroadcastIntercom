#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENTS_DIR="$(cd "$APP_DIR/.." && pwd)"
DEVICE_ID="${IOS_DEVICE_ID:-}"
DEVICE_NAME="${IOS_DEVICE_NAME:-}"
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
this bundle identifier and device. Physical iPhone installs must be signed.

Fix:
  1. Keep $DEVICE_NAME connected and trusted.
  2. Open Xcode > Settings > Accounts and confirm the Apple ID for team $APPLE_DEVELOPMENT_TEAM is signed in.
  3. Confirm the account can manage devices/profiles for that team.
  4. Re-run:
       APPLE_DEVELOPMENT_TEAM=$APPLE_DEVELOPMENT_TEAM ./app/scripts/ios-device-dev.sh

This wrapper injects -allowProvisioningDeviceRegistration for physical-device
dev runs. If Xcode still says the device is not registered, manually add this
UDID in the Apple Developer portal:
  $DEVICE_ID

If Xcode says the bundle identifier is unavailable, change the app identifier
to a unique development value before rerunning.
EOF
}

run_tauri_ios_dev() {
  local log_file
  log_file="$(mktemp "${TMPDIR:-/tmp}/intercom-ios-device-dev.XXXXXX")"

  set +e
  cargo tauri ios dev "$DEVICE_NAME" --features="${TAURI_IOS_FEATURES:-native}" "$@" 2>&1 | tee "$log_file"
  local status="${PIPESTATUS[0]}"
  set -e

if [ "$status" -ne 0 ]; then
    if grep -Eq "No Accounts|No profiles|provisioning profiles|provisioning profile|requires a provisioning profile|No signing certificate|Signing for|isn't registered|not registered" "$log_file"; then
      print_signing_help
    fi
    exit "$status"
  fi
}

if ! command -v xcrun >/dev/null 2>&1; then
  printf 'error: xcrun is required for physical iOS device detection\n' >&2
  exit 1
fi

if [ -z "${APPLE_DEVELOPMENT_TEAM:-}" ]; then
  printf 'error: APPLE_DEVELOPMENT_TEAM is required for a physical iOS device run\n' >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx 'aarch64-apple-ios'; then
  printf 'error: missing Rust target aarch64-apple-ios\n' >&2
  printf 'run: rustup target add aarch64-apple-ios\n' >&2
  exit 1
fi

list_ios_devices() {
  xcrun xctrace list devices 2>/dev/null |
    awk '
      /^== Devices ==/ { in_devices = 1; next }
      /^== / { in_devices = 0; next }
      !in_devices { next }
      /Mac/ { next }
      !/(iPhone|iPad|iPod)/ { next }
      match($0, /\([0-9A-Fa-f-]{24,}\)[[:space:]]*$/) {
        id = substr($0, RSTART + 1, RLENGTH - 2)
        name = substr($0, 1, RSTART - 1)
        sub(/[[:space:]]*\([^)]*\)[[:space:]]*$/, "", name)
        print id "\t" name
      }
    '
}

DEVICE_ROWS="$(list_ios_devices)"

if [ -n "$DEVICE_ID" ] && [ -z "$DEVICE_NAME" ]; then
  DEVICE_NAME="$(
    printf '%s\n' "$DEVICE_ROWS" |
      awk -F '\t' -v wanted="$DEVICE_ID" '$1 == wanted { print $2; exit }'
  )"
fi

if [ -n "$DEVICE_NAME" ] && [ -z "$DEVICE_ID" ]; then
  DEVICE_ID="$(
    printf '%s\n' "$DEVICE_ROWS" |
      awk -F '\t' -v wanted="$DEVICE_NAME" '$2 == wanted { print $1; exit }'
  )"
fi

if [ -z "$DEVICE_ID" ] || [ -z "$DEVICE_NAME" ]; then
  FIRST_DEVICE="$(printf '%s\n' "$DEVICE_ROWS" | awk 'NF { print; exit }')"
  if [ -n "$FIRST_DEVICE" ]; then
    DEVICE_ID="${FIRST_DEVICE%%	*}"
    DEVICE_NAME="${FIRST_DEVICE#*	}"
  fi
fi

if [ -z "$DEVICE_ID" ] || [ -z "$DEVICE_NAME" ]; then
  printf 'error: no connected physical iPhone/iPad/iPod found\n' >&2
  printf 'connect and trust the device, then check that it appears under "== Devices ==" in:\n' >&2
  printf '  xcrun xctrace list devices\n\n' >&2
  xcrun xctrace list devices >&2 || true
  exit 1
fi

export DEVELOPMENT_TEAM="$APPLE_DEVELOPMENT_TEAM"
export CODE_SIGN_STYLE="${CODE_SIGN_STYLE:-Automatic}"

"$APP_DIR/scripts/ios-clean-build-output.sh"
install_xcodebuild_registration_shim

printf 'running on iOS device: %s (%s)\n' "$DEVICE_NAME" "$DEVICE_ID"

cd "$CLIENTS_DIR"
run_tauri_ios_dev "$@"
