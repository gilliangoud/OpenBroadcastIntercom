# Bridge App

[Docs index](README.md) | [Root README](../README.md)

The bridge app is a cross-platform desktop launcher for production audio
machines. It manages multiple bridge routes from one UI and starts one `bridge`
process per audio-device route. vMix Browser Source routes are served directly
by the bridge app because vMix needs stable local URLs on the production PC.
During development it can run as a local browser UI or as a Tauri desktop
window; both modes use the same config file and route process manager.

Use it when a Windows vMix PC, macOS laptop, or Linux audio box needs several
in/out routes at once:

- vMix or virtual audio program feed into RedLine.
- PA output from a RedLine channel into a USB/audio interface.
- Production monitor or recorder feed from selected channels.
- RedLine channel mixes exposed to vMix as independent local browser inputs.
- NDI audio receive and send routes when the system NDI runtime is installed.
- A carefully isolated duplex bridge where the physical audio interface already
  prevents feedback.

## Start

```sh
cargo run -p bridge-app
```

For the native desktop wrapper:

```sh
cargo run -p bridge-app --features native --bin bridge-app-native
```

Defaults:

- UI bind: `127.0.0.1:41012`
- config file: `intercom-bridge-app.json` compatibility default
- server UDP: `127.0.0.1:40000`
- control WebSocket: `ws://127.0.0.1:40001`
- admin API: derived from the control host as `http://<host>:40002/admin/api/state`

Useful options:

```sh
cargo run -p bridge-app -- --server 192.0.2.10:40000 --control ws://192.0.2.10:40001
cargo run -p bridge-app -- --admin http://192.0.2.10:40002
cargo run -p bridge-app -- --bridge-bin ./bridge --config-file production-bridges.json
cargo run -p bridge-app -- --init-config
cargo run -p bridge-app -- --print-config
cargo run -p bridge-app -- --no-open
```

The app looks for the `bridge` binary next to itself first, then falls back to a
`bridge` executable on `PATH`. Use `--bridge-bin` when running from a packaged
folder or when the bridge binary lives elsewhere.

The Tauri wrapper opens the same bridge manager inside an OS webview window.
Closing the native window asks the manager to stop all launched bridge routes so
production audio child processes are not left running in the background.

