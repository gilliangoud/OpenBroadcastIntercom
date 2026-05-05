# Rust Intercom Prototype

This repository contains a first-pass intercom prototype:

- `common`: shared packet format and control messages.
- `clients/core`: shared client runtime pieces for config state, audio codec
  encode/decode, playback buffering, and WebSocket reconnect/control handling.
- `server`: authoritative UDP audio receiver, per-listener PCM mixer, WebSocket control endpoint, and web admin UI.
- `clients/desktop`: Rust CLI desktop client using `cpal` for microphone and speaker I/O.
- `clients/app`: app-host entry point that reuses the desktop runtime and local browser UI, ready to grow into a native wrapper.
- `clients/pi`: headless Raspberry Pi/reference client using the same protocol and audio path.
- `clients/pi-gpio`: Raspberry Pi GPIO companion that maps physical buttons to the Pi client's local HTTP API.
- `clients/esp32`: ESP-IDF firmware scaffold for the Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S ES8388 board.

## Public Clone Notes

This repository uses Git LFS for curated model assets. Install Git LFS before
cloning if you want the included Whisper, DeepFilterNet, and Supertonic models:

```sh
git lfs install
git clone <repo-url>
```

Large optional Whisper models are intentionally not committed. Place additional
models in `intercom-models/` or point the server at another folder with
`--whisper-model-dir`. Use `intercom-state.example.json` and
`intercom-app-settings.example.json` as sanitized starting points; do not commit
local runtime state, credentials, signing material, or generated build output.
See `docs/public-repo-hygiene.md` before publishing changes.

The default/debug edge audio format is 16 kHz mono PCM16 in 10 ms frames.
`pcm24` and `pcm48` are also available when bandwidth is less important than
quality. `pcm48` and Opus listeners can optionally receive stereo mixes with
per-channel left/center/right panning; PCM16 and PCM24 remain mono. Opus is
available in low/standard/high speech profiles plus a music/high-detail profile.
Audio packets use UDP.
Control messages use JSON over WebSocket. Desktop clients keep one persistent
control connection open for startup config, talk-mode changes, and runtime route
updates. They also register that connection with `hello`, so the server can push
config corrections back to the client after it has connected.

The server decodes edge audio to 48 kHz PCM, mixes active talkers every 10 ms,
and sends each listener one stream. Mix-minus is enabled by default, so a user
does not receive their own mic back. Each talker has a bounded six-frame source
queue so the mixer consumes frames in order without allowing latency to grow
without limit. The mixer applies per-channel listener gain, per-listener
per-talker gain, priority ducking, and IFB program ducking before limiting.
Priority is scoped per channel: when a priority user is actively transmitting
on one of their configured priority channels, non-priority sources on that same
channel are ducked by 12 dB for affected listeners. Emergency all-call is a
separate override path that can reach all clients, selected clients, or
listeners of selected channels regardless of normal listen routing. Stereo-enabled
`pcm48` and Opus listeners receive an interleaved left/right mix built from
their per-channel pan map; mono listeners receive the current single-channel
mix. Output mixes pass through a fixed-latency peak limiter instead of hard
clipping, and the server tracks input/output meters, active talkers, limiter
activity, queue depth, frame drops, and decode errors for the admin UI/API.
Before frames enter the mixer, the server can run a per-client input processing
chain. The processing object has an `engine`, a speech `profile`, and optional
ordered `pipeline` stages. `built_in` uses the low-latency high-pass, gate/VAD,
transient suppression, compressor, and presence chain. `webrtc` runs bundled
WebRTC APM in-process for high-pass, noise suppression, AGC/limiting, and voice
activity state. `rnnoise` runs Xiph RNNoise in-process on the server's 48 kHz /
10 ms frames. `deepfilternet` runs DeepFilterNet ONNX `.tar.gz` models through
the bundled Rust/Tract backend on a per-user worker thread. Its config includes
backend selection fields so Apple/Core ML acceleration can be requested from the
same operator UI; the current runtime uses Tract safely and reports when a
Core ML request falls back. If the model is not configured, cannot load, or
cannot keep up, the server falls back to `built_in` when `fallback_to_builtin`
is true or bypasses processing when it is false.
`auto` mode bypasses clean PCM48 links for the built-in engine and applies
conservative low-rate speech enhancement for PCM16, PCM24, and Opus; `enabled`
forces processing; `disabled` bypasses it. The same processing block can enable
loudness normalization, a low-latency speech leveler that runs after cleanup and
before routing/mixer gains. It targets a configured RMS while respecting max
boost, max attenuation, adaptation speed, and a noise-floor guard so whispers
can be lifted without raising idle room noise. Processing status is reported per
client so operators can see the active engine, stage availability, selected
backend, inference time, gate state, RMS, gain reduction, and normalization gain.

Clients can also advertise dedicated talk button slots. The server/admin UI owns
what those buttons do: each configured button can be momentary or latching and
can run one or more declarative actions, including transmitting to channels or
direct users, sending alerts, changing talk mode, applying presets, or editing
routes. If several buttons are active, their transmit targets are unioned and
deduplicated.

Clients send a small UDP registration packet on startup and periodically after
that, independent of active talk routes. This lets receive-only or muted clients
appear online and receive mixed audio before they ever transmit microphone
audio.

Desktop, app-host, and Pi clients share the same client runtime. Edge codec
conversion keeps 48 kHz as the client mixer/playback domain and uses persistent
rubato resamplers for PCM16, PCM24, PCM48, and Opus frame conversion instead of
recreating a resampler for every packet.

Each listener can also have one server-owned IFB block. IFB defines program
channels, interrupt channels, an enabled flag, and a duck gain. When interrupt
audio is active for that listener, only the configured program channels are
ducked, while the interrupt audio remains at normal channel volume.

## Run

Start the server:

```sh
cargo run -p server
```

Start two clients in separate terminals:

```sh
cargo run -p desktop -- --user-id 1 --tx-channel 1 --listen-channel 1
cargo run -p desktop -- --user-id 2 --tx-channel 1 --listen-channel 1
```

Start a headless Pi/reference client:

```sh
cargo run -p pi -- --user-id 10 --tx-channel 1 --listen-channel 1
```

Create a GPIO mapping for Pi physical buttons:

```sh
cargo run -p pi-gpio -- --init-config
cargo run -p pi-gpio
```

Build the ESP32-A1S firmware with the repo wrapper:

```sh
tools/esp32 setup
tools/esp32 doctor
tools/esp32 menuconfig
tools/esp32 build
tools/esp32 flash-monitor /dev/cu.usbserial-XXXX
```

Exit the ESP-IDF serial monitor with `Ctrl+]`; `Ctrl+C` is not the normal
monitor exit key. Use `Ctrl+T` then `Ctrl+H` inside the monitor for its shortcut
help.

If ESP-IDF refuses to clean a stale partial `clients/esp32/build` folder, run
`tools/esp32 reset-build` and then retry `tools/esp32 menuconfig`.

The first ESP32 firmware target is the Ai-Thinker ESP32 Audio Kit V2.2 /
ESP32-A1S with ES8388, PCM16/PCM24/PCM48, WebSocket control, UDP mixed receive,
microphone transmit, active-low PTT, and two advertised dedicated button slots.
Configure Wi-Fi, server IP, requested user ID, optional stable client UID,
ES8388 ADC input, mic PGA gain, capture channel, GPIOs, and optional local
sidetone in `tools/esp32 menuconfig`.
Those audio settings are the firmware fallback. Once `esp32_audio` is enabled
for the client in the admin UI/API, the server overrides the runtime ESP32
audio settings on each config update.
Sidetone currently uses the safe firmware mix path during bring-up. The older
ES8388 line-bypass config fields are retained for visibility, but codec-bypass
sidetone is forced off until the fixed playback/capture baseline is stable.
The ESP32 ES8388/I2S hardware path stays fixed at `48 kHz`, `16-bit`, stereo.
Network codec changes are software conversion only: `pcm16` sends 16 kHz
packets, `pcm24` sends 24 kHz packets, and `pcm48` stays native at 48 kHz.
Start new hardware on `pcm16`, confirm clean capture health and packet TX, then
switch to `pcm24` or `pcm48` in the admin UI for better playback and mic
quality. `pcm48` is the current known-good ESP32 quality mode. `pcm16` now uses
lightweight interpolation/FIR resampling on the ESP32, but it remains the
bandwidth-saving speech tier rather than the quality baseline. ESP32 Opus
remains deferred until CPU headroom is measured.

For ESP32 board-mic bring-up, start with:

- `ES8388 ADC input`: `Differential board mic`
- `ES8388 mic PGA gain`: `9 dB`
- `Capture channel`: `Left`
- `Capture high-pass/DC blocker`: enabled
- `Microphone software gain percent`: `100`
- `Notification sound gain percent`: `50`

The firmware reports capture health once per second. The admin client editor
shows left/right/selected RMS, peak, DC offset, raw clipping, software clipping,
whether server-owned audio is active, and the active ES8388 codec config so
gain can be fixed at the ES8388 before server DSP is used.
`Notification sound gain percent` controls only local ESP32 connection tones,
including the reconnecting chime. It does not change mic capture, sidetone, or
server playback loudness.

For local self-monitoring on the ESP32, use:

```text
Intercom ESP32 Client -> Local sidetone / self-monitor
```

