# Public Repository Hygiene

Use a fresh Git history for the public repository. This working tree previously
contained local state, generated native build output, and very large build
caches, so the old `.git` object database must not be pushed.

## Keep

- Rust/C source, Cargo manifests, docs, scripts, schemas, app icons, and Tauri
  metadata needed to regenerate mobile projects.
- External Whisper model metadata in `intercom-models/manifest.json`.
- Curated non-Whisper Git LFS model assets listed in `THIRD_PARTY_NOTICES.md`.
- Sanitized example config files.

## Exclude

- `target/`, Tauri `build/`, Apple `Externals/`, Android Gradle build output,
  ESP32 build output, `.xcarchive`, `.ipa`, `.apk`, debug audio, `.DS_Store`,
  Finder duplicates, and local runtime state JSON.
- Apple signing assets, provisioning profiles, certificates, private keys, and
  hard-coded team identifiers.
- Downloaded Whisper `.bin` files and uncurated large model files.

## Pre-Publish Checklist

```sh
cargo fmt --all -- --check
cargo test -p client-core
cargo test -p desktop
cargo test -p app
cargo test -p server --no-default-features
tools/check-version-sync.sh
python3 -m unittest tools/test_release_version.py
python3 tools/check-model-manifest.py
tools/check-generated-artifacts.sh
tools/check-public-secrets.sh
git status --short
git ls-files
```
