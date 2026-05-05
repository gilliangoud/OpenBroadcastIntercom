# Mobile Tauri Clients

`clients/app` contains the first iOS and Android Tauri 2 mobile shell for the
Intercom Suite client. The mobile app reuses the same Rust client runtime,
control protocol, UDP audio transport, codecs, local UI, and server-owned
configuration model as the desktop app.

## Runtime Model

On iOS and Android, Tauri calls the Rust mobile entry point exported from the
`app` crate. The mobile shell opens `tauri-assets/mobile.html` as the setup
page, lets the operator enter the server/control addresses and startup defaults,
then starts the existing client runtime on a background Rust thread. Once the
runtime is running, the shell navigates to the same local phone-shaped client UI
served by the desktop runtime. That local UI is the main client surface on
mobile too; the setup page remains reachable through the mobile-only `Setup`
button in the local UI or the `Controls` button on the setup page.

The mobile settings file is stored in the platform app config directory as
`intercom-app-settings.json`. The settings page forces local UI on because the
local UI is the actual mobile control surface.

On iOS, the setup page also keeps a saved/recent server picker. `Scan` browses
the LAN for Bonjour `_intercom-suite._tcp.local` services and fills the audio,
control, and admin addresses from the server TXT metadata. Manual audio/control
entry remains the fallback when local-network permission is denied or Bonjour is
not available on the LAN.

## Permissions

iOS:

- `NSMicrophoneUsageDescription` for live intercom capture.
- `NSLocalNetworkUsageDescription` for LAN UDP/WebSocket server access.
- `NSBonjourServices = _intercom-suite._tcp` for LAN server discovery.
- `UIBackgroundModes = audio` so the app can be evaluated for ongoing intercom
  audio while backgrounded.
- App Transport Security allows local networking and web content loads for the
  app-hosted local UI.

Before the Rust audio streams are built, iOS configures `AVAudioSession` as
`PlayAndRecord` / `VoiceChat`, requests microphone permission, prefers 48 kHz,
defaults output to the speaker, and allows Bluetooth routing. Route changes,
interruptions, media-service resets, and foreground transitions reactivate the
session. The current v1 background rule is to preserve the existing listen/talk
state while the app is backgrounded or locked; there are no lock-screen PTT
controls yet.

Android:

- `INTERNET` and `ACCESS_NETWORK_STATE` for UDP/WebSocket networking.
- `RECORD_AUDIO` and `MODIFY_AUDIO_SETTINGS` for capture/playback.
- `WAKE_LOCK` for keeping audio sessions stable during long-running tests.
- `POST_NOTIFICATIONS`, `FOREGROUND_SERVICE`, and
  `FOREGROUND_SERVICE_MICROPHONE` are declared for the later foreground-service
  lifecycle pass. The current activity requests microphone permission at startup.
- Debug builds allow cleartext traffic so LAN `ws://` control URLs and the
  local HTTP client UI work during development.

## Local Toolchain

The macOS development setup used for this repo is:

```sh
brew install xcodegen libimobiledevice
brew install --cask android-commandlinetools
cargo install tauri-cli --locked
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export NDK_HOME=$ANDROID_HOME/ndk/29.0.14206865
export ANDROID_NDK=$NDK_HOME
export ANDROID_NDK_HOME=$NDK_HOME
export ANDROID_NDK_ROOT=$NDK_HOME
export CMAKE_MAKE_PROGRAM=/usr/bin/make
sdkmanager "platform-tools" "platforms;android-35" "build-tools;35.0.1" "ndk;29.0.14206865"
```

Use `cargo-tauri` 2.11.0 or newer for iOS simulator work with Xcode 26. Older
Tauri CLI-generated Apple projects can select a custom `arm64-sim` Xcode
architecture, which causes clang to fail with an invalid simulator target
triple.