`Firmware mix into playback` is the supported test mode and uses `Firmware
sidetone gain percent`. The historical `ES8388 line-bypass mixer` option is
parsed for older admin configs but forced off by the current ESP32 firmware
because it interfered with the normal DAC/server playback route during
bring-up. Rebuild and flash after changing sidetone defaults:

```sh
tools/esp32 build
tools/esp32 flash-monitor /dev/cu.usbserial-XXXX
```

See [clients/esp32/README.md](clients/esp32/README.md) for the full sidetone
bring-up sequence and safety notes.

Start the app-host desktop client:

```sh
cargo run -p app
```

Create or update an app-host settings file when you want a specific identity or
device setup:

```sh
cargo run -p app -- --init-config --user-id 1 --button director=Director
cargo run -p app
cargo run -p app -- --print-config
```

`--init-config` only creates the JSON settings file and exits. Use
`--write-config` instead when you want to save command-line overrides and keep
the app running in the same command.

Clients now have two identifiers. `user_id` is the short operator alias used in
audio packets, routing, direct calls, and admin controls. `client_uid` is a
stable UUID used for deployed-device identity and enrollment. Desktop, Pi,
bridge, and app-host clients create and reuse a local identity file by default;
use `--client-uid` for fixed lab identities or `--identity-file` to choose where
that UUID is stored. ESP32 stores its generated UUID in NVS, with an optional
menuconfig override.

Run a receive-only Pi endpoint:

```sh
cargo run -p pi -- --user-id 11 --listen-channel 1 --receive-only
```

While a desktop client is running, type line commands into its terminal:

```text
tx 2
listen 1,2
vol 2=0.6
button director down
button director up
talk off
talk on
talk-mode open
mute
unmute
show
```

The first TX channel is used as the packet channel. `mute`/`unmute` change the
regular talk mode without changing the configured route, so receive continues
while regular local transmit is disabled.

Advertise dedicated buttons and, optionally, focused-terminal hotkeys:

```sh
cargo run -p desktop -- --user-id 1 --button director=Director --button pa=PA
cargo run -p desktop -- --user-id 1 --button director=Director --button-key director=d
```

If levels are low, tune the client gain without changing the protocol:

```sh
cargo run -p desktop -- --user-id 1 --mic-gain 1.5 --speaker-gain 1.5
```

The desktop client uses a small playback jitter buffer by default. Increase it
if audio crackles on a busy machine or Wi-Fi link; set it to `0` for direct
low-latency testing:

```sh
cargo run -p desktop -- --user-id 1 --jitter-ms 60
cargo run -p desktop -- --user-id 1 --jitter-ms 0
```

Desktop/app and Pi local state include playback buffer health:
`available_samples`, `capacity_samples`, `prebuffer_samples`, `started`,
`underflows`, `overflows`, and `dropped_samples`. Rising underflows usually
mean the speaker path is starving because packets are late, missing, or decode
is falling behind. Rising overflows mean the receive buffer is filling faster
than the output device consumes it.

Desktop capture also applies a low-level de-click silence gate before packets
are sent. This is meant to suppress chopped background-noise residue from
platform voice-processing modes without changing normal speech. Client/server
processing is server-owned through the `processing` config block. Use `raw` for
external audio interfaces, `voice` for normal intercom speech,
`voice_isolation` for the strongest speech VAD/transient rejection, and
`broadcast` when preserving more room tone matters. `native_voice_processing`
tells capable clients to use OS-level voice features when available. On macOS,
that means the default-input VoiceProcessingIO path; selecting a specific input
device forces raw capture. Apple exposes Voice Isolation as a user-selected
Control Center microphone mode, not as a public programmatic setter. The desktop
local UI Stats panel reports the active/preferred macOS microphone mode and can
open Apple's microphone-mode selector so the operator can choose Voice Isolation.
Server processing is still useful for every client, but the best background
rejection happens on the client before codec/network transport. Desktop,
app-host/native app, Pi, bridge output, and ESP32 clients
play short local tones when the control connection connects or disconnects,
then play a soft reconnecting chime about every four seconds until the control
connection returns.

Inspect audio devices and select devices by case-insensitive name substring:

```sh
cargo run -p desktop -- --list-devices
cargo run -p desktop -- --user-id 1 --input-device "microphone" --output-device "headphones"
```

Run with Opus at the edges:

```sh
cargo run -p server
cargo run -p desktop -- --user-id 1 --tx-channel 1 --listen-channel 1 --codec opus --opus-profile speech-24-standard
cargo run -p desktop -- --user-id 2 --tx-channel 1 --listen-channel 1 --codec opus
cargo run -p pi -- --user-id 10 --tx-channel 1 --listen-channel 1 --codec opus
```

Opus profiles are selected separately from the `opus` codec:

- `speech-16-low`: 16 kHz, lower CPU/bandwidth speech.
- `speech-24-standard`: 24 kHz, default intercom speech and the first ESP32 Opus target.
- `speech-48-high`: 48 kHz, higher speech quality.
- `music-48`: 48 kHz higher-detail audio with Opus music signal/application hints.

Stereo is a separate listener setting and works with every Opus profile. Stereo
profiles use a higher target bitrate than mono; packet headers still identify
the packet as `opus`.

Run with high-quality PCM at the edges:

```sh
cargo run -p server
cargo run -p desktop -- --user-id 1 --tx-channel 1 --listen-channel 1 --codec pcm48
cargo run -p desktop -- --user-id 2 --tx-channel 1 --listen-channel 1 --codec pcm48
```

The server can also switch a registered desktop client between PCM16, PCM24, PCM48, and Opus at
runtime:

```sh
cargo run -p admin -- codec --user-id 1 --codec pcm
cargo run -p admin -- codec --user-id 1 --codec pcm24
cargo run -p admin -- codec --user-id 1 --codec pcm48
cargo run -p admin -- codec --user-id 1 --codec opus
```

By default the server listens on UDP `0.0.0.0:40000` for audio and TCP
`0.0.0.0:40001` for WebSocket control. It also serves the admin UI at
`http://0.0.0.0:40002/admin/` and stores desired client/channel config in
`intercom-state.json`.
New admin state is seeded with the default show channel plan from GOU-69:
Program, Production PL, Referee PL, Director IFB, Producer Cue, PA, and Utility.
Emergency is not a default channel because the server has a separate emergency
override path.

The server admin UI/API has no authentication unless `--admin-token` or
`INTERCOM_ADMIN_TOKEN` is set. Without a token, anyone who can reach the admin
bind address can control every client. Bind it to localhost, set a token, or
disable it on untrusted networks.

To test audio behavior under packet loss or jitter, run clients through the UDP
impairment proxy:

```sh
cargo run -p server
cargo run -p netem -- --listen 127.0.0.1:41000 --server 127.0.0.1:40000 --drop-percent 2 --delay-ms 20 --jitter-ms 20
cargo run -p desktop -- --user-id 1 --server 127.0.0.1:41000
cargo run -p desktop -- --user-id 2 --server 127.0.0.1:41000
```

For repeatable venue validation, latency measurements, walking tests, CPU
profiling, and the pre-demo regression checklist, use
[`docs/field-testing.md`](docs/field-testing.md).

For the current hardware path, prototype BOM, button layout, and open embedded
device decisions, use [`docs/hardware.md`](docs/hardware.md).

For the advanced routing model and Admin/Client UI direction, use
[`docs/advanced-routing-ui.md`](docs/advanced-routing-ui.md).

For recommended director, producer, talent, referee, program, PA, and IFB
workflows, use [`docs/ifb-talent-workflows.md`](docs/ifb-talent-workflows.md).

## Command Reference

### Server

```sh
cargo run -p server -- [OPTIONS]
```

Options:

- `--audio-bind <ADDR>`: UDP audio bind address. Default: `0.0.0.0:40000`.
- `--control-bind <ADDR>`: WebSocket control bind address. Default: `0.0.0.0:40001`.
- `--admin-bind <ADDR>`: HTTP admin UI/API bind address. Default: `0.0.0.0:40002`.
- `--admin-state-file <PATH>`: JSON file for desired clients and channel names. Default: `intercom-state.json`.
- `--enrollment-policy <auto|approval|preconfigured-only>`: behavior for unknown `client_uid` devices. Default: `auto`.
- `--admin-token <TOKEN>`: require HTTP authorization for `/admin/` and `/admin/api`. Can also be set with `INTERCOM_ADMIN_TOKEN`.
- `--recordings-dir <PATH>`: local folder for recording session directories. Default: `intercom-recordings`.
- `--debug-audio-dir <PATH>`: opt-in diagnostic WAV taps. Writes `server-decoded-input-user-<id>-1ch.wav` before server DSP and `server-mixed-output-user-<id>-<channels>ch.wav` before server encoding.
- `--whisper-model <PATH>`: optional local Whisper model path for built-in live transcription and recording transcription. Can also be set with `INTERCOM_WHISPER_MODEL`.
- `--whisper-model-dir <PATH>`: folder scanned by the admin UI for selectable Whisper models. Default: `intercom-models`.
- `--deepfilternet-model-dir <PATH>`: folder scanned by the admin UI for selectable DeepFilterNet models. Default: `deepfilternet-models`.
- `--transcription-engine <disabled|builtin-whisper|external-whisper>`: transcription engine. If `--whisper-model` is set and this flag is omitted, the server defaults to `builtin-whisper`.
- `--whisper-command <PATH>`: optional local Whisper/whisper.cpp command used only when `--transcription-engine external-whisper`. Can also be set with `INTERCOM_WHISPER_COMMAND`.
- `--disable-admin-ui`: disable the HTTP admin UI/API.

