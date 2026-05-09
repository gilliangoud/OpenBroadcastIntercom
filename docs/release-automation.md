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

- macOS: RedLine, RedLine Bridge, and RedLine Server `.app` zip
  archives.
- Windows: RedLine and RedLine Bridge NSIS installers.
- Linux: RedLine and RedLine Bridge AppImage/deb bundles.
- Android: RedLine debug/sideload APK.
- iOS: RedLine simulator `.app` zip. When Apple signing secrets are
  configured, the workflow also publishes a signed device `.ipa` for
  provisioned iPhone/iPad installs.

## iOS GitHub Release Signing

GitHub-hosted macOS runners cannot create a physical-device iOS install without
Apple signing material. Add these repository secrets before expecting a
device-installable IPA on GitHub releases:

- `APPLE_DEVELOPMENT_TEAM`
- `IOS_CERTIFICATE_P12_BASE64`
- `IOS_CERTIFICATE_PASSWORD`
- `IOS_PROVISIONING_PROFILE_BASE64`

`IOS_CERTIFICATE_P12_BASE64` is a base64-encoded `.p12` signing certificate.
`IOS_PROVISIONING_PROFILE_BASE64` is a base64-encoded provisioning profile for
`com.intercomsuite.client`. Until those secrets exist, the release still
publishes an iOS simulator bundle and an explanatory signing note.