Install the Rust mobile targets:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
```

Run the repo doctor:

```sh
clients/app/scripts/mobile-doctor.sh
```

Generate or refresh native projects:

```sh
cd clients/app
ANDROID_HOME=/opt/homebrew/share/android-commandlinetools \
NDK_HOME=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK_HOME=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK_ROOT=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
CMAKE_MAKE_PROGRAM=/usr/bin/make \
cargo tauri android init --ci --skip-targets-install
cargo tauri ios init --ci --skip-targets-install
```

Run on devices/simulators:

```sh
cd clients
ANDROID_HOME=/opt/homebrew/share/android-commandlinetools \
NDK_HOME=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK_HOME=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
ANDROID_NDK_ROOT=/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865 \
CMAKE_MAKE_PROGRAM=/usr/bin/make \
cargo tauri android dev
./app/scripts/ios-dev.sh
```

The iOS wrapper boots the selected simulator, waits for `xcrun simctl
bootstatus -b`, then runs `cargo tauri ios dev <device> --features=native`.
This avoids the CoreSimulator race where Tauri starts install while the
simulator is still in `Shutdown` or still booting. The default simulator is
`iPhone 17`; override it with:

```sh
IOS_SIMULATOR_NAME="iPhone 17 Pro" ./app/scripts/ios-dev.sh
```

Build an iOS simulator bundle without launching it:

```sh
cd clients
./app/scripts/ios-build-sim.sh
```

The generated debug simulator bundle is written under
`clients/app/gen/apple/build/arm64-sim/`.
The iOS build config also runs `scripts/ios-clean-build-output.sh` first because
`cargo tauri ios build` can fail with `Directory not empty` when rerun against
an existing simulator `.app` bundle. The script moves stale generated output to
`clients/app/gen/apple/build/.stale/`.

The extra Android NDK and CMake variables are intentional. The `opus` crate
builds bundled Opus through CMake when cross-compiling, and CMake needs the NDK
path explicitly on this Apple Silicon setup.

Physical iPhone development is the readiness gate for iOS. Set your Apple team
ID and run the device wrapper from `clients`:

```sh
APPLE_DEVELOPMENT_TEAM=TEAMID ./app/scripts/ios-device-build.sh
APPLE_DEVELOPMENT_TEAM=TEAMID ./app/scripts/ios-device-dev.sh
```

`ios-device-build.sh` requires the `aarch64-apple-ios` Rust target, clears stale
Tauri Apple build output, and builds a development-signed generic device
bundle. It requires the iPhone to already be known to your Apple development
team or provisioning will fail before an app can be produced.
`ios-device-dev.sh` is the first-run physical-device path: it detects only
physical iPhone/iPad/iPod entries from `xcrun xctrace list devices`, rejects
Mac and simulator entries, then runs `cargo tauri ios dev <device>
--features=native` so Xcode targets that device directly. It also injects
Xcode's `-allowProvisioningDeviceRegistration` flag for physical-device dev
runs, because the current Tauri CLI only passes `-allowProvisioningUpdates`.
Set `IOS_DEVICE_ID` or `IOS_DEVICE_NAME` when more than one physical device is
attached or Xcode reports the device name differently.

If Xcode reports `No Accounts` or `No profiles for 'com.intercomsuite.client'`,
the app has reached the signing step but Xcode is not signed in to an Apple
account that can create an iOS App Development profile. Open Xcode > Settings >
Accounts, add the Apple ID for `APPLE_DEVELOPMENT_TEAM`, create or download an
Apple Development certificate, then rerun the wrapper with the iPhone connected
and trusted. If Xcode reports that the device is not registered even with the
wrapper's device-registration flag, add the iPhone UDID manually in the Apple
Developer portal for that team. If Xcode then reports that the bundle
identifier is unavailable, change `clients/app/tauri.conf.json` to a unique
development identifier before regenerating/rerunning.

Android builds require a connected device/emulator and accepted SDK licenses.

## Server Discovery

The server advertises Bonjour/mDNS by default on the control port:

```sh
cargo run -p server -- --advertise-name "Studio Intercom"
cargo run -p server -- --disable-discovery
```

The advertised service is `_intercom-suite._tcp.local`. TXT metadata includes
`audio_port`, `admin_port` when the admin UI is enabled, `name`, `version`, and
`auth`. iOS uses that metadata to populate the picker and persists selected
profiles in the mobile settings file.

## Validation Notes

The first mobile milestone is a real Tauri mobile shell and native project
setup, not a separate mobile-specific audio stack. Audio quality and reliability
must still be validated on actual iOS/Android hardware because mobile operating
systems can suspend background work and can route microphones differently than
desktop `cpal`.

Known follow-ups:

- A foreground-service implementation for Android long-running intercom use.
- Physical iPhone validation for first-run permissions, LAN discovery, duplex
  audio, background/lock survival, Wi-Fi changes, server restarts, and
  Bluetooth route changes.
- Mobile-specific audio backend tuning if `cpal` defaults do not match the
  quality of the desktop/macOS path.
