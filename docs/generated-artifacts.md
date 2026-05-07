# Generated Artifact Policy

The source of truth for the Tauri mobile app lives in `clients/app`: Rust
sources, `tauri.conf.json`, `tauri.ios.conf.json`, `Info.ios.plist`, app icons,
`tauri-assets`, and scripts. Generated Apple and Android projects under
`clients/app/gen` are allowed only where Tauri needs checked-in native project
metadata.

Build products are not source. Do not commit Tauri Apple build folders,
`Externals` static libraries, `.xcarchive`, `.ipa`, Android Gradle build
outputs, ESP32 build output, local state JSON, debug audio, `.DS_Store`, or
Finder duplicate files.

Whisper `.bin` model files are local downloads described by
`intercom-models/manifest.json`; do not commit them. Large approved
non-Whisper model files are source only when they are part of the curated Git
LFS set documented in `THIRD_PARTY_NOTICES.md`. Do not commit unreviewed model
weights or model archives.

Use this check before committing broad mobile/build changes:

```sh
tools/check-generated-artifacts.sh
tools/check-public-secrets.sh
```

If the script reports tracked generated output, remove it from the index and
regenerate it locally when needed.
