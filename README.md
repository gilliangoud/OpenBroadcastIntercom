# RedLine

RedLine is a low-latency broadcast intercom for production teams, sports crews,
referees, and live event operators. It combines a Rust audio server, desktop and
mobile clients, headless hardware clients, and bridge tools for vMix, NDI, PA,
program audio, and recording workflows.

This README is the starting point. Detailed references live in
[docs/README.md](docs/README.md).

## What Is Included

| Area | Purpose |
| --- | --- |
| `server` | UDP audio mixer, WebSocket control plane, admin UI/API, recording, transcription, model management, tally integration. |
| `clients/app` | Tauri desktop/mobile client shell using the shared operator UI. |
| `clients/desktop` | Rust desktop CLI/local browser client using `cpal` audio I/O. |
| `clients/bridge` | Headless audio bridge endpoint for production routing. |
| `clients/bridge-app` | Multi-route bridge manager with audio device, vMix browser source, and NDI route setup. |
| `clients/pi` | Headless Raspberry Pi/reference client. |
| `clients/pi-gpio` | GPIO companion for physical buttons and tally outputs. |
| `clients/esp32` | ESP32-A1S / ES8388 firmware scaffold. |
| `clients/core` | Shared client runtime for config, audio codecs, playback buffering, and reconnect behavior. |
| `common` | Shared protocol, status, audio, processing, and admin types. |
| `docs` | Setup guides, architecture notes, command/API references, hardware, release, and benchmark docs. |

## Quick Start

Run the server:

```sh
cargo run -p server
```

Open the admin UI:

```text
http://127.0.0.1:40002/admin/
```

Run two local desktop clients in separate terminals:

```sh
cargo run -p desktop -- --user-id 1 --tx-channel 1 --listen-channel 1
cargo run -p desktop -- --user-id 2 --tx-channel 1 --listen-channel 1
```

Run the app-host client:

```sh
cargo run -p app
```

Run the native Tauri client during development:

```sh
cargo run -p app --features native --bin app-native -- --user-id 1
```

Run the bridge manager for production audio routing:

```sh
cargo run -p bridge-app
```

Run the native bridge manager during development:

```sh
cargo run -p bridge-app --features native --bin bridge-app-native
```

For more commands and options, see
[Command Reference](docs/command-reference.md).

## Common Paths

| Goal | Start Here |
| --- | --- |
| Understand how the system fits together | [Architecture](docs/architecture.md) |
| Run clients and package native apps | [Native App](docs/native-app.md), [Mobile Tauri Clients](docs/mobile-clients.md) |
| Configure bridge routes, vMix browser sources, or NDI | [Bridge App](docs/bridge-app.md) |
| Build Pi/ESP32 hardware clients | [Hardware](docs/hardware.md), [ESP32 README](clients/esp32/README.md) |
| Download Whisper or DeepFilterNet models | [Model Assets](docs/model-assets.md) |
| Compare transcription models | [Transcription Benchmarks](docs/transcription-benchmarks.md) |
| Validate a venue or field test | [Field Testing Runbook](docs/field-testing.md) |
| Build release artifacts | [Release Automation](docs/release-automation.md), [Generated Artifact Policy](docs/generated-artifacts.md) |
| Keep the public repo clean | [Public Repository Hygiene](docs/public-repo-hygiene.md) |

## Model Assets

Large model weights stay outside Git. Curated Whisper and DeepFilterNet assets
are declared in `intercom-models/manifest.json` and can be downloaded manually:

```sh
tools/download-whisper-models.py --list
tools/download-whisper-models.py
```

Downloaded models live in ignored catalog destination folders and are visible
from the server admin System page. The current default external transcription
model is `ggml-large-v3-turbo-q5_0.bin`. See
[Model Assets](docs/model-assets.md) for catalog and hosting details.

## Security Notes

The server admin UI/API has no authentication unless `--admin-token` or
`INTERCOM_ADMIN_TOKEN` is set. Local client UIs/APIs also need
`--local-ui-token`, `--local-api-token`, or the matching environment variables
when exposed beyond localhost.

Use no-auth mode only for bench testing or physically isolated networks. On a
shared LAN, bind admin/local APIs to trusted interfaces, set tokens, and put TLS
or public access behind a reverse proxy.

## Repository State

Use `intercom-state.example.json` and `intercom-app-settings.example.json` as
sanitized starting points. Do not commit local runtime state, credentials,
signing material, downloaded model weights, or generated build output.

Some compatibility filenames and service identifiers still use `intercom-*`
names even though the product name is RedLine.

## Test

Run the full Rust workspace tests:

```sh
cargo test --workspace
```

Run clippy when preparing code changes:

```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Docs-only changes normally need link/preview checks rather than full builds.