The admin UI is available at `/admin/` on the admin bind address. It can create
client configs before clients connect, enroll stable device UIDs, edit
listen/TX channels, assign dedicated talk buttons, configure IFB
program/interrupt ducking, set per-channel listener gains, switch codecs, set
talk mode/priority, configure per-channel priority, start/stop emergency
all-call overrides, manage channel names from the Mix Matrix card, and inspect
live server metrics, active-talker state, level meters, limiter activity, and
packet/queue health. Clicking a client row opens a modal editor with structured
controls for identity, routing, button routes, IFB, and gains.

The admin dashboard can also start/stop opt-in recording sessions and built-in
live transcription. The server taps each client's processed mono 48 kHz ingest
once before mixing, writes per-client WAV files into a session folder when
recording is active, writes `metadata.jsonl` for per-frame client/target/codec
context, and exposes recent transcript text. Recording output is one mono WAV
per client (`user-<id>.wav`), not one interleaved multichannel file; the files
can be imported as separate tracks in an editor. Built-in live
transcription uses `whisper-rs` with the configured `--whisper-model` or a model
selected from `--whisper-model-dir`, chunks each client's audio independently,
and publishes final transcript segments while people are still talking. Place
`.bin` or `.gguf` Whisper models in `intercom-models/` to make them selectable
from the Recording page. Live transcription is opt-in from the Recording page or
admin API. If transcription falls behind, the server drops transcription backlog
and reports the drop counters instead of delaying audio. Recordings and
transcripts are local files under `--recordings-dir`; enable them only where
operators and participants expect audio/text retention.

On macOS, build the server with Apple Whisper acceleration enabled:

```sh
cargo build -p server --release --features macos-accelerated
```

`macos-accelerated` currently enables `whisper-rs/metal`, so built-in live
transcription and recording transcription can use whisper.cpp's Metal path.
The Recording page and `/admin/api/state` report the compiled transcription
acceleration backend as `cpu`, `metal`, or `coreml`.

Warning: no-auth mode is for bench testing. If `--admin-token` is omitted,
anyone who can reach `--admin-bind` can control all clients and rewrite desired
config. When a token is configured, clients may authenticate with
`Authorization: Bearer <TOKEN>` or browser Basic auth using any username and
the token as the password.

Server admin HTTP API:

- `GET /admin/api/state`: returns live sessions, desired client configs, stable device enrollments, enrollment policy, client/bridge role and live bridge status, advertised/configured/active buttons, IFB config/status, stereo config/status, ESP32 audio config/status, lockout policy, emergency state, recording and live transcription status, channel names, metrics, warnings, meters, active-talker state, and packet/queue health.
- `PUT /admin/api/clients/:user_id`: replace a desired client config with `{client_uid, role, name, listen, tx, buttons, ifb, stereo, esp32_audio, processing, lockout, vol, talker_vol, codec, opus_profile, talk_mode, priority, priority_channels}`.
- `PATCH /admin/api/clients/:user_id`: update any subset of `{client_uid, role, name, listen, tx, buttons, ifb, stereo, esp32_audio, processing, lockout, vol, talker_vol, codec, opus_profile, talk_mode, priority, priority_channels}`.
- `DELETE /admin/api/clients/:user_id`: remove desired config without disconnecting the live client.
- `POST /admin/api/devices/:client_uid/approve`: approve a pending or rejected stable device identity.
- `POST /admin/api/devices/:client_uid/reject`: reject a stable device identity.
- `PUT /admin/api/devices/:client_uid` / `PATCH /admin/api/devices/:client_uid`: update device enrollment metadata such as `{user_id, name, status}`.
- `PUT /admin/api/channels/:channel_id`: create or rename a channel with `{"name":"Program"}`.
- `DELETE /admin/api/channels/:channel_id`: delete only the channel name; client routing is left unchanged.
- `PUT /admin/api/presets/:preset_id`: create or replace a preset with `{"name":"Refs","clients":[...]}` where clients are desired client configs.
- `POST /admin/api/presets/:preset_id`: apply a preset after validating all live client codec constraints.
- `DELETE /admin/api/presets/:preset_id`: delete a preset without changing desired or live client config.
- `PUT /admin/api/templates/:template_id`: create or replace a reusable client template with `{"name":"Referee","client":{...}}` where `client` has the same fields as a desired client config except `user_id`.
- `POST /admin/api/templates/:template_id/apply`: apply a template to one offline or live client with `{"user_id":12}`. Live clients receive the normal authoritative `config_update`.
- `DELETE /admin/api/templates/:template_id`: delete a template without changing desired or live client config.
- `POST /admin/api/alerts`: send a runtime call alert with `{"sender":1,"target":{"kind":"user","id":2},"message":"Call me"}` or `{"sender":1,"target":{"kind":"channel","id":4},"message":"Director calling"}`.
- `POST /admin/api/alerts/:alert_id/cancel`: cancel an active alert. Body may include `{"user_id":1}` for sender attribution.
- `POST /admin/api/announcements`: send a combined text alert and/or spoken announcement with `{"sender":0,"targets":[{"kind":"channel","id":1}],"message":"Stand by","text_alert":true,"tts":true,"priority":true,"duck":false,"gain":0.18}`. `text_alert` creates visible alert metadata; `tts` injects generated speech into the normal server mix. Spoken announcements use Supertonic as the only TTS engine. The Supertonic ONNX model assets and default voice style are embedded into the server build, so there are no OS voices, shell commands, runtime downloads, or external TTS executables to configure.
- `POST /admin/api/tts`: compatibility endpoint for spoken announcements. It behaves like `/announcements` with both `text_alert` and `tts` enabled.
- The bundled Supertonic model is distributed under its upstream OpenRAIL license. The first spoken announcement extracts the embedded model files to a local temp/cache folder so ONNX Runtime can load them, but the assets are shipped inside the server build.
- `POST /admin/api/emergency`: start or stop emergency override audio, for example `{"source":1,"active":true,"target":{"kind":"all"},"duck_gain":0.125,"mute_others":false}`. Targets may also be `{"kind":"users","users":[2,3]}` or `{"kind":"channels","channels":[9]}`.
- `POST /admin/api/recording/start`: start a recording session with `{"transcribe":true}` or `{"transcribe":false}`. Optional `users` limits recording to specific client IDs.
- `POST /admin/api/recording/stop`: stop the active recording session and launch configured recording transcription if enabled.
- `GET /admin/api/recording/status`: return current recording/transcription engine status.
- `GET /admin/api/recording/sessions`: list recent completed recording sessions with session folder, recorded users, frame count, and transcription flag.
- `POST /admin/api/transcription/live/start`: start built-in live transcription with optional `{"users":[1,2]}` filtering.
- `POST /admin/api/transcription/live/stop`: stop live transcription and flush active speech chunks.
- `GET /admin/api/transcription/live/status`: return live transcription engine, queue, drop, and per-user worker status.
- `GET /admin/api/transcription/models`: list selectable `.bin` and `.gguf` models from the configured model folder.
- `PUT /admin/api/transcription/model`: select a model from the configured model folder with `{"model":"ggml-base.en.bin"}`.
- `GET /admin/api/transcripts`: list transcript segments. Optional query parameters: `user_id`, `user_ids=1,2`, `channel_id`, `channel_ids=1,2`, `direct_user_id`, `source=live|recording|manual`, `since_ms`, `until_ms`, and `q` for text search.
- `POST /admin/api/transcripts`: append a manual/test transcript segment with `{"user_id":1,"text":"..."}`.

Enrollment policy controls what happens when a new `client_uid` connects:
`auto` enrolls it and assigns the requested numeric alias when free, otherwise
the next free alias; `approval` records it as pending and blocks client-owned
audio/control until approved; `preconfigured-only` rejects unknown devices
unless their UID is already present in server state. A known UID keeps its
assigned alias even if the client later requests a different `--user-id`.

Security notes for venue deployments:

- Prefer `--admin-token`, `--local-ui-token`, and `--local-api-token` on any shared LAN.
- Keep the WebSocket control bind on a trusted management network or localhost behind a tunnel/reverse proxy; this prototype does not terminate TLS directly.
- Put TLS, access logging, and any public exposure in front of the Rust services with a reverse proxy for now.
- Use no-auth mode only for local bench testing or physically isolated networks.

Button config shape:

```json
{
  "buttons": [
    {
      "id": "director",
      "label": "Director",
      "mode": "momentary",
      "actions": [
        {"type": "transmit", "channels": [2], "users": [12], "duck": true},
        {"type": "alert", "targets": [{"kind": "user", "id": 12}], "message": "Director calling"}
      ]
    },
    {
      "id": "pa",
      "label": "PA",
      "mode": "latching",
      "actions": [
        {"type": "transmit", "channels": [9], "users": [], "duck": false}
      ]
    }
  ]
}
```

The admin UI uses structured fields for button actions; old `tx`-only button
JSON is no longer accepted.

Processing config shape:

```json
{
  "processing": {
    "mode": "auto",
    "engine": "rnnoise",
    "profile": "voice_isolation",
    "high_pass": true,
    "noise_gate": true,
    "vad": true,
    "transient_suppression": true,
    "compressor": true,
    "presence": true,
    "native_voice_processing": true,
    "fallback_to_builtin": true,
    "deep_filter_model": null,
    "deep_filter_backend": "auto",
    "apple_compute_units": "all",
    "worker_queue_frames": 12,
    "normalization": {
      "enabled": true,
      "target_rms": 0.14,
      "max_boost": 4.0,
      "max_attenuation": 8.0,
      "adaptation_ms": 250,
      "noise_floor_rms": 0.012
    },
    "pipeline": [
      {"engine": "webrtc", "enabled": true},
      {"engine": "built_in", "enabled": true}
    ]
  }
}
```

