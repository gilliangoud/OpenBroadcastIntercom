#!/usr/bin/env sh
set -eu

fail=0

report() {
  fail=1
  printf '%s\n' "$1" >&2
}

if git ls-files | grep -E '(^|/)(intercom-state\.json|intercom-app-settings\.json)$' >/dev/null; then
  report "tracked local runtime state/config JSON found"
fi

if git ls-files | grep -E '(^|/)(debug-audio|target)/' >/dev/null; then
  report "tracked debug audio or target build output found"
fi

if git ls-files | grep -E '\.(mobileprovision|provisionprofile|p12|pem|key|cer|der|certSigningRequest|csr)$' >/dev/null; then
  report "tracked signing, certificate, or private key material found"
fi

if git ls-files \
  | grep -E '(^|/)clients/esp32/sdkconfig($|[ .])' \
  | grep -v '^clients/esp32/sdkconfig\.defaults$' >/dev/null; then
  report "tracked ESP32 sdkconfig file found; only sdkconfig.defaults is public-safe"
fi

matches="$(
  rg -n --hidden \
    -g '!.git/**' \
    -g '!.git.local-prepublish-backup/**' \
    -g '!target/**' \
    -g '!tools/check-public-secrets.sh' \
    -g '!intercom-models/*.bin' \
    -g '!server/assets/supertonic/**/*.onnx' \
    -g '!deepfilternet-models/*.tar.gz' \
    -e 'CONFIG_INTERCOM_WIFI_SSID="[^"]+"' \
    -e 'CONFIG_INTERCOM_WIFI_PASSWORD="[^"]+"' \
    -e 'APPLE_DEVELOPMENT_TEAM=[A-Z0-9]{10}' \
    -e 'DEVELOPMENT_TEAM = "[A-Z0-9]{10}"' \
    -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' \
    -e 'AKIA[0-9A-Z]{16}' \
    -e 'AIza[0-9A-Za-z_-]{35}' \
    -e 'gh[pousr]_[0-9A-Za-z_]{20,}' \
    -e 'xox[baprs]-[0-9A-Za-z-]+' \
    -e 'Authorization: Bearer [A-Za-z0-9._-]{16,}' \
    . || true
)"

if [ -n "$matches" ]; then
  report "private identifiers or known leaked local values found:"
  printf '%s\n' "$matches" >&2
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
