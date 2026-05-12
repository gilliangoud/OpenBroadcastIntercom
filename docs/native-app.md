# Native App

[Docs index](README.md) | [Root README](../README.md)

The native client is the `app-native` binary in `clients/app`. It uses Tauri 2
for the OS window, tray/menu integration, settings window, and bundle metadata.
The audio/control runtime is still the existing Rust desktop runtime.

## Development

Run the server first:

```sh
cargo run -p server
```

Run one native client:

```sh
cargo run -p app --features native --bin app-native -- --user-id 1
```

The app package uses `app-native` as Cargo `default-run` because the Tauri CLI
requires a default binary for bundling. Running `cargo run -p app` without the
`native` feature still delegates to the browser app host behavior.

The same app package also contains the iOS/Android Tauri 2 mobile shell. Mobile
builds export a Tauri mobile entry point from the `app` library, show a compact
setup page, start the same Rust client runtime, and then open the same
phone-shaped local client UI used by the desktop runtime. See
[Mobile Tauri Clients](mobile-clients.md) for toolchain setup, permissions, and
mobile validation notes.

Run a second local client by changing the user ID. The app picks another local
UI port if the requested one is already in use:

```sh
cargo run -p app --features native --bin app-native -- --user-id 2
```

The production-audio bridge has its own Tauri desktop wrapper. Use it on a
vMix, PA, recorder, or venue-audio machine when several bridge routes should be
managed from one desktop app:

```sh
cargo run -p bridge-app --features native --bin bridge-app-native
```

The bridge desktop app opens the multi-route bridge manager in a Tauri window,
persists `intercom-bridge-app.json`, and launches one `bridge` process per
route. The default filename is retained for compatibility with existing bridge
configs. Closing the native window stops those launched route processes.
Build its release binary or platform bundle with:

```sh
cargo build -p bridge-app --features native --bin bridge-app-native --release
clients/bridge-app/scripts/package-native.sh
```

For a non-GUI launch smoke test, print the launch plan. The native binary
defaults to `window_mode = "native"` and should pick an available local UI port:

```sh
cargo run -p app --features native --bin app-native -- --print-launch-plan --local-ui-bind 127.0.0.1:0
```

Current visual references for the native app settings, Tauri operator console,
desktop local UI, Pi browser UI, mobile setup, and bridge app are collected in
[Client UI Screenshots](client-ui-screenshots.md).

## Native Controls

The tray/menu exposes:

- `Open Operator Window`
- `App Settings`
- `Refresh Status`
- `Mute`
- `Unmute`
- `Start Talk`
- `Stop Talk`
- `Quit`

These controls call the existing local HTTP API where possible. If
`--local-ui-token` is configured, the tray sends it as a bearer token.

## Settings Window

`App Settings` opens a Tauri asset window backed by `intercom-app-settings.json`
or the path supplied with `--config-file`. The default filename is retained for
compatibility with existing app-host configs.

The settings window can edit:

- server/control addresses
- user ID
- startup TX/listen channels
- advertised buttons and focused hotkeys
- codec, gains, jitter, input backend, and audio device match strings
- local UI bind/token/window mode

Saved startup settings apply the next time the native client starts. Live route,
talk, codec, gain, and button control still happens through the operator UI and
server-authoritative control plane.

On macOS, the `auto` and `voice-processing` input backends try Apple's
VoiceProcessingIO for the default microphone before falling back to the raw
portable capture path. Choose `raw` when testing hardware inputs or when a
specific `input_device` match is required. The client Stats modal shows both the
requested backend and the active backend, including any fallback note.

Apple's Voice Isolation mode is not exposed as a public programmatic setter.
AVFoundation exposes `preferredMicrophoneMode` and `activeMicrophoneMode` as
read-only state selected by the user in Control Center, plus a system UI method
for opening the microphone-mode selector. The native client therefore reports
the active/preferred macOS microphone mode in Stats and provides `Open Controls`
to deep-link to that selector. Use that to choose Voice Isolation; the app can
verify the result, but it cannot silently force the Control Center setting.