`mode` may be `auto`, `enabled`, or `disabled`. `engine` may be `built_in`,
`webrtc`, `rnnoise`, or `deepfilternet`. `profile` may be `raw`, `voice`,
`voice_isolation`, or `broadcast`. If `pipeline` is empty, the selected
`engine` runs as a single stage. If `pipeline` is present, stages run in order;
common presets are `webrtc -> built_in` and `webrtc -> rnnoise -> built_in`.
Use `deepfilternet` only as an opt-in high-quality target. The admin UI scans
`deepfilternet-models/` for compatible ONNX `.tar.gz`/`.tgz` models and offers
them in a dropdown. The PyTorch checkpoint `.zip` packages are not runtime
models for the server backend. `deep_filter_backend` may be `auto`, `tract`, or
`coreml`; `apple_compute_units` may be `all`, `cpu_and_gpu`,
`cpu_and_neural_engine`, or `cpu_only`. The current DeepFilterNet audio runtime
uses Tract and reports a safe fallback if `coreml` is requested. Processing
status reports the active backend, inference time, model load failures, worker
timeouts, LSNR, and fallback behavior.

`normalization` is optional and defaults to disabled. When enabled, it runs
after the selected processing chain and before channel volumes, per-talker
gains, IFB/priority ducking, stereo panning, and the output limiter. Recommended
starting points:

- Laptop mic: target `0.14`, max boost `4`, max attenuation `8`, adaptation
  `250 ms`, noise floor `0.012`.
- ESP32/Ai-Thinker board mic: target `0.13`, max boost `3`, max attenuation
  `8`, adaptation `300 ms`, noise floor `0.015`.
- Headset mic: target `0.14`, max boost `3`, max attenuation `6`, adaptation
  `220 ms`, noise floor `0.010`.
- Production bridge/program input: target `0.16`, max boost `1.5`, max
  attenuation `4`, adaptation `500 ms`, noise floor `0.006`.

Client lockout policy shape:

```json
{
  "lockout": {
    "allow_channels": true,
    "allow_volumes": true,
    "allow_codec": true,
    "allow_talk_mode": true,
    "allow_priority": true,
    "allow_buttons": true,
    "allow_ifb": true,
    "allow_device_selection": true,
    "allow_local_api": true
  }
}
```

All fields default to `true`. When a field is `false`, server-side client
control messages for that setting are rejected and desktop/Pi local APIs return
HTTP `400`. Client state includes the policy so UIs can grey out locked
controls and explain that admin owns them.

IFB config shape:

```json
{"ifb":{"enabled":true,"program":[1],"interrupt":[9],"duck_gain":0.125}}
```

The default `duck_gain` is `0.125`, about -18 dB. The dashboard shows IFB as
ready or active per client and displays the current duck gain when interrupt
audio is ducking that listener's program channels.

### Desktop Client

```sh
cargo run -p desktop -- [OPTIONS] --user-id <USER_ID>
```

Options:

- `--server <ADDR>`: UDP audio server address. Default: `127.0.0.1:40000`.
- `--control <URL>`: WebSocket control URL. Default: `ws://127.0.0.1:40001`.
- `--user-id <ID>`: required numeric user ID.
- `--client-uid <UUID>`: stable client identity override. By default the client creates/reuses a UUID in the OS config directory.
- `--identity-file <PATH>`: path to the JSON file used for the generated stable client UUID.
- `--tx-channel <CHANNEL>`: initial transmit channel. Default: `1`.
- `--listen-channel <CHANNEL>`: initial listen channel. Default: `1`.
- `--codec <pcm16|pcm24|pcm48|opus>`: edge codec. Default: `pcm16`. JSON/API also accepts `pcm-24` and `pcm-48`.
- `--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>`: Opus encoder profile. Default: `speech-24-standard`.
- `--mic-gain <GAIN>`: local mic gain before encoding. Default: `1`.
- `--input-limiter`: enable a soft input limiter after local mic gain and before encoding. Off by default; useful for diagnosing or reducing loud-source clipping.
- `--speaker-gain <GAIN>`: local speaker gain after decoding. Default: `1`.
- `--jitter-ms <MS>`: playback prebuffer before audio starts. Default: `40`; allowed range: `0` to `250`.
- `--input-device <NAME_SUBSTRING>`: choose a microphone/input device by case-insensitive name substring.
- `--input-backend <auto|raw|voice-processing>`: microphone capture backend. On macOS, `auto` and `voice-processing` try Apple's VoiceProcessingIO for the default input and fall back to raw `cpal` if unavailable; `raw` keeps the portable path. Selecting an input device forces raw capture because VoiceProcessingIO only uses the default input. The local UI Stats modal shows the requested and active backend, reports the active/preferred macOS microphone mode, and includes an `Open Controls` button for Apple's microphone-mode selector when available.
- `--input-channel <average|left|right>`: channel selection/downmix for multi-channel input devices. Default: `average`.
- `--output-device <NAME_SUBSTRING>`: choose a speaker/output device by case-insensitive name substring.
- `--debug-audio-dir <PATH>`: opt-in diagnostic WAV taps. Writes `desktop-pre-gain.wav` and `desktop-post-gain.wav` at 48 kHz mono so capture quality can be compared against server taps.
- `--button <ID[=LABEL]>`: advertise a dedicated button slot to the server. May be repeated.
- `--button-key <ID=KEY>`: focused-terminal hotkey for a button. May be repeated. When enabled, the hotkey reader owns stdin and line commands are disabled.
- `--local-ui-bind <ADDR>`: local browser control UI/API bind address. Default: `127.0.0.1:41002`.
- `--local-ui-token <TOKEN>`: require HTTP authorization for the local desktop UI/API. Can also be set with `INTERCOM_LOCAL_UI_TOKEN`.
- `--disable-local-ui`: disable the local desktop browser UI/API.
- `--list-devices`: print available input/output devices and exit. Does not require `--user-id`.

Runtime commands typed into the desktop client:

- `tx <channels>`: set TX channel list, for example `tx 2` or `tx 1,2`.
- `listen <channels>`: set listen channel list, for example `listen 1,2`.
- `vol <channel=gain,...>`: set per-channel listener gains, for example `vol 2=0.6,3=0.25`.
- `talk-mode muted|ptt|open`: set the regular/default mic route mode.
- `talk on` / `talk off`: activate or release regular talk while in `ptt` mode.
- `mute` / `unmute`: set talk mode to `muted` or restore the previous non-muted mode.
- `button <id> down` / `button <id> up`: send a momentary button press/release to the server.
- `button <id> toggle`: send a latching-style press event to the server.
- `buttons`: print the current configured and active button state.
- `show`: print the client’s current local route/talk-mode/volume config.
- `help`: print the runtime command list.

Runtime updates are sent over the desktop client’s persistent WebSocket control
connection. Audio packets carry explicit v2 targets for channel, direct user, or
server mixed output. Server-pushed config updates replace the client's local
listen/TX/talk-mode/priority/volume state, so an admin can correct a route after
the client is already running.

Dedicated buttons are independent of the regular talk mode. The desktop client
transmits to all effective TX channels: regular `tx` when `talk_mode` is `open`,
or when `talk_mode` is `ptt` and regular Talk is held, plus the union of all
active button routes pushed by the server. Dedicated buttons still transmit
while regular talk is `muted`.

On startup, the desktop client sends `hello` first. If the server already has a
desired config for that user ID, the client waits for the server's
`config_update` and does not overwrite it with CLI defaults. If no desired
config exists, the CLI options seed the server-side desired config.

If the WebSocket control connection drops, the desktop and app-host clients keep
audio I/O running and reconnect automatically with exponential backoff from
500 ms up to 5 seconds. After reconnecting they send `hello` again so the
server can resume pushing authoritative config updates. If the UDP audio socket
reports transient send/receive errors while the server is down, the desktop and
Pi clients log the failure, keep running, and resume audio when the server is
reachable again. After a client has connected once, local UI/API control
requests made while the control WebSocket is reconnecting fail fast instead of
hanging indefinitely. Output-capable clients play a short disconnect tone
followed by a soft reconnecting chime about every four seconds until the control
connection is restored.
The desktop/app-host runtime starts the output stream before the first control
handshake, so the native app can also play this cue while it is waiting for an
initial server connection.

Desktop/server audio diagnostics:

```sh
cargo run -p server -- --debug-audio-dir ./debug-audio/server
cargo run -p desktop -- --user-id 1 --codec pcm48 --input-channel average --debug-audio-dir ./debug-audio/client-1
```

The desktop tap writes `desktop-pre-gain.wav` before local mic gain and
`desktop-post-gain.wav` after local mic gain, exactly as sent to the encoder.
The server tap writes `server-decoded-input-user-<id>-1ch.wav` before server DSP
and `server-mixed-output-user-<id>-<channels>ch.wav` before server encoding. Use
these files with the admin capture health meters to find whether distortion is
created by the OS/input device, local gain/encoding, server decode/processing,
or final mixing.

