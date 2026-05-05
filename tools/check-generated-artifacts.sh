#!/usr/bin/env sh
set -eu

tracked="$(
  git ls-files \
    'clients/app/gen/apple/build/**' \
    'clients/app/gen/apple/Externals/**' \
    'clients/app/gen/apple/**/*.xcarchive' \
    'clients/app/gen/apple/**/*.ipa' \
    'clients/app/gen/android/**/build/**' \
    'clients/app/gen/android/.gradle/**' \
    'clients/app/gen/android/buildSrc/.gradle/**' \
    'clients/esp32/build/**' \
    'clients/esp32/managed_components/**' \
    'clients/esp32/sdkconfig' \
    'clients/esp32/sdkconfig.*' \
    'clients/esp32/sdkconfig [0-9]*' \
    'debug-audio/**' \
    'intercom-state.json' \
    'intercom-app-settings.json' \
    'artifacts/**' \
    '*.AppImage' \
    '*.deb' \
    '*.rpm' \
    '*.dmg' \
    '*.msi' \
    '*.apk' \
    '*.aab' \
    '*.ipa' \
    'intercom-models/ggml-medium-*.bin' \
    'intercom-models/ggml-large-*.bin' \
    '**/.DS_Store' \
    '**/* [0-9]' \
    '**/* [0-9].*' || true
)"

if [ -n "$tracked" ]; then
  printf '%s\n' "tracked generated/build artifacts found:" >&2
  printf '%s\n' "$tracked" >&2
  exit 1
fi