The server-owned `processing` config controls the capture cleanup profile used
by the native client and the server backstop DSP. `engine = "built_in"` uses the
server's lightweight real-time chain, `engine = "webrtc"` runs the bundled
WebRTC Audio Processing Module in the server binary, and `engine = "rnnoise"`
runs server-side RNNoise on decoded 48 kHz frames. The `pipeline` array can run
multiple stages in order, for example `webrtc -> built_in` for laptop mic
cleanup with a light final gate/compressor. `engine = "deepfilternet"` runs
compatible ONNX `.tar.gz` models through the server's bundled Rust/Tract worker
backend, or complete Core ML package directories through Apple Core ML when the
server is built for macOS with `processing-deepfilternet-coreml`. It reports
fallback status if the selected model cannot load or cannot keep up. Its
`deep_filter_backend` and `apple_compute_units` fields choose the backend and
Apple compute target; ONNX archives use Tract, while Core ML package directories
use Core ML on supported macOS builds. A macOS server can be built with
`--features macos-accelerated` to include WebRTC, RNNoise, DeepFilterNet, Core
ML package inference, and whisper.cpp Metal support for built-in transcription.
The macOS Server Tauri app enables that feature set through its `native` build
feature.
Use `voice_isolation` for laptop microphones and keyboard-heavy
environments, `voice` for normal RedLine use, `broadcast` for less aggressive
gating, and `raw` for external audio interfaces. When `native_voice_processing`
is enabled, macOS clients use VoiceProcessingIO where possible; selecting a
specific input device still forces the raw portable backend.

## Runtime Lifecycle

The Tauri process starts the desktop runtime on a background Rust thread. When
the Tauri app exits, it sends an explicit shutdown signal to that runtime before
joining the thread. This keeps native quit behavior cleaner than abruptly
dropping the process.

## Packaging

Build the release binary:

```sh
cargo build -p app --features native --bin app-native --release
```

Build platform bundles from the app crate when the Tauri CLI is installed:

```sh
cargo install tauri-cli --version '^2'
clients/app/scripts/package-native.sh
```

With no arguments, the helper picks a conservative host default:

- macOS: `--bundles app`
- Linux: `--bundles appimage deb`
- Windows: `--bundles nsis`

On this macOS development machine, the `.app` bundle target is verified with:

```sh
cargo build -p app --features native --bin app-native --release
clients/app/scripts/package-native.sh --bundles app
```

The verified `.app` output is written to
`target/release/bundle/macos/RedLine.app`.
The `.dmg` target reaches Tauri's generated DMG script here but still needs a
proper local macOS packaging environment pass before we should treat it as
release-ready. In this workspace, `hdiutil` currently fails with
`Device not configured` while creating the disk image.

Current local verification for the native wrapper is:

```sh
cargo test -p app --features native
cargo clippy -p app --features native --all-targets -- -D warnings
cargo run -p app --features native --bin app-native -- --print-launch-plan --local-ui-bind 127.0.0.1:0
cargo build -p app --features native --bin app-native --release
clients/app/scripts/package-native.sh --bundles app
```

Mobile project smoke checks:

```sh
clients/app/scripts/mobile-doctor.sh
cd clients
ANDROID_HOME=/opt/homebrew/share/android-commandlinetools NDK_HOME=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 cargo tauri android init --ci --skip-targets-install
cargo tauri ios init --ci --skip-targets-install
./app/scripts/ios-dev.sh
./app/scripts/ios-build-sim.sh
APPLE_DEVELOPMENT_TEAM=TEAMID ./app/scripts/ios-device-build.sh
APPLE_DEVELOPMENT_TEAM=TEAMID ./app/scripts/ios-device-dev.sh
```

Use `ios-device-dev.sh` for the first physical iPhone run. It targets a
connected trusted iPhone/iPad/iPod directly; `ios-device-build.sh` creates a
generic signed device bundle and expects provisioning to already know the
device. The dev wrapper injects Xcode's
`-allowProvisioningDeviceRegistration` flag so Xcode can add a trusted
destination device to the development team when the account has permission.

For `No Accounts` or missing provisioning profile errors, open Xcode > Settings
> Accounts, add the Apple ID for `APPLE_DEVELOPMENT_TEAM`, and create/download
an Apple Development certificate before rerunning the wrapper.

The Tauri bundle config lives at `clients/app/tauri.conf.json`. It currently
targets all supported bundle formats for the host OS and uses the branded icon
assets in `clients/app/icons`.

Expected outputs depend on the host OS:

- macOS: `.app` and `.dmg`
- Windows: NSIS `.exe` and/or MSI
- Linux: `.AppImage`, `.deb`, and/or `.rpm`

Tauri can only produce the platform bundles supported by the current host and
installed system dependencies. Build each platform on that platform or in a CI
runner prepared for Tauri packaging.

## Branding

The source icon is `clients/app/icons/icon.svg`. Generated bundle icons are
stored alongside it. Replace the SVG and regenerate `icon.png`/`icon.ico` when
final branding is ready.
