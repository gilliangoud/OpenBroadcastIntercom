# Command Reference

[Docs index](README.md) | [Root README](../README.md)

This page collects the common RedLine commands. Use `--help` on each binary for
the exact parser output supported by the current build.

## Server

```sh
cargo run -p server -- [OPTIONS]
```

Common options:

| Option | Purpose |
| --- | --- |
| `--audio-bind <ADDR>` | UDP audio bind. Default: `0.0.0.0:40000`. |
| `--control-bind <ADDR>` | WebSocket control bind. Default: `0.0.0.0:40001`. |
| `--admin-bind <ADDR>` | Admin UI/API bind. Default: `0.0.0.0:40002`. |
| `--admin-state-file <PATH>` | Desired clients, devices, presets, templates, channels, and system config JSON. |
| `--enrollment-policy <auto|approval|preconfigured-only>` | Unknown device policy. |
| `--admin-token <TOKEN>` | Require auth for `/admin/` and `/admin/api`; also `INTERCOM_ADMIN_TOKEN`. |
| `--recordings-dir <PATH>` | Recording session output directory. |
| `--debug-audio-dir <PATH>` | Diagnostic WAV taps before DSP and before output encoding. |
| `--whisper-model <PATH>` | Whisper model for live/recording transcription; also `INTERCOM_WHISPER_MODEL`. |
| `--whisper-model-dir <PATH>` | Folder scanned by the admin UI for selectable Whisper models. |
| `--deepfilternet-model-dir <PATH>` | Folder scanned by the admin UI for DeepFilterNet models. |
| `--transcription-engine <disabled|builtin-whisper|external-whisper>` | Transcription engine selection. |
| `--whisper-command <PATH>` | External Whisper command when using `external-whisper`. |
| `--disable-admin-ui` | Disable HTTP admin UI/API. |

macOS accelerated server build:

```sh
cargo build -p server --release --features macos-accelerated
```

`macos-accelerated` enables the native cleanup/transcription feature set:
WebRTC APM, RNNoise, DeepFilterNet, DeepFilterNet Core ML package support, and
`whisper-rs/metal`.

## Desktop Client

```sh
cargo run -p desktop -- [OPTIONS] --user-id <USER_ID>
```

Common options:

| Option | Purpose |
| --- | --- |
| `--server <ADDR>` | UDP server address. Default: `127.0.0.1:40000`. |
| `--control <URL>` | WebSocket control URL. Default: `ws://127.0.0.1:40001`. |
| `--user-id <ID>` | Requested numeric alias. |
| `--client-uid <UUID>` / `--identity-file <PATH>` | Stable identity override or identity file path. |
| `--tx-channel <CHANNEL>` / `--listen-channel <CHANNEL>` | Initial regular TX and listen channels. May be repeated where supported. |
| `--codec <pcm16|pcm24|pcm48|opus>` | Edge codec. |
| `--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>` | Opus quality profile. |
| `--mic-gain <GAIN>` / `--speaker-gain <GAIN>` | Local linear gain. |
| `--input-limiter` | Enable local soft input limiter. |
| `--jitter-ms <MS>` | Playback prebuffer, `0` to `250`. |
| `--input-device <NAME>` / `--output-device <NAME>` | Select devices by case-insensitive substring. |
| `--input-backend <auto|raw|voice-processing>` | Capture backend; macOS can use VoiceProcessingIO for the default input. |
| `--input-channel <average|left|right>` | Multi-channel input selection/downmix. |
| `--button <ID[=LABEL]>` | Advertise a dedicated button slot. May be repeated. |
| `--button-key <ID=KEY>` | Focused-terminal hotkey for a button. |
| `--local-ui-bind <ADDR>` | Local browser UI/API bind. Default: `127.0.0.1:41002`. |
| `--local-ui-token <TOKEN>` | Require auth for local UI/API; also `INTERCOM_LOCAL_UI_TOKEN`. |
| `--disable-local-ui` | Disable local browser UI/API. |
| `--list-devices` | Print audio devices and exit. |

Runtime terminal commands:

```text
tx 2
listen 1,2
vol 2=0.6
button director down
button director up
talk-mode open
talk on
talk off
mute
unmute
buttons
show
help
```

Audio diagnostics:

```sh
cargo run -p server -- --debug-audio-dir ./debug-audio/server
cargo run -p desktop -- --user-id 1 --codec pcm48 --input-channel average --debug-audio-dir ./debug-audio/client-1
```

## App Host And Native Client

```sh
cargo run -p app -- [OPTIONS]
cargo run -p app --features native --bin app-native -- --user-id 1
```

Useful app commands:

```sh
cargo run -p app -- --init-config --user-id 1 --button director=Director
cargo run -p app -- --print-config
cargo run -p app -- --write-config --user-id 2
cargo build -p app --features native --bin app-native --release
clients/app/scripts/package-native.sh
```

Important options include `--config-file`, `--server`, `--control`,
`--user-id`, `--client-uid`, `--identity-file`, `--tx-channel`,
`--listen-channel`, `--codec`, `--opus-profile`, `--mic-gain`,
`--speaker-gain`, `--jitter-ms`, `--input-device`, `--output-device`,
`--input-backend`, `--button`, `--button-key`, `--local-ui-bind`,
`--local-ui-token`, `--window-mode <system-browser|native|disabled>`,
`--app-title`, `--ui-open-delay-ms`, and `--list-devices`.

See [Native App](native-app.md) and [Mobile Tauri Clients](mobile-clients.md).

## Bridge And Bridge App

Headless bridge:

```sh
cargo run -p bridge -- --user-id 90 --name "vMix Program" --mode input --tx-channels 20 --input-device "BlackHole" --codec pcm48
cargo run -p bridge -- --user-id 91 --name "PA Output" --mode output --listen-channels 30 --output-device "USB Audio" --codec pcm48
```

Bridge manager:

```sh
cargo run -p bridge-app
cargo run -p bridge-app --features native --bin bridge-app-native
clients/bridge-app/scripts/package-native.sh
```

Common bridge options:

| Option | Purpose |
| --- | --- |
| `--mode <input|output|duplex>` | Capture into RedLine, play out of RedLine, or both. |
| `--user-id <ID>` / `--client-uid <UUID>` | Requested alias and stable identity. |
| `--tx-channels <CSV>` / `--listen-channels <CSV>` | RedLine channel routing. |
| `--input-device <NAME>` / `--output-device <NAME>` | Audio device selectors. |
| `--input-kind <audio-device|ndi-source>` | Input endpoint type. |
| `--output-kind <audio-device|vmix-browser-source|ndi-output>` | Output endpoint type. |
| `--ndi-source <NAME>` / `--ndi-output-name <NAME>` / `--ndi-groups <CSV>` | NDI receive/send settings. |
| `--input-gain <GAIN>` / `--output-gain <GAIN>` | Bridge-local gain. |
| `--codec <pcm16|pcm24|pcm48|opus>` / `--opus-profile <...>` | Edge codec and Opus quality. |
| `--stereo` | Request stereo receive where supported. |
| `--list-devices` / `--list-ndi-sources` | Inspect audio devices or NDI sources. |

See [Bridge App](bridge-app.md) for route fields, vMix browser sources, NDI, and
packaging notes.

## Pi Client

```sh
cargo run -p pi -- [OPTIONS] --user-id <USER_ID>
```

The Pi client supports the same core server/control/identity/routing/codec/gain
options as the desktop client, plus:

| Option | Purpose |
| --- | --- |
| `--receive-only` | Start muted with no TX channels. |
| `--local-api-bind <ADDR>` | Local HTTP API bind. Default: `0.0.0.0:41001`. |
| `--local-api-token <TOKEN>` | Require auth for the local API; also `INTERCOM_LOCAL_API_TOKEN`. |
| `--disable-local-api` | Disable local HTTP API. |

The Pi client is intended for unattended/headless use and has no stdin command
loop. Configure it at startup, from the admin UI/API, or through the local API.

## Pi GPIO Companion

```sh
cargo run -p pi-gpio -- --init-config
cargo run -p pi-gpio
```

Common options:

| Option | Purpose |
| --- | --- |
| `--config-file <PATH>` | GPIO mapping JSON. Default: `pi-buttons.json`. |
| `--local-api <URL>` | Pi local API base URL. |
| `--local-api-token <TOKEN>` | Bearer token for Pi local API. |
| `--gpio-root <PATH>` | GPIO sysfs root. Default: `/sys/class/gpio`. |
| `--dry-run` | Log button events without sending HTTP requests. |

GPIO mappings can drive regular talk, configured talk buttons, and preview/live
tally outputs.

## ESP32 Firmware

Use the repo wrapper:

```sh
tools/esp32 setup
tools/esp32 doctor
tools/esp32 menuconfig
tools/esp32 build
tools/esp32 flash-monitor /dev/cu.usbserial-XXXX
```

Exit the ESP-IDF serial monitor with `Ctrl+]`. If ESP-IDF refuses to clean a
stale partial build folder, run:

```sh
tools/esp32 reset-build
```

See [ESP32 README](../clients/esp32/README.md) and [Hardware](hardware.md).

## Admin CLI

Inspect sessions:

```sh
cargo run -p admin -- status
```

Update route and processing config:

```sh
cargo run -p admin -- config --user-id 1 --listen 1,2 --tx 1 --vol 2=0.6
cargo run -p admin -- config --user-id 1 --codec opus --opus-profile speech-48-high
cargo run -p admin -- config --user-id 1 --processing-engine rnnoise --processing-mode enabled --processing-profile voice-isolation
cargo run -p admin -- config --user-id 20 --listen 1,9 --tx 1 --ifb-enabled true --ifb-program 1 --ifb-interrupt 9 --ifb-duck-gain 0.125
```

Change runtime state:

```sh
cargo run -p admin -- codec --user-id 1 --codec pcm48
cargo run -p admin -- talk-mode --user-id 1 --mode ptt
cargo run -p admin -- talk --user-id 1 --active true
cargo run -p admin -- priority --user-id 1 --active true
cargo run -p admin -- emergency --user-id 1 --active true --target all --duck-gain 0.125
```

Global option:

| Option | Purpose |
| --- | --- |
| `--control <URL>` | WebSocket control URL. Default: `ws://127.0.0.1:40001`. |

Commands include `status`, `field-report`, `config`, `codec`, `talk-mode`,
`talk`, `priority`, and `emergency`.

## UDP Impairment Proxy

```sh
cargo run -p netem -- --listen 127.0.0.1:41000 --server 127.0.0.1:40000 --drop-percent 2 --delay-ms 20 --jitter-ms 20
```

Options:

| Option | Purpose |
| --- | --- |
| `--listen <ADDR>` | UDP address clients send to. |
| `--server <ADDR>` | Real UDP audio server address. |
| `--drop-percent <PERCENT>` | Random packet loss in both directions. |
| `--delay-ms <MS>` | Fixed per-packet delay. |
| `--jitter-ms <MS>` | Random delay variation around `--delay-ms`. |
| `--seed <N>` | Deterministic pseudo-random seed. |
| `--stats-interval-ms <MS>` | Packet counter log interval; `0` disables. |

For field validation, see [Field Testing Runbook](field-testing.md).
