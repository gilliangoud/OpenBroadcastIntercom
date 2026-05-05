# Bridge App

The bridge app is a cross-platform desktop launcher for production audio
machines. It manages multiple bridge routes from one UI and starts one `bridge`
process per route. During development it can run as a local browser UI or as a
Tauri desktop window; both modes use the same config file and route process
manager.

Use it when a Windows vMix PC, macOS laptop, or Linux audio box needs several
in/out routes at once:

- vMix or virtual audio program feed into Intercom.
- PA output from an Intercom channel into a USB/audio interface.
- Production monitor or recorder feed from selected channels.
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
- config file: `intercom-bridge-app.json`
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
- `user_id`: numeric Intercom alias used by the server.
- `mode`: `input`, `output`, or `duplex`.
- `tx_channels`: Intercom channels fed by the input device, selected from the channel dropdown.
- `listen_channels`: Intercom channels rendered to the output device, selected from the channel dropdown.
- `input_device` / `output_device`: selected from local audio device dropdowns.
- `codec` and `opus_profile`: normal Intercom edge codec settings.
- `stereo`: enables stereo receive for output/duplex routes when supported.
- `input_gain` / `output_gain`: local linear gain.
- `note`: operator note shown in bridge status.
- `enabled`: included when using Start Enabled.

The bridge app rejects routes that listen and transmit on the same channel. That
guardrail is intentional because a production bridge can easily create feedback
between PA/program audio and intercom audio.

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