The desktop client also serves a local browser UI at
`http://127.0.0.1:41002/` by default. This is the first cross-platform app
surface and is intended to be reused by a later native wrapper. The layout is
optimized for a phone aspect ratio: the top bar shows the client ID plus the
server-configured name when available, stats and settings live behind modal
buttons, active listen/talk channels are shown in the main view, configured
special talk buttons sit above the bottom mute/talk controls, and all detailed
route/mix/audio/IFB settings live in the settings modal. Talk buttons are
rendered from the server-sent button config: momentary buttons are hold-to-talk,
and latching buttons toggle on click. If the requested local UI port is already
in use, the client automatically tries the next ports and logs the actual URL.
Channel rows are collapsed by default; tap a channel to fold out the live roster
for that channel. Roster rows show each present client by configured name plus
ID and mark whether that client is currently transmitting into that channel.

The local desktop UI/API has no authentication unless `--local-ui-token` or
`INTERCOM_LOCAL_UI_TOKEN` is set. The default bind is localhost; if you bind it
to a LAN address without a token, anyone who can reach that port can control
that running desktop client. Token auth accepts `Authorization: Bearer <TOKEN>`
or browser Basic auth using any username and the token as the password.

Desktop local HTTP API:

- `GET /health`: returns `{"ok":true}`.
- `GET /state`: returns `{user_id, client_uid, name, listen, tx, vol, talker_vol, codec, opus_profile, talk_mode, regular_talk_active, priority, priority_channels, emergency, ifb, processing, channel_rosters, mic_gain, speaker_gain, playback, supported_codecs, advertised_buttons, buttons, active_buttons, active_alerts, recent_alerts}`.
- `PUT /config`: sends full config through the server first, for example `{"listen":[1,2],"tx":[1],"vol":{"2":0.6},"talker_vol":{"12":0.8},"codec":"opus","opus_profile":"speech_48_high","talk_mode":"ptt","priority":false,"priority_channels":[],"ifb":{"enabled":true,"program":[1],"interrupt":[9],"duck_gain":0.125}}`.
- `POST /talk-mode`: body `{"mode":"muted"}`, `{"mode":"ptt"}`, or `{"mode":"open"}`.
- `POST /talk/down`, `/talk/up`, `/talk/toggle`: activate, release, or toggle regular Talk.
- `POST /mute`: asks the server to set `talk_mode` to `muted`.
- `POST /unmute`: asks the server to restore the previous non-muted `talk_mode`.
- `POST /codec`: body `{"codec":"pcm16"}`, `{"codec":"pcm48"}`, or `{"codec":"opus"}`. Use full `/config` or admin UI/API to change `opus_profile`.
- `POST /gain`: body `{"mic_gain":1.5,"speaker_gain":1.2}`; values are applied locally without a server round trip.
- `POST /buttons/:id/down`, `/buttons/:id/up`, `/buttons/:id/toggle`: sends button events to the server.
- `POST /alerts`: sends a call alert. Body: `{"target":{"kind":"user","id":2},"message":"Call me"}` or `{"target":{"kind":"channel","id":4},"message":"Ready?"}`.
- `POST /alerts/:id/ack`: acknowledges one active alert for this client.
- `POST /alerts/:id/cancel`: asks the server to cancel one active alert.

### App Host

```sh
cargo run -p app -- [OPTIONS]
```

The app host is the first native-app boundary. It launches the same desktop
audio/control runtime, serves the same local browser UI, and adds a persistent
JSON settings file. The default wrapper mode opens the local operator UI in the
platform browser while keeping all audio/control logic in the existing desktop
runtime.

For a development Tauri app window, use the optional native feature:

```sh
cargo run -p app --features native --bin app-native -- --user-id 1
```

This starts the same desktop runtime in the background and opens the operator UI
inside a Tauri OS webview window sized for the phone-oriented client UI. Tauri
loads the running local client UI URL after the app host has picked an available
local port, so the native shell still reuses the existing Rust audio/control
runtime instead of duplicating it.

The Tauri app also creates a tray/menu icon with quick operator controls:

- `Open Operator Window`: show and focus the client window.
- `Refresh Status`: read `/state` from the local client API and update the tray tooltip.
- `Mute` / `Unmute`: call the local `/mute` and `/unmute` endpoints.
- `Start Talk` / `Stop Talk`: call `/talk/down` and `/talk/up` for the regular PTT route.
- `Quit`: exit the native app.

If `--local-ui-token` is set, tray controls send it as a bearer token to the
local API.

Without a settings file, the app starts with user ID `1`, channel `1`, PCM16,
and opens the local browser UI at `http://127.0.0.1:41002/`. If that port is
already in use, the app tries the next available port. Use `--user-id` or the
settings file for unique identities when running more than one client.

Options:

- `--config-file <PATH>`: app settings JSON path. Default: `intercom-app-settings.json`.
- `--init-config`: write the effective settings file, print the path, and exit.
- `--print-config`: print the effective settings after file load and CLI overrides, then exit.
- `--print-launch-plan`: print the app launch plan after settings resolution, then exit.
- `--write-config`: write the effective settings file before starting the client.
- `--server <ADDR>`: UDP audio server address. Overrides the settings file.
- `--control <URL>`: WebSocket control URL. Overrides the settings file.
- `--user-id <ID>`: numeric user ID. Default: `1`; overrides the settings file.
- `--client-uid <UUID>`: stable client identity override; otherwise the shared client identity file is used.
- `--identity-file <PATH>`: path to the generated stable client UUID file.
- `--tx-channel <CHANNEL>`: initial transmit channel. Overrides the settings file.
- `--listen-channel <CHANNEL>`: initial listen channel. Overrides the settings file.
- `--codec <pcm16|pcm24|pcm48|opus>`: edge codec. Overrides the settings file.
- `--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>`: Opus profile. Overrides the settings file.
- `--mic-gain <GAIN>` and `--speaker-gain <GAIN>`: local gain settings.
- `--input-limiter`: enable the optional local soft input limiter.
- `--jitter-ms <MS>`: playback prebuffer, `0` to `250`.
- `--input-device <NAME_SUBSTRING>` and `--output-device <NAME_SUBSTRING>`: audio device selectors.
- `--input-backend <auto|raw|voice-processing>`: microphone capture backend. On macOS, `auto` and `voice-processing` use Apple VoiceProcessingIO when possible for the default input and report any raw fallback in the client Stats modal.
- `--input-channel <average|left|right>`: input channel selection/downmix.
- `--debug-audio-dir <PATH>`: opt-in client-side WAV diagnostics.
- `--button <ID[=LABEL]>`: advertised dedicated button slot. May be repeated; replaces file button slots when supplied.
- `--button-key <ID=KEY>`: focused-terminal hotkey. May be repeated; replaces file hotkeys when supplied.
- `--local-ui-bind <ADDR>`: local browser UI/API bind address.
- `--local-ui-token <TOKEN>`: require HTTP authorization for the local browser UI/API.
- `--disable-local-ui`: disable the local browser UI/API.
- `--enable-local-ui`: re-enable the local browser UI/API when the settings file disables it.
- `--window-mode <system-browser|native|disabled>`: open the local operator UI in the platform browser, open it in the Tauri window, or run terminal-only.
- `--app-title <TITLE>`: title metadata for the native/app launch plan.
- `--ui-open-delay-ms <MS>`: delay before opening the local UI, `0` to `30000`. Default: `750`.
- `--list-devices`: print audio devices and exit.

Settings file shape:

```json
{
  "app_title": "Intercom Suite",
  "server": "127.0.0.1:40000",
  "control": "ws://127.0.0.1:40001",
  "user_id": 1,
  "tx_channel": 1,
  "listen_channel": 1,
  "codec": "pcm48",
  "opus_profile": "speech_24_standard",
  "mic_gain": 1.25,
  "speaker_gain": 1.5,
  "jitter_ms": 40,
  "input_backend": "auto",
  "input_device": "microphone",
  "output_device": "headphones",
  "buttons": ["director=Director", "pa=PA"],
  "button_keys": ["director=d"],
  "local_ui_bind": "127.0.0.1:41002",
  "local_ui_token": null,
  "disable_local_ui": false,
  "window_mode": "system_browser",
  "ui_open_delay_ms": 750
}
```

Command-line values override the JSON file. `--write-config` is useful after
tuning devices, gains, codec, or button slots from the command line.
The app validates settings before saving or launching: `jitter_ms` must be
`0..250`, local gains must be finite values from `0` to `8`, the control URL
cannot be empty, and button strings must match the same syntax as the desktop
client. Saving is atomic and creates parent directories when needed, so a native
settings window can safely write the same file later.

Use `--window-mode disabled` when you want the app host to behave like the
terminal client and avoid opening any UI. `--print-launch-plan` is intended for
future native wrappers and smoke tests; it reports the chosen UI URL and whether
the app would open a window without starting audio.

Development Tauri app commands:

```sh
cargo run -p app --features native --bin app-native -- --user-id 1
cargo run -p app --features native --bin app-native -- --user-id 2 --local-ui-bind 127.0.0.1:41003
cargo build -p app --features native --bin app-native --release
clients/app/scripts/package-native.sh
```

The native binary defaults to `window_mode = "native"` and creates the tray/menu
controls described above. Passing
`--window-mode system-browser` falls back to the browser-launch behavior, and
`--window-mode disabled` runs without a window. Packaged installers are still
created through Tauri when the platform supports the requested bundle target.
`app-native` is the package default run target for Tauri CLI compatibility;
without the `native` feature it delegates to the browser app-host behavior.
See [Native App](docs/native-app.md) for packaging prerequisites, lifecycle
behavior, and settings-window details. The iOS/Android Tauri mobile shell and
permissions are covered in [Mobile Tauri Clients](docs/mobile-clients.md).