A current bridge manager screenshot is included in
[Client UI Screenshots](client-ui-screenshots.md#bridge-app).

The route editor uses dropdown controls for local audio devices and server
channels. Device dropdowns are discovered from the local audio host through
`cpal`. Channel dropdowns are loaded from the server admin state when reachable;
use `--admin` when the admin API does not live at the derived default. If the
admin API cannot be reached, the app falls back to the standard show channels
plus any channel IDs already present in the saved bridge routes.

## Route Fields

Each route maps directly to one `bridge` process:

- `id`: stable route identifier inside the bridge app.
- `name`: client name shown in admin.
- `user_id`: numeric RedLine alias used by the server.
- `mode`: `input`, `output`, or `duplex`.
- `input_kind`: `audio_device` or `ndi_source`.
- `output_kind`: `audio_device`, `vmix_browser_source`, or `ndi_output`.
- `tx_channels`: RedLine channels fed by the input device, selected from the channel dropdown.
- `listen_channels`: RedLine channels rendered to the output device, selected from the channel dropdown.
- `input_device` / `output_device`: selected from local audio device dropdowns.
- `ndi_source`, `ndi_output_name`, and `ndi_groups`: NDI route settings.
- `codec` and `opus_profile`: normal RedLine edge codec settings.
- `stereo`: enables stereo receive for output/duplex routes when supported.
- `input_gain` / `output_gain`: local linear gain.
- `note`: operator note shown in bridge status.
- `enabled`: included when using Start Enabled.

The bridge app rejects routes that listen and transmit on the same channel. That
guardrail is intentional because a production bridge can easily create feedback
between PA/program audio and RedLine audio.

## vMix Tally

Configure the vMix endpoint from the admin System page. The server uses HTTP XML
for input discovery and can prefer TCP `SUBSCRIBE TALLY` updates for lower-latency
state changes. Per-client tally mappings live in the admin Clients page; map by
stable vMix input key when possible, with input number and title as fallbacks.

Clients receive resolved `off`, `preview`, or `live` state over the control
connection. Shared desktop/mobile/Pi browser UI shows a green edge halo for
preview and a red edge halo for live. Pi hardware clients can also drive
preview/live LEDs through the `pi-gpio` `outputs.preview` and `outputs.live`
config described in [Hardware](hardware.md#tally-leds).

## vMix Browser Source Outputs

For RedLine-to-vMix audio, prefer a `vMix Browser Source` output route over a
virtual audio cable when the bridge app is running on the same vMix PC. The route
creates a stable URL such as:

```text
http://127.0.0.1:41012/vmix/source/program
```

Add that URL to vMix as a Web Browser input. Each route URL becomes a separate
vMix input with its own fader, meters, mute, and routing. Browser-source routes
are output-only and require `listen_channels`; the reverse direction,
vMix-to-RedLine, still needs an input route from a real audio device, virtual
cable, hardware/ASIO output, or a future NDI receive route.

The browser source page is intentionally blank except for a small connection
status label. It opens a local WebSocket and plays 48 kHz Float32 PCM through Web
Audio, so vMix captures it as normal browser audio. Application Audio capture can
still be used as a fallback, but it is less predictable when multiple copies of
the same program are running.

The bridge app reports browser-source route telemetry: connected browser clients,
audio level, source audio frames, WebSocket underflows, dropped/lagged frames,
stale audio state, and time since the last audio frame.

## NDI Audio Routes

NDI is part of the default bridge app configuration surface. Use `NDI Source` for
program/audio feeds coming into RedLine and `NDI Output` for publishing selected
RedLine listen channels as discoverable NDI audio sources. Existing audio-device
routes continue to load unchanged.

NDI support dynamically loads the system NDI runtime. Install the NDI runtime or
SDK on the bridge machine and ensure `libndi` is on the system loader path, or
set `NDI_RUNTIME_DIR`/`NDI_SDK_DIR` to the install directory. If NDI is missing,
non-NDI routes still run normally and NDI routes show a runtime error.
On macOS, RedLine also checks common NDI Tools installs such as
`/usr/local/lib/libndi.4.dylib` and app-bundled copies inside NDI Monitor,
Virtual Input, Scan Converter, Discovery, Video Monitor, and Test Patterns.

The route editor discovers available NDI sources for the `NDI Source` field.
`NDI Output` publishes the route's selected RedLine listen-channel mix as a
discoverable NDI audio source with the configured output name and optional NDI
groups. V1 is audio-only and uses RedLine's normal 48 kHz path.

NDI bridge routes report endpoint telemetry through normal bridge status:
runtime library, audio level, frames, stale state, underflows, drops, reconnects,
and time since last audio. The admin Session Health view includes those warnings
and counters.

## vMix Windows PC Example

Run the server somewhere reachable on the LAN, then run the bridge app on the
vMix PC:

```sh
cargo run -p bridge-app -- --server 192.0.2.10:40000 --control ws://192.0.2.10:40001
```

Create routes:

| Route | Mode | Device | Channels | Notes |
| --- | --- | --- | --- | --- |
| `program-in` | `input` | vMix/virtual audio output | TX `1` Program | Clean program feed into IFB. |
| `pa-out` | `output` | USB interface/virtual cable | Listen `6` PA | Feed arena PA or vMix input. |
| `program-source` | `output` | vMix Browser Source URL | Listen `1` Program | Independent local vMix browser input. |
| `production-monitor` | `output` | Headphones/interface | Listen `2` Production PL | Local show-control monitor. |

Use `pcm48` first for wired/local audio quality. Use Opus profiles when network
bandwidth matters more than absolute quality.

## Admin Visibility

Bridge routes still appear as normal clients with role `bridge`. The admin UI
shows:

- selected bridge mode
- selected input/output device names
- listen/TX channels
- local input/output gains
- route note
- input/output meters and queue health
- warnings for feedback-risk or PA/program bridge mistakes

The server also includes the live bridge status in `GET /admin/api/state` under
each session’s `bridge` field.

## Packaging Notes

For a production machine, ship the native `bridge-app-native` binary and
`bridge` in the same folder so the launcher can find the bridge binary without
extra configuration. Build the native development binary with:

```sh
cargo build -p bridge-app --features native --bin bridge-app-native --release
```

The Tauri bundle config lives at `clients/bridge-app/tauri.conf.json`. Platform
installers should be produced on the target OS or a CI runner prepared for Tauri
packaging. The helper script follows the same host defaults as the native client
app:

```sh
clients/bridge-app/scripts/package-native.sh
```

The non-native browser mode remains useful for headless hosts and quick
SSH/remote setup.
