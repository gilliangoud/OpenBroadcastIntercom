# Architecture

[Docs index](README.md) | [Root README](../README.md)

RedLine is built around one authoritative server and multiple clients. Audio
uses UDP for low latency; control and configuration use JSON over WebSocket and
HTTP admin APIs.

## Runtime Pieces

| Piece | Role |
| --- | --- |
| `server` | Receives client audio, decodes to 48 kHz PCM, applies per-client processing, mixes per listener, sends mixed audio back, serves admin UI/API, records audio, and manages transcription/model assets. |
| `clients/core` | Shared runtime for desktop, app-host, Pi, and bridge clients: identity, config state, codecs, playback buffering, local control, and reconnect behavior. |
| `clients/desktop` | Local Rust desktop client with terminal commands and a browser UI. |
| `clients/app` | Native/app-host wrapper over the same client runtime and shared UI. |
| `clients/bridge` | Headless production audio endpoint for audio-device and NDI routes. |
| `clients/bridge-app` | Multi-route bridge manager, vMix browser-source host, and native bridge launcher. |
| `clients/pi` / `clients/pi-gpio` | Headless hardware client and GPIO button/tally companion. |
| `clients/esp32` | ESP32-A1S firmware path using the RedLine control and audio protocol. |
| `common` | Shared packet, protocol, processing, status, and admin types. |

## Default Ports

| Service | Default |
| --- | --- |
| Server UDP audio | `0.0.0.0:40000` |
| Server WebSocket control | `0.0.0.0:40001` |
| Server admin UI/API | `0.0.0.0:40002` |
| Desktop local UI/API | `127.0.0.1:41002` |
| Pi local API | `0.0.0.0:41001` |
| Bridge app UI and vMix browser sources | `127.0.0.1:41012` |

## Audio Flow

Clients capture local audio, encode edge packets as PCM16, PCM24, PCM48, or
Opus, and send UDP packets to the server. The server decodes edge audio to
48 kHz PCM, optionally runs a per-client input processing chain, then mixes one
listener-specific output stream for each connected client.

Mix-minus is enabled by default, so a user does not receive their own microphone
back. The mixer applies channel routing, listener channel gain, per-listener
per-talker gain, priority ducking, IFB program ducking, stereo panning where
enabled, and a fixed-latency limiter before encoding output packets.

Each talker has a bounded source queue so late or bursty packets cannot grow
latency without limit. The server reports queue depth, frame drops, decode
errors, input/output meters, limiter activity, processing status, and active
talker state to the admin UI/API.

## Control Flow

Clients keep a persistent WebSocket control connection. On startup they send a
`hello` message with their requested numeric alias, stable `client_uid`,
supported codecs, advertised buttons, and client-local capabilities. The server
enrolls or matches the stable device, assigns the effective `user_id`, and
pushes authoritative `config_update` messages.

Admin UI/API and admin CLI updates are persisted as desired server config. If a
client is online, the server applies the change and pushes it immediately. If a
client is offline, the desired config is staged and applied when that device or
user connects again.

Clients also send a small UDP registration packet on startup and periodically
afterward. This lets receive-only, muted, and bridge clients appear online and
receive mixed audio even before they transmit microphone audio.

## Identity And Enrollment

RedLine uses two identifiers:

- `user_id`: short numeric operator alias used in audio packets, routing,
  direct calls, admin controls, and UI labels.
- `client_uid`: stable UUID for a deployed device identity.

Desktop, app-host, Pi, and bridge clients create and reuse a local identity file
by default. ESP32 stores its generated UUID in NVS, with an optional menuconfig
override. Enrollment policy controls unknown devices:

- `auto`: enroll and assign the requested alias when available.
- `approval`: record as pending and block client-owned audio/control until
  approved.
- `preconfigured-only`: reject unknown UIDs unless already present in state.

## Routing, Buttons, IFB, And Tally

Normal listen/TX routing is channel-based. Dedicated talk buttons are advertised
by clients but configured by the server/admin UI. Button actions can transmit to
channels or users, send alerts, apply presets, change talk mode, or edit routes.

IFB is listener-side. A client can have program channels, interrupt channels,
and a duck gain. When interrupt audio is active for that listener, only the
configured program channels duck.

Emergency override is source-side and can reach all clients, selected clients,
or listeners of selected channels regardless of normal routing.

vMix tally is server-owned. The server resolves each mapped RedLine client to
`off`, `preview`, or `live` and pushes that state over the existing control
connection. Shared client UI shows green preview or red live halos; Pi GPIO can
mirror the same state with LEDs.

## Audio Processing And Models

Server-owned processing is configured per client. The processing block can run:

- `built_in`: low-latency high-pass, gate/VAD, transient suppression,
  compression, and presence chain.
- `webrtc`: bundled WebRTC APM for high-pass, noise suppression, AGC/limiting,
  and VAD state.
- `rnnoise`: Xiph RNNoise on 48 kHz / 10 ms frames.
- `deepfilternet`: DeepFilterNet model processing with runtime/backend status
  and fallback behavior.

The same block can enable loudness normalization after cleanup and before
routing/mixer gains. The admin UI reports active engine, stage availability,
backend, timing, gate state, RMS, gain reduction, fallback reason, and leveler
gain.

Transcription is opt-in. The server can record processed mono client ingest WAVs
and run local Whisper-based live or recording transcription. Model downloads are
manual-only from the curated catalog.

## Bridge And Production Integration

The bridge client connects production audio sources and sinks to RedLine. It can
use normal audio devices, receive NDI audio into RedLine, or publish RedLine
listen-channel mixes as NDI audio sources when the system NDI runtime is
installed.

The bridge app manages multiple routes from one UI. It also serves vMix Browser
Source output routes from stable localhost URLs so vMix can ingest separate
RedLine mixes as independent browser inputs without virtual audio cables.

## Security Model

No-auth defaults are for local bench testing. On shared networks:

- Set `--admin-token` or `INTERCOM_ADMIN_TOKEN` for the server admin UI/API.
- Set `--local-ui-token` or `--local-api-token` for client local UIs/APIs when
  exposed beyond localhost.
- Keep control/admin binds on trusted networks or behind a reverse proxy.
- Terminate TLS and public access outside the Rust services for now.