### Audio Bridge Client

```sh
cargo run -p bridge -- --user-id 90 --name "vMix Program" --mode input --tx-channels 20 --input-device "BlackHole" --codec pcm48
cargo run -p bridge -- --user-id 91 --name "PA Output" --mode output --listen-channels 30 --output-device "USB Audio" --codec pcm48
cargo run -p bridge-app
cargo run -p bridge-app --features native --bin bridge-app-native
```

The bridge is a headless production-audio endpoint for PA, USB interfaces,
virtual audio cables, and vMix-style workflows. It connects to the same UDP
audio and WebSocket control plane as other clients, advertises itself with role
`bridge`, and can be managed from the admin UI/API like a normal client.

`bridge-app` is the cross-platform multi-route launcher for production
machines. It persists `intercom-bridge-app.json` and starts one `bridge` process
per configured route. The normal command opens the manager in a local browser UI
at `127.0.0.1:41012`; the `bridge-app-native` command opens the same manager in
a Tauri desktop window and stops launched routes when the window closes. Use it
on a Windows vMix PC or venue audio machine when you need several simultaneous
routes, such as program input, PA output, and a recorder feed. See
[Bridge App](docs/bridge-app.md) for the full setup.
The bridge app route editor uses local audio-device dropdowns and server-channel
dropdowns. It derives the admin API from the control host by default; pass
`--admin http://SERVER:40002` when the admin API is elsewhere.

Package the native bridge launcher on the target OS with:

```sh
clients/bridge-app/scripts/package-native.sh
```

Options:

- `--mode <input|output|duplex>`: capture into Intercom, play out of Intercom, or both. Default: `duplex`.
- `--user-id <ID>`: requested numeric alias for the bridge.
- `--client-uid <UUID>`: stable bridge identity override. By default the bridge creates/reuses a UUID in the OS config directory.
- `--identity-file <PATH>`: path to the JSON file used for the generated stable bridge UUID.
- `--tx-channels <CSV>`: channels fed by the selected input device. Default: `1`.
- `--listen-channels <CSV>`: channels rendered to the selected output device. Default: `1`.
- `--input-device <NAME_SUBSTRING>` / `--output-device <NAME_SUBSTRING>`: choose audio devices by case-insensitive name substring.
- `--input-gain <GAIN>` / `--output-gain <GAIN>`: bridge-local linear gain before sending or playing audio. Default: `1.0`.
- `--note <TEXT>`: optional operator note shown in admin bridge status, for example the attached interface or vMix bus.
- `--codec <pcm16|pcm24|pcm48|opus>` and `--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>`: edge codec settings. Default bridge codec is `pcm48`.
- `--stereo`: request stereo receive for output/duplex bridges when the server config and codec allow it.
- `--list-devices`: print bridge-visible input/output devices.

Live bridge status is reported to `/admin/api/state` and the admin Clients/System
pages as `bridge`: mode, selected input/output device names, listen/TX routes,
local gains, and the optional note. Input and output meters still come from the
normal session health fields. The server warns if a bridge listens and transmits
on the same channel because that can feed PA/program audio back into itself.

Common setups:

- vMix input into Intercom: route vMix or a virtual audio cable to a bridge `input` device and set `--tx-channels` to the program channel.
- Intercom to PA: run an `output` bridge listening to a PA channel and select the USB/audio-interface output feeding the PA chain.
- Two-way production bridge: use `duplex` only when the physical interface has separate input/output paths and feedback is controlled externally.
- Multi-route vMix PC: run `cargo run -p bridge-app --features native --bin bridge-app-native`, add one `input` route for the vMix/program bus, one `output` route for PA or production monitor, and optional extra `output` routes for recorder/stream feeds.

### Pi Client

```sh
cargo run -p pi -- [OPTIONS] --user-id <USER_ID>
```

Options:

- `--server <ADDR>`: UDP audio server address. Default: `127.0.0.1:40000`.
- `--control <URL>`: WebSocket control URL. Default: `ws://127.0.0.1:40001`.
- `--user-id <ID>`: required numeric user ID.
- `--client-uid <UUID>`: stable Pi identity override. By default the Pi client creates/reuses a UUID in the OS config directory.
- `--identity-file <PATH>`: path to the JSON file used for the generated stable Pi UUID.
- `--tx-channel <CHANNEL>`: initial transmit channel. Default: `1`.
- `--listen-channel <CHANNEL>`: initial listen channel. Default: `1`.
- `--codec <pcm16|pcm24|pcm48|opus>`: edge codec. Default: `pcm16`. JSON/API also accepts `pcm-24` and `pcm-48`.
- `--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>`: Opus encoder profile. Default: `speech-24-standard`.
- `--mic-gain <GAIN>`: local mic gain before encoding. Default: `1`.
- `--speaker-gain <GAIN>`: local speaker gain after decoding. Default: `1`.
- `--jitter-ms <MS>`: playback prebuffer before audio starts. Default: `40`; allowed range: `0` to `250`.
- `--input-device <NAME_SUBSTRING>`: choose a microphone/input device by case-insensitive name substring.
- `--output-device <NAME_SUBSTRING>`: choose a speaker/output device by case-insensitive name substring.
- `--receive-only`: start with no TX channels and `talk_mode` set to `muted`. The server can still push TX/talk-mode config later.
- `--button <ID[=LABEL]>`: advertise a dedicated button slot to the server. May be repeated.
- `--local-api-bind <ADDR>`: local HTTP control API bind address. Default: `0.0.0.0:41001`.
- `--local-api-token <TOKEN>`: require HTTP authorization for the Pi local API. Can also be set with `INTERCOM_LOCAL_API_TOKEN`.
- `--disable-local-api`: disable the local HTTP control API.
- `--list-devices`: print available input/output devices and exit. Does not require `--user-id`.

The Pi client is intended for unattended/headless use: it has no stdin command
loop. Configure it at startup or later through the server/admin control plane.
It sends `hello` with its supported codecs, receives server-pushed config
updates, and applies remote listen/TX/talk-mode/priority/volume/codec changes while
running.

Like the desktop client, the Pi sends `hello` before startup config. A
preconfigured server-side desired config wins over the Pi's CLI defaults.

If the WebSocket control connection drops, the Pi client keeps audio I/O and the
local API process running and reconnects automatically with exponential backoff
from 500 ms up to 5 seconds. After reconnecting it sends `hello` again so the
server can resume pushing authoritative config updates. Output-capable clients
play a short disconnect tone followed by a soft repeating reconnecting chime
until the control connection is restored.

The Pi local HTTP API binds to the LAN by default and has no authentication
unless `--local-api-token` or `INTERCOM_LOCAL_API_TOKEN` is set. Anyone who can
reach the bind address can control the Pi client when no token is configured.
Use a token, `--disable-local-api`, or bind to `127.0.0.1:41001` if the network
is not trusted. If the requested local API port is already in use, the Pi client
automatically tries the next ports and logs the actual bind address.

Pi local HTTP API:

- `GET /health`: returns `{"ok":true}`.
- `GET /state`: returns `{user_id, client_uid, name, listen, tx, vol, talker_vol, codec, opus_profile, talk_mode, regular_talk_active, priority, priority_channels, emergency, ifb, processing, channel_rosters, playback, supported_codecs, advertised_buttons, buttons, active_buttons, active_alerts, recent_alerts}`.
- `PUT /config`: full config body, for example `{"listen":[1,2],"tx":[1],"vol":{"2":0.6},"talker_vol":{"12":0.8},"codec":"opus","opus_profile":"speech_24_standard","talk_mode":"ptt","priority":false,"priority_channels":[],"ifb":{"enabled":false}}`.
- `POST /talk-mode`: body `{"mode":"muted"}`, `{"mode":"ptt"}`, or `{"mode":"open"}`.
- `POST /talk/down`, `/talk/up`, `/talk/toggle`: activate, release, or toggle regular Talk.
- `POST /mute`: asks the server to set `talk_mode` to `muted`.
- `POST /unmute`: asks the server to restore the previous non-muted `talk_mode`.
- `POST /codec`: body `{"codec":"pcm16"}`, `{"codec":"pcm48"}`, or `{"codec":"opus"}`. Use full `/config` or admin UI/API to change `opus_profile`.
- `POST /buttons/:id/down`: sends button `pressed=true` to the server.
- `POST /buttons/:id/up`: sends button `pressed=false` to the server.
- `POST /buttons/:id/toggle`: sends a latching-style button press to the server.
- `POST /alerts`: sends a call alert. Body: `{"target":{"kind":"user","id":2},"message":"Call me"}` or `{"target":{"kind":"channel","id":4},"message":"Ready?"}`.
- `POST /alerts/:id/ack`: acknowledges one active alert for this Pi client.
- `POST /alerts/:id/cancel`: asks the server to cancel one active alert.

The local API submits control changes to the server first. The Pi's live runtime
state changes only after the server replies and pushes back an authoritative
`config_update`.

### Pi GPIO Companion

```sh
cargo run -p pi-gpio -- [OPTIONS]
```

The GPIO companion is a small process that polls Linux GPIO value files and
calls the Pi client's local HTTP API. GPIO support intentionally uses the same
local API as future UI tools, so there is still only one control path into the
running Pi client.

Options:

- `--config-file <PATH>`: GPIO mapping JSON path. Default: `pi-buttons.json`.
- `--local-api <URL>`: Pi local API base URL. Default: `http://127.0.0.1:41001`.
- `--local-api-token <TOKEN>`: send `Authorization: Bearer <TOKEN>` to the Pi local API. Can also be set with `INTERCOM_LOCAL_API_TOKEN`.
- `--gpio-root <PATH>`: GPIO sysfs root. Default: `/sys/class/gpio`.
- `--init-config`: write an example mapping and exit.
- `--dry-run`: log button events instead of sending HTTP requests.

Example config:

```json
{
  "debounce_ms": 30,
  "poll_ms": 20,
  "buttons": [
    {
      "name": "regular-talk",
      "gpio": 17,
      "active_low": true,
      "mode": "momentary",
      "action": { "type": "regular_talk" }
    },
    {
      "name": "director",
      "gpio": 27,
      "active_low": true,
      "mode": "momentary",
      "action": { "type": "talk_button", "button_id": "director" }
    }
  ]
}
```

Momentary regular talk sends `/talk/down` on press and `/talk/up` on release.
Latching regular talk sends `/talk/toggle` on press. Momentary talk buttons send
`/buttons/:id/down` and `/buttons/:id/up`; latching talk buttons send
`/buttons/:id/toggle` on press.

### Admin Control

Inspect connected/configured users and server counters:

```sh
cargo run -p admin -- status
```

Update a user's route live:

```sh
cargo run -p admin -- config --user-id 1 --listen 1,2 --tx 1 --vol 2=0.6
cargo run -p admin -- config --user-id 1 --listen 1,2 --tx 1 --vol 2=0.6 --codec opus --opus-profile speech-48-high
cargo run -p admin -- config --user-id 1 --processing-engine rnnoise --processing-mode enabled --processing-profile voice-isolation
cargo run -p admin -- config --user-id 1 --processing-mode enabled --processing-profile voice-isolation --processing-pipeline webrtc,built-in
cargo run -p admin -- config --user-id 1 --processing-engine deepfilternet --processing-mode enabled --processing-profile voice-isolation --deep-filter-model deepfilternet-models/DeepFilterNet3_onnx.tar.gz --deep-filter-backend auto --apple-compute-units all
cargo run -p admin -- config --user-id 20 --listen 1,9 --tx 1 --ifb-enabled true --ifb-program 1 --ifb-interrupt 9 --ifb-duck-gain 0.125
cargo run -p admin -- config --user-id 2 --listen 1,2 --tx 2 --priority-channels 2
```

Processing flags update the server-owned processing object without requiring
raw JSON. `--processing-engine` accepts `built-in`, `webrtc`, `rnnoise`, or
`deepfilternet`; `--processing-profile` accepts `raw`, `voice`,
`voice-isolation`, or `broadcast`; `--processing-pipeline` accepts a comma list
such as `webrtc,built-in` or `webrtc,rnnoise,built-in`; `--deep-filter-model`
sets the DeepFilterNet model path directly; `--deep-filter-backend` accepts
`auto`, `tract`, or `coreml`; `--apple-compute-units` accepts `all`,
`cpu-and-gpu`, `cpu-and-neural-engine`, or `cpu-only`.

Switch only a user's edge codec:

```sh
cargo run -p admin -- codec --user-id 1 --codec pcm
cargo run -p admin -- codec --user-id 1 --codec opus
```

Set a user's regular talk mode or transient regular Talk state:

```sh
cargo run -p admin -- talk-mode --user-id 1 --mode muted
cargo run -p admin -- talk-mode --user-id 1 --mode ptt
cargo run -p admin -- talk --user-id 1 --active true
cargo run -p admin -- talk --user-id 1 --active false
```

Toggle priority ducking for a user:

```sh
cargo run -p admin -- priority --user-id 1 --active true
cargo run -p admin -- priority --user-id 1 --active false
```

Start or stop emergency override audio:

```sh
cargo run -p admin -- emergency --user-id 1 --active true --target all --duck-gain 0.125
cargo run -p admin -- emergency --user-id 1 --active true --target users:2,3 --mute-others true
cargo run -p admin -- emergency --user-id 1 --active false
```

`listen` controls which channels the user hears. `tx` controls which channels
their regular/default talk route uses. In the admin mix matrix, the `listen`
checkbox controls receive routing, the `regular TX` checkbox controls regular TX routing, and
the number is the per-channel listener gain. `vol` stores that gain.
`talker_vol` applies per-listener/per-talker gain after channel gain and before
priority/IFB ducking; user IDs map to gain values, for example
`"talker_vol":{"12":0.8}` lowers user 12 in that listener's mix.
`codec` controls whether the client sends/receives PCM16, 24 kHz PCM, 48 kHz
PCM, or Opus edge audio. `opus_profile` controls Opus sample rate,
quality/CPU/bandwidth, and is ignored by PCM codecs.
`talk_mode` controls the regular mic route: `muted` blocks regular TX, `ptt`
requires regular Talk down/up, and `open` transmits regular TX continuously.
Dedicated buttons bypass `muted` for their configured routes. `priority`
enables ducking behavior for that user, and `priority_channels` scopes that
ducking to specific channels. Priority on channel 2 does not duck unrelated
audio on channel 1.
Emergency override is source-side: when emergency is active, the source client
sends its mic as a server-mixed emergency stream. Recipients are all users,
specific users, or live listeners of specific channels; their normal audio is
either ducked by `duck_gain` or muted completely when `mute_others` is true.
IFB is listener-side: set `--ifb-program` to the channels the client should
normally hear, and `--ifb-interrupt` to the director/referee interrupt channels
that should duck those program channels for that client.
Stereo receive is listener-side too. Set `"stereo":{"enabled":true,"channel_pan":{"1":-1,"9":1}}`
on a client/template and use `pcm48` or `opus` to hear channel 1 left and
channel 9 right. Center/default pan is `0`; stereo config remains visible but
inactive for PCM16/PCM24 and the admin dashboard reports a warning.

Admin CLI updates are persisted as desired server config. If the user is
offline, the config is staged and applied when that user ID later connects. If a
desired Opus codec is staged while the client is offline but the connected
client later advertises only PCM16, the active session falls back to PCM16 and
the admin API reports a warning.

Admin CLI:

```sh
cargo run -p admin -- [OPTIONS] <COMMAND>
```

Global options:

- `--control <URL>`: WebSocket control URL. Default: `ws://127.0.0.1:40001`.

Commands:

- `status`: print server counters plus all known sessions. Session rows include current source queue depth, priority channels, emergency status, and IFB config/status.
- `field-report [--min-sessions <N>] [--max-queue-depth <N>] [--max-age-ms <MS>] [--require-audio] [--json]`: print a field-test health snapshot and exit non-zero when basic pass criteria fail.
- `config --user-id <ID> --listen <channels> --tx <channels> [--vol <channel=gain,...>] [--codec <pcm|pcm16|pcm24|pcm48|opus>] [--opus-profile <speech-16-low|speech-24-standard|speech-48-high|music-48>] [--talk-mode <muted|ptt|open>] [--priority-channels <channels>] [--ifb-enabled <true|false>] [--ifb-program <channels>] [--ifb-interrupt <channels>] [--ifb-duck-gain <float>]`
- `codec --user-id <ID> --codec <pcm|pcm16|pcm24|pcm48|opus>`
- `talk-mode --user-id <ID> --mode <muted|ptt|open>`
- `talk --user-id <ID> --active <true|false>`
- `priority --user-id <ID> --active <true|false>`
- `emergency --user-id <ID> --active <true|false> [--target <all|users:1,2|channels:1,2>] [--duck-gain <float>] [--mute-others <true|false>]`

### UDP Impairment Proxy

```sh
cargo run -p netem -- [OPTIONS]
```

Options:

- `--listen <ADDR>`: UDP address clients send to. Default: `127.0.0.1:41000`.
- `--server <ADDR>`: real UDP audio server address. Default: `127.0.0.1:40000`.
- `--drop-percent <PERCENT>`: random packet loss applied in both directions. Default: `0`; allowed range: `0` to `100`.
- `--delay-ms <MS>`: fixed per-packet delay applied in both directions. Default: `0`; allowed range: `0` to `1000`.
- `--jitter-ms <MS>`: random per-packet delay variation around `--delay-ms`, clamped at `0`, applied in both directions. Use `--delay-ms 20 --jitter-ms 20` for roughly `0..40 ms` delay, equivalent to `20 ms +/- 20 ms`. Default: `0`; allowed range: `0` to `1000`.
- `--seed <N>`: deterministic pseudo-random seed. Default: `470674607`.
- `--stats-interval-ms <MS>`: log packet counters at this interval. Set `0` to disable. Default: `5000`.

The proxy creates one server-facing UDP socket per client address, so multiple
desktop clients can share the same proxy without collapsing into a single server
session.

### Control Messages

Control messages are JSON over WebSocket. Supported messages:

```json
{"type":"hello","requested_user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","codecs":["pcm16","pcm24","pcm48","opus"],"buttons":[{"id":"director","label":"Director"},{"id":"pa","label":"PA"}]}
```

