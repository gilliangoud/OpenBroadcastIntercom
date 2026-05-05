# Release Automation

Every non-release push to `main` runs the release workflow. The workflow computes
the next SemVer-compatible CalVer version (`YYYY.M.counter`), commits the version
updates as `chore(release): vYYYY.M.N [skip release]`, tags that commit, builds
unsigned downloadable artifacts, and publishes a GitHub Release.

## Version Source Of Truth

- `Cargo.toml` `[workspace.package].version`
- Tauri app `version` fields
- Android `bundle.android.versionCode`

Use these checks locally:

```sh
tools/check-version-sync.sh
python3 -m unittest tools/test_release_version.py
```

## First-Pass Artifacts

- macOS: Intercom Suite, Intercom Bridge App, and Intercom Server `.app` zip
  archives.
- Windows: Intercom Suite and Intercom Bridge App NSIS installers.
- Linux: Intercom Suite and Intercom Bridge App AppImage/deb bundles.
- Android: Intercom Suite debug/sideload APK.

iOS remains a CI compile check only until Apple signing and provisioning secrets
are intentionally added.