```json
{"type":"config","user_id":1,"listen":[1,2],"tx":[1],"vol":{"2":0.6},"codec":"opus","opus_profile":"speech_48_high","talk_mode":"ptt","priority_channels":[1],"processing":{"mode":"auto","engine":"rnnoise","profile":"voice_isolation","high_pass":true,"noise_gate":true,"vad":true,"transient_suppression":true,"compressor":true,"presence":true,"native_voice_processing":true,"fallback_to_builtin":true,"deep_filter_model":null,"deep_filter_backend":"auto","apple_compute_units":"all","worker_queue_frames":12},"esp32_audio":{"enabled":true,"adc_input":"difference","mic_pga_gain_db":9,"capture_channel":"left","mic_software_gain_percent":100,"speaker_software_gain_percent":100,"notification_gain_percent":50,"high_pass_enabled":true,"sidetone":{"mode":"off","firmware_gain_percent":25,"codec_bypass_gain_percent":25,"mic_bypass_gain_percent":100}}}
```

```json
{"type":"audio_codec","user_id":1,"codec":"pcm48"}
```

```json
{"type":"capture_health","user_id":50,"health":{"codec_config":{"chip":"es8388","active_codec":"pcm48","server_control_enabled":true,"audio_backend":"legacy_i2s_es8388","adc_input":"difference","mic_pga_gain_db":9,"capture_channel":"left","mic_software_gain_percent":100,"speaker_software_gain_percent":100,"notification_gain_percent":50,"high_pass_enabled":true,"hardware_sample_rate_hz":48000,"hardware_channels":2,"hardware_bits_per_sample":16,"i2s_sample_rate_hz":48000,"i2s_format":"philips","i2s_slot_width":"16","sidetone":{"mode":"off","firmware_gain_percent":0,"codec_bypass_gain_percent":25,"mic_bypass_gain_percent":100,"active_bypass_source":"none","codec_bypass_preserves_dac":true}},"adc_input":"difference","mic_pga_gain_db":9,"capture_channel":"left","software_gain_percent":100,"high_pass_enabled":true,"playback_queue_depth":2,"tx_target_count":1,"tx_packets_sent":120,"tx_send_failures":0,"left":{"rms":0.08,"peak":0.22,"dc_offset":0.01},"right":{"rms":0.01,"peak":0.03,"dc_offset":0.0},"selected":{"rms":0.08,"peak":0.22,"dc_offset":0.0},"raw_clipped_samples":0,"software_clipped_samples":0}}
```

```json
{"type":"talk_mode","user_id":1,"mode":"open"}
```

```json
{"type":"talk","user_id":1,"active":true}
```

```json
{"type":"priority","user_id":1,"active":true}
```

```json
{"type":"emergency","user_id":1,"active":true,"target":{"kind":"all"},"duck_gain":0.125,"mute_others":false}
```

```json
{"type":"button","user_id":1,"button_id":"director","pressed":true}
```

```json
{"type":"ping","user_id":1}
```

```json
{"type":"status"}
```

Clients send `hello` on startup to enroll their stable `client_uid`, receive
their assigned numeric `user_id`, mark the persistent WebSocket as the
client-owned control connection, and advertise the edge codecs they can
actually encode/decode. Admin tools do not need to send `hello`. If `codecs` is
omitted, the server assumes `["pcm16"]`. A `hello` response with
`preconfigured:true` means the server already has desired config for that UID or
assigned user and will push it as `config_update`; clients should not send their
startup defaults afterward. A pending/rejected enrollment must not transmit
audio until the admin approves it.

Admin `config`, `audio_codec`, `talk_mode`, `priority`, and `emergency`
messages update the server's desired/live routing state. `config` can also
carry IFB, stereo, and per-channel priority settings. Client `talk` and
`button` messages update only live transient talk/button state. Button
assignments are configured through the admin HTTP API/UI.

Responses:

```json
{"type":"hello","user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","enrollment":"enrolled","preconfigured":true}
```

```json
{"type":"ack"}
```

```json
{"type":"error","message":"..."}
```

```json
{"type":"status","sessions":[{"user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","enrollment":"enrolled","addr":"127.0.0.1:50000","listen":[1],"tx":[1],"codec":"pcm48","opus_profile":"speech_24_standard","supported_codecs":["pcm16","pcm24","pcm48","opus"],"advertised_buttons":[{"id":"director","label":"Director"}],"buttons":[{"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[2],"users":[],"duck":false}]}],"active_buttons":[],"active_alerts":[],"recent_alerts":[],"emergency":null,"ifb":{"enabled":true,"program":[1],"interrupt":[2],"duck_gain":0.125},"ifb_status":{"active":false,"duck_gain":1.0},"stereo":{"enabled":true,"channel_pan":{"1":-1}},"stereo_status":{"active":true,"channels":2,"warning":null},"processing":{"mode":"auto","engine":"rnnoise","profile":"voice_isolation","high_pass":true,"noise_gate":true,"vad":true,"transient_suppression":true,"compressor":true,"presence":true,"fallback_to_builtin":true,"deep_filter_backend":"auto","apple_compute_units":"all","worker_queue_frames":12},"processing_status":{"active":true,"bypassed":false,"gate_open":true,"engine":"rnnoise","engine_available":true,"engine_detail":"RNNoise VAD 0.82","input_rms":0.06,"output_rms":0.04,"gain_reduction_db":-2.5},"talk_mode":"ptt","regular_talk_active":true,"priority":false,"priority_channels":[1],"queue_depth":1,"age_ms":12,"input":{"active":true,"peak":0.25,"rms":0.12,"last_channel":1,"last_packet_age_ms":8},"output":{"peak":0.4,"rms":0.18,"limiter_gain":1.0,"limiter_reduction_db":0.0,"limiter_events":0},"capture":{"adc_input":"difference","mic_pga_gain_db":9,"capture_channel":"left","software_gain_percent":100,"high_pass_enabled":true,"left":{"rms":0.08,"peak":0.22,"dc_offset":0.01},"right":{"rms":0.01,"peak":0.03,"dc_offset":0.0},"selected":{"rms":0.08,"peak":0.22,"dc_offset":0.0},"raw_clipped_samples":0,"software_clipped_samples":0},"transport":{"source_queue_depth":1,"source_frames_dropped":0,"decode_errors":0}}],"active_alerts":[],"recent_alerts":[],"emergency":null,"metrics":{"audio_packets_received":20,"malformed_packets_dropped":0,"audio_decode_errors":0,"audio_frames_decoded":20,"source_frames_enqueued":20,"source_frames_dropped":0,"expired_source_queues":0,"mixed_packets_sent":18,"audio_encode_errors":0,"control_messages_received":3}}
```

The server rejects `audio_codec` / `config.codec` changes if either the server
build or that specific registered client does not support the requested codec.

Server-to-client events:

```json
{"type":"config_update","user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","name":"Ref 1","listen":[1,2],"tx":[1],"vol":{"2":0.6},"codec":"opus","opus_profile":"speech_48_high","talk_mode":"ptt","regular_talk_active":true,"priority":false,"priority_channels":[1],"processing":{"mode":"auto","engine":"rnnoise","profile":"voice_isolation","high_pass":true,"noise_gate":true,"vad":true,"transient_suppression":true,"compressor":true,"presence":true,"fallback_to_builtin":true,"deep_filter_backend":"auto","apple_compute_units":"all","worker_queue_frames":12},"buttons":[{"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[2],"users":[],"duck":false}]}],"active_buttons":[],"active_alerts":[],"recent_alerts":[],"emergency":null,"ifb":{"enabled":true,"program":[1],"interrupt":[2],"duck_gain":0.125},"stereo":{"enabled":true,"channel_pan":{"1":-1,"2":1}}}
```

```json
{"type":"presence_update","user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","channels":[{"channel_id":1,"members":[{"user_id":1,"name":"Ref 1","present":true,"transmitting":false},{"user_id":2,"name":"Director","present":true,"transmitting":true}]}]}
```

The server sends `config_update` to a registered desktop client whenever that
user's `config`, `audio_codec`, `talk_mode`, transient Talk, `priority`, or active button state
changes. The desktop and Pi clients apply the update locally, including codec,
regular TX channels, configured button routes, and active button IDs used for
outbound UDP audio.
The server also sends periodic `presence_update` events with foldout channel
rosters for the channels visible to that client. Presence is derived from live
listen routes, regular TX routes, and configured/active button transmit routes;
`transmitting` is true when recent input is active and the effective TX targets
include that channel.

Status metrics:

- `audio_packets_received`: valid audio packets accepted by the server.
- `malformed_packets_dropped`: packets rejected before audio decode.
- `audio_decode_errors`: packets with unsupported/invalid audio payloads.
- `audio_frames_decoded`: payloads successfully decoded to PCM frames.
- `source_frames_enqueued`: PCM frames added to source queues.
- `source_frames_dropped`: old queued frames dropped because a source queue was full.
- `expired_source_queues`: inactive source queues removed by the mixer.
- `mixed_packets_sent`: mixed output packets sent to listeners.
- `audio_encode_errors`: failures while encoding mixed output.
- `audio_send_errors`: UDP send failures while delivering mixed output. The
  server clears the stale audio endpoint and waits for the client to
  re-register instead of exiting.
- `control_messages_received`: WebSocket control messages received.

Per-session health fields:

- `input`: last input peak/RMS, active-talker flag, last channel, and age of the last input packet.
- `output`: last output peak/RMS plus limiter gain, limiter reduction in dB, and limiter event count.
- `transport`: current source queue depth plus attributable source frame drops and decode errors.
- `processing`: processing config and current processing status, including
  engine, backend availability, fallback detail, bypass state, gate state,
  input/output RMS, and gain reduction.

## Test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
