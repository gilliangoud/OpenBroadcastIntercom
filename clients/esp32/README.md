# ESP32-A1S Audio Kit Client

This is the first ESP-IDF firmware target for `GOU-59`, aimed at the
Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S board with the ES8388 codec.

The ESP32 firmware currently includes:

- Wi-Fi station mode.
- WebSocket control `hello`, startup `config`, `config_update`, `talk`,
  `button`, `ack_alert`, `direct_call`, capture health, and `ping` messages.
- UDP v2 audio packets with `pcm16`, `pcm24`, `pcm48`, or optional mono Opus.
- Mixed audio receive and ES8388 playback through the legacy ESP-IDF I2S driver.
- ES8388 microphone capture through the legacy ESP-IDF I2S driver and transmit to effective
  regular/button routes.
- Regular PTT GPIO, four advertised A-D dedicated buttons, and a separate
  reply/alert hold-to-talk button.
- Optional 1.54 inch 240x240 ST7789 SPI TFT UI rotated 90 degrees for status,
  alerts, button labels, and unit identity.
- Wi-Fi power save disabled after connect.
- PSRAM-preferred playback jitter buffer.
- Local connecting/reconnecting, connected, and disconnected cues with
  configurable notification volume.

The ES8388 hardware path is fixed at `48 kHz`, `16-bit`, stereo, Philips/
standard I2S with MCLK. Network codec selection is software conversion only:
`pcm16` sends/receives 16 kHz packets, `pcm24` sends/receives 24 kHz packets,
and `pcm48` stays native to the hardware path. Opus currently uses the default
24 kHz speech profile on ESP32; keep ESP32 clients on `speech_24_standard`
until firmware profile switching is implemented and validated on hardware.

The firmware currently uses the legacy ESP-IDF I2S API (`driver/i2s.h`) because
that is the last known-good path for this board. It still uses the new I2C
master API (`driver/i2c_master.h`) for ES8388 register control and codec
probing.

## Ai-Thinker ESP32 Audio Kit V2.2 Defaults

The defaults in `main/Kconfig.projbuild` target the common V2.2 ES8388 layout:

| Signal | GPIO |
| --- | ---: |
| I2C SDA | 33 |
| I2C SCL | 32 |
| I2S MCLK | 0 |
| I2S BCLK | 27 |
| I2S LRCK/WS | 25 |
| I2S DOUT | 26 |
| I2S DIN | 35 |
| Speaker amp enable | 21 |

At boot, the firmware probes the I2C bus before ES8388 init and logs every
responding device. A normal ES8388 board should answer at `0x10`. AC101 boards
usually answer at `0x1a`; if that address is present, the firmware logs a hard
warning because this target currently supports ES8388 only. If `0x10` is absent,
the firmware stops before audio init so we do not mistake an AC101 or wrong-pin
board for a broken ES8388 setup. The most reliable physical check is still the
marking on the codec chip/module: Ai-Thinker sold ESP32-A1S/Audio Kit variants
with both AC101 and ES8388 codecs under similar V2.2 naming.

The audio hardware path is deliberately fixed for bring-up:

- ES8388 control is a local manual register sequence matching the old bring-up
  path.
- I2S is Philips/standard format, 48 kHz, 16-bit, stereo, with MCLK enabled.
  This matches the saved old config from the working-era firmware.
- Server codec changes do not reconfigure I2S or rewrite ES8388 format.
- Normal startup uses the same soft reconnecting cue as the disconnected state
  while the client is connecting. There is no separate direct I2S boot tone in
  normal mode.
- `Speaker/headphone output enable polarity`: GPIO21 polarity used when enabling
  the board output gate. If `output-test` writes
  full tone frames and the ES8388 readback looks correct but headphones are
  still silent, try the opposite polarity.
- `Audio TX packet queue length`: encoded mic packets waiting for the network
  task. Capture drops when this queue is full instead of blocking the audio
  core, which protects headset playback from Wi-Fi send stalls.
- `UDP task stack size`: stack for the network send/receive task. Keep this at
  `8192` or higher while using `pcm48` and control telemetry.
- The checked-in `sdkconfig.defaults` selects the large single-app partition and
  enables Opus. The generated local `sdkconfig` stays ignored so lab Wi-Fi,
  server, and pin settings do not get committed.

Current app-size audit on ESP-IDF 5.4, large single-app partition `0x177000`:

| Opus | ST7789 | App size | Free app partition |
|---|---|---:|---:|
| enabled | disabled | `0x11c620` | `0x5a9e0` (24%) |
| enabled | enabled, sample pins | `0x125310` | `0x51cf0` (22%) |
| disabled | enabled, sample pins | `0xf6c30` | `0x803d0` (34%) |
| disabled | disabled | `0xee1a0` | `0x88e60` (37%) |

Runtime heap pressure is reported in `capture_health.memory`, including total
free/minimum heap, internal free heap, largest internal block, PSRAM free heap,
and largest PSRAM block. When the ST7789 UI is enabled, its 240x240 RGB565
framebuffer is `115200` bytes, prefers PSRAM, falls back to general 8-bit heap,
and reports `display.framebuffer_in_psram` plus `display.framebuffer_bytes`.

The ES8388 init path is a local legacy register sequence. Use
`output-test` as the clean local test of ES8388/I2S/PA routing; normal startup
does not play a separate boot test tone.

The firmware does not have a separate semantic "headphone jack" device. It
writes stereo PCM into the ES8388 DAC and enables the codec output pairs that
the board may wire to the headphone/speaker path. In `output-test`, the
firmware now cycles explicit profiles: community Aux/headphone, community
speaker/line1, ADF all-outputs, and GPIO21 forced high/low. The Aux/headphone
profile uses the community-reported ESP32-A1S V2.2 register values:
`DACPOWER=0x0c`, OUT2 volume at `0x21`, and OUT1 muted. The log prints the
active profile before each three-tone sequence, so the first audible profile
tells us which output path your board revision actually uses.

If your board revision has no playback, check the printed PCB revision and try
the alternate DOUT/LRCK pins reported for some A1S V2.2 variants through
`idf.py menuconfig`.

GPIO34-GPIO39 are input-only and do not provide internal pull-ups. If you use
one of those pins for PTT or a dedicated button, wire an external pull-up and
expect the firmware to log a warning instead of enabling the ESP32 internal
pull-up.

## Build And Flash

Use the repo wrapper from the repository root:

```sh
tools/esp32 setup
tools/esp32 doctor
tools/esp32 menuconfig
tools/esp32 build
tools/esp32 flash-monitor /dev/cu.usbserial-XXXX
```

ESP-IDF monitor does not normally exit with `Ctrl+C`. Use `Ctrl+]` to leave
the serial monitor. If your keyboard layout makes that awkward, open the monitor
help with `Ctrl+T` then `Ctrl+H`.

If `idf.py` reports that `build/` is not a CMake build directory, remove the
stale partial build folder and retry:

```sh
tools/esp32 reset-build
tools/esp32 menuconfig
```

In `menuconfig`, set:

- `Intercom ESP32 Client -> Wi-Fi SSID`
- `Intercom ESP32 Client -> Wi-Fi password`
- `Intercom ESP32 Client -> Intercom server host or IP`
- `Intercom ESP32 Client -> Client user ID`, the requested numeric alias
- `Intercom ESP32 Client -> Stable client UID override`, optional fixed lab identity
- `Intercom ESP32 Client -> Audio diagnostic mode`
- `Intercom ESP32 Client -> Initial audio codec`
- `Intercom ESP32 Client -> ES8388 ADC input`
- `Intercom ESP32 Client -> ES8388 mic PGA gain`
- `Intercom ESP32 Client -> Capture channel`
- `Intercom ESP32 Client -> Playback jitter buffer frames`
- `Intercom ESP32 Client -> Playback prefill frames after underrun`
- `Intercom ESP32 Client -> Playback task stack size`
- `Intercom ESP32 Client -> Capture task stack size`
- `Intercom ESP32 Client -> Speaker/headphone output enable polarity`
- `Intercom ESP32 Client -> Swap I2S LRCK/WS and data out pins`, only if all
  output-test route/polarity profiles stay silent
- optional PTT, A-D dedicated button GPIOs, and reply/alert button GPIO
- optional `Intercom ESP32 Client -> ST7789 display`
- optional `Intercom ESP32 Client -> Enable Opus codec support`
- optional `Intercom ESP32 Client -> ESP32 intercom task watchdog`
- optional `Intercom ESP32 Client -> Local sidetone / self-monitor`

Keep the playback/capture task stack sizes at the defaults or higher when
testing `pcm24`, `pcm48`, or Opus. The high-rate codecs use larger 10 ms frames
than `pcm16`; the firmware keeps the large frame buffers off-stack, but the
larger stack guard leaves room for I2S, Opus, and control call frames.

The ST7789 display and battery UI are intentionally hardware-gated:

- Display support is disabled until the screen pins are wired and configured.
- The TFT top row reports battery as unknown. Real battery measurement is
  deferred until the custom PCB defines the ADC divider and charge-state
  signals.
- Hardware pin defaults are placeholders. Confirm ST7789 SPI pins, A-D buttons,
  reply/alert GPIO, and future battery ADC before making a deployable image.

The ESP32 sends both a stable `client_uid` and a requested numeric user ID in
its control `hello`. Leave `Stable client UID override` empty for deployed
boards; the firmware generates a UUID once and stores it in NVS. Set the
override only for repeatable lab images where you deliberately want a fixed
identity. The server enrollment policy decides whether a new UID is
auto-enrolled, held pending for approval, or rejected unless preconfigured.

The playback prefill setting controls how many UDP audio frames are queued
before playback resumes after startup or a buffer underrun. Keep the default
`2` while debugging headphone clicks; it adds a small fixed delay but avoids
hard zero-to-audio edges. The admin client health view reports playback queue
depth, underflows, and overflows from the ESP32 so you can tell whether clicks
are caused by jitter or by source audio artifacts.

## Firmware-Only Audio Diagnostics

Use `Intercom ESP32 Client -> Audio diagnostic mode` before debugging server
routing. These modes intentionally bypass the server so admin config cannot
hide a local ES8388/I2S problem.

- `normal intercom client`: full Wi-Fi, WebSocket, UDP, PTT, capture, playback.
- `output-test`: skips Wi-Fi/control/UDP/capture/PTT and repeatedly plays a
  three-tone local output test through the legacy I2S/ES8388 path using the
  fixed 48 kHz hardware format.
- `capture-test`: skips Wi-Fi/control/UDP/playback/PTT and reads ES8388 capture
  through the legacy I2S path, printing left/right/avg RMS, peak, DC offset, and
  clipping once per second.
- `local-loopback`: skips Wi-Fi/control/UDP/PTT and routes selected mic capture
  directly to headphone playback while printing capture meters.

Recommended bring-up order:

1. Select `output-test`, build, flash, and listen for clean repeated tones. If
   this is silent or only crackles, the fault is below the intercom server:
   physical output path, PA path, jack/speaker path, board variant, or codec
   support.
2. Select `capture-test`, build, flash, and speak normally. Confirm left/right
   meters move clearly above idle noise and do not clip. Use the configured
   capture channel to pick the cleaner side.
3. Select `local-loopback`, build, flash, and confirm the selected mic is
   audible locally without server involvement.
4. Return to `normal intercom client`, start with `pcm16`, then retest `pcm48`
   only after local playback and capture are proven.

The firmware dumps selected ES8388 registers after codec init and after
server-owned ESP32 audio config changes so we can compare the hardware state to
the known-good legacy register map.

The same commands are exposed from this folder through `make`:

```sh
make setup
make menuconfig
make build
make flash-monitor PORT=/dev/cu.usbserial-XXXX
```

The wrapper installs ESP-IDF into `~/.espressif/esp-idf` by default and targets
the `release/v5.4` branch, which keeps us on the ESP-IDF 5.x APIs used by this
first firmware. Override with `ESP_IDF_DIR` or `ESP_IDF_REF` if you already have
a preferred checkout. ESP-IDF downloads the managed `espressif/esp_websocket_client`
and `78/esp-opus` components during the first build.

Start the Rust server on the same LAN:

```sh
cargo run -p server
```

The ESP32 advertises `pcm16`, `pcm24`, `pcm48`, and Opus when Opus is enabled.
Start with `pcm16` while gain-staging a new board. Once the admin capture
meters show normal speech with zero clipping and a healthy peak margin, switch
the client codec to `pcm48` or Opus from the admin UI. `pcm16` is accepted as
the low-bandwidth fallback and now uses linear interpolation plus lightweight
FIR decimation instead of sample hold/averaging, but it will still sound more
constrained than native `pcm48`.

## Bring-Up Order

1. Flash with no PTT GPIO configured and confirm the server shows the client
   online.
2. Configure a listen channel for the ESP32 in the admin UI and verify playback.
3. Open the admin client editor and watch `Live Capture Health` while speaking
   normally at the expected mic distance.
4. Configure a regular TX channel and set talk mode to `open` temporarily to
   verify microphone send.
5. If capture health shows no raw/software clipping, switch the codec to
   `pcm48` and verify both playback and mic send. Keep `pcm16` available for
   fallback testing when bandwidth matters more than full-rate quality.
6. Wire PTT as active-low to ground, configure the GPIO, set talk mode to `ptt`,
   and verify press-to-talk.
7. Wire dedicated A-D buttons as active-low to ground and assign actions in the
   admin UI.
8. Wire the reply/alert button as active-low to ground. Holding it acks the
   newest active alert and direct-calls the alert sender; when no alert exists,
   it replies to the last direct caller.
9. Enable the ST7789 display only after the screen wiring is confirmed. The
   full-screen states cover Wi-Fi, server, enrollment/config errors, and missing
   config; the normal state shows status, alerts/calls, A-D labels, and unit ID.

## Capture Quality And Gain Staging

The ESP32 capture path is intentionally measurable before we add higher-rate
codecs. The firmware reports `capture_health` over the control WebSocket once
per second. The server exposes the latest report in admin state as
`session.capture`, shows it in the client editor, and raises warnings for
clipping, silence, high DC offset, or a likely wrong I2S channel.

Recommended board-mic defaults:

- `ES8388 ADC input`: `Differential board mic`
- `ES8388 mic PGA gain`: `9 dB`
- `Capture channel`: `Left`
- `Capture high-pass/DC blocker`: enabled
- `ES8388 automatic level control`: parsed but not applied during legacy bring-up
- `ES8388 ADC noise gate`: parsed but not applied during legacy bring-up
- `Microphone software gain percent`: `100`

Use this tuning order:

1. Keep software gain at `100`.
2. Keep `Capture channel` on `Left` or `Right` when using `Differential board
   mic`. Do not use `Average` for the differential mic path; it can cancel
   wanted voice and leave board/common noise.
3. Speak normally and confirm raw/software clipped samples stay at `0`.
4. If raw clipping appears, lower `ES8388 mic PGA gain`.
5. If software clipping appears but raw clipping does not, lower software gain.
6. If capture is nearly silent, compare `Left` and `Right` in capture health,
   then try alternate ADC inputs.
7. Leave the high-pass/DC blocker enabled unless you are intentionally measuring
   raw DC offset.
8. Leave ALC/noise-gate settings alone for now. They are parsed for admin
   visibility but not applied during the legacy ES8388 bring-up path.

Do not use server DSP to hide bad capture. High-pass, gate, compressor, and
presence processing can clean usable audio, but analog clipping from the ES8388
input path is already damaged before the server receives it.

## Local Sidetone / Self-Monitor

The ESP32 client can optionally play its own microphone locally so the wearer
can hear themselves without waiting for the server mix. This is local monitor
audio only; it does not change what is transmitted to the server, regular PTT,
dedicated button routes, or the server-side mix.

Configure the flashed fallback in `tools/esp32 menuconfig`, or override it
later from the admin client editor when server-owned ESP32 audio config is
enabled:

```text
Intercom ESP32 Client
  Local sidetone / self-monitor
```

- `Local sidetone / self-monitor -> Off`: default, no local mic monitor.
- `Local sidetone / self-monitor -> Firmware mix into playback`: mixes the
  captured mic into local playback before writing I2S. This is the safest first
  test mode, but it has roughly one local audio frame of delay.
- `Local sidetone / self-monitor -> ES8388 line-bypass mixer`: parsed for older
  configs but intentionally disabled in the current legacy bring-up.
  Use firmware sidetone until playback and capture are proven stable.

After changing this setting, rebuild and flash the firmware:

```sh
tools/esp32 build
tools/esp32 flash-monitor /dev/cu.usbserial-XXXX
```

`Firmware sidetone gain percent` controls only the firmware-mixed mode. The
codec-bypass gain fields are retained in config JSON for visibility, but the
firmware does not write ES8388 bypass/mixer registers in this bring-up pass.
The firmware path uses the captured PCM frame and mixes it into the same
playback frame as server audio, so it is predictable and easy to disable.

Recommended bring-up order:

1. Start with `Off` and verify normal server playback and microphone transmit.
2. Switch to `Firmware mix into playback`, keep gain near `25`, rebuild/flash,
   and verify the sidetone level is comfortable.
3. Leave codec-bypass sidetone off until the legacy playback/capture path is stable.

## Server-Owned ESP32 Audio Configuration

The flashed `menuconfig` values are the standalone fallback. Once the server
sends `esp32_audio.enabled=true` in a `config_update`, the firmware overrides
the runtime-changeable audio defaults from the server:

- ES8388 ADC input
- ES8388 mic PGA gain
- capture channel
- capture high-pass/DC blocker
- ES8388 automatic level control and ADC noise gate, currently reported but not
  applied during legacy bring-up
- microphone software gain
- speaker software gain, applied to the final playback stream before writing
  I2S; the ES8388 output driver is used as enabled/muted during bring-up
- notification sound gain
- sidetone mode and firmware sidetone gain; codec-bypass sidetone is forced off
- playback queue depth, underflows, and overflows

The active codec is server-owned separately through the normal client `codec`
setting. On the ESP32, changing the codec changes only UDP packet frame size and
software conversion. The ES8388 hardware remains fixed at 48 kHz.

Configure this from the admin client editor under `ESP32 Audio Hardware`. If
the server-owned toggle is disabled, the firmware reverts to its flashed
`menuconfig` audio settings. The ESP32 reports whether server-owned audio is
active in `capture_health.health.codec_config.server_control_enabled`.

Example desired config:

```json
{
  "esp32_audio": {
    "enabled": true,
    "adc_input": "difference",
    "mic_pga_gain_db": 9,
    "capture_channel": "left",
    "mic_software_gain_percent": 100,
    "speaker_software_gain_percent": 100,
    "notification_gain_percent": 50,
    "high_pass_enabled": true,
    "alc_enabled": false,
    "noise_gate_enabled": false,
    "sidetone": {
      "mode": "off",
      "firmware_gain_percent": 25,
      "codec_bypass_gain_percent": 25,
      "mic_bypass_gain_percent": 100
    }
  }
}
```

## ESP32 Codec Configuration Report

Every `capture_health` control message includes a `codec_config` object so the
admin UI can show the exact active board-side audio settings:

```json
{
  "codec_config": {
    "chip": "es8388",
    "active_codec": "pcm48",
    "server_control_enabled": false,
    "adc_input": "difference",
    "mic_pga_gain_db": 9,
    "capture_channel": "left",
    "mic_software_gain_percent": 100,
    "speaker_software_gain_percent": 100,
    "notification_gain_percent": 50,
    "high_pass_enabled": true,
    "alc_enabled": false,
    "noise_gate_enabled": false,
    "audio_backend": "legacy_i2s_es8388",
    "hardware_sample_rate_hz": 48000,
    "hardware_channels": 2,
    "hardware_bits_per_sample": 16,
    "i2s_sample_rate_hz": 48000,
    "i2s_format": "philips",
    "i2s_slot_width": "16",
    "sidetone": {
      "mode": "off",
      "firmware_gain_percent": 0,
      "codec_bypass_gain_percent": 25,
      "mic_bypass_gain_percent": 100,
      "active_bypass_source": "none",
      "codec_bypass_preserves_dac": true,
      "codec_bypass_available": false
    }
  }
}
```

Use these fields to confirm that server-owned config was applied and to tune
mic input safely: lower analog PGA first when raw clipping is reported, then
adjust software gain only after the analog path is clean.
`notification_gain_percent` only affects local ESP32 connection sounds. It does
not affect server playback, microphone capture, or sidetone. These notification
tones use a generated lookup-table sine cue with smooth attack/release so they
do not require a stored audio file or expensive per-sample trig in the playback
task. The reconnecting sound is a short one-shot cue that the control task
re-triggers about every four seconds while disconnected; it does not keep a long
silent notification stream active between tones.
The final headphone stream clamps mixed samples directly to signed 16-bit PCM
and then applies the idle floor below when the mixed value is exactly zero.
Tune these under `Intercom ESP32 Client -> Playback idle tuning`:

- `Keep tiny playback idle floor`
- `Playback idle floor amplitude`

The firmware does not stop I2S or zero the DMA buffer around notification
playback; the codec output stays open and notifications are mixed into the
normal playback stream. The legacy I2S driver is configured with TX descriptor
auto-clear disabled so underruns do not ask the driver to inject abrupt DMA
silence; the playback task continuously writes explicit idle frames when there
is no server audio or notification cue. By default those idle frames use a
constant `+1` PCM floor, the quietest non-zero signed 16-bit value, to avoid
exact all-zero digital silence after short tones without adding broadband hiss.
If the post-tone pop returns, try `Playback idle floor amplitude = 2` or `3`.
If the audio task falls behind, the monitor logs throttled
`I2S playback timing warning` messages with write gap, write duration, and short
write counters.
At startup, the ES8388 DAC is digitally muted while the DAC outputs and board
output gate are enabled. The playback task then writes silence for a short
settle window before the firmware unmutes and plays the connecting cue. The
dynamic playback mute gate remains disabled during bring-up, so normal
notifications and intercom audio stay inside the always-running I2S stream.

### ESP32 Post-Tone Pop Fix

The ESP32 Audio Kit produced a click/pop immediately after each short local
notification tone, especially the reconnecting cue. The root cause was not the
tone waveform itself. We verified this by adding smoother tone envelopes,
removing per-sample trig from the playback task, disabling I2S TX descriptor
auto-clear, adding I2S write timing logs, and trying aggressive software
de-click settings. The I2S monitor showed writes were still completing and
audio was not underrunning; the pop remained until the output stopped entering
exact digital zero.

The working fix is:

- Keep I2S and the ES8388 output path running continuously.
- Do not stop/restart I2S for local tones.
- Do not zero the DMA buffer around notification playback.
- Disable I2S TX descriptor auto-clear.
- Remove the software de-clicker; it did not address the hardware behavior.
- Replace exact zero output samples with a constant `+1` PCM idle floor.

That `+1` sample is the quietest possible non-zero signed 16-bit PCM value. It
keeps the ES8388/output stage from settling or auto-muting after short tones,
but unlike the earlier random ±1 floor, it does not add audible broadband hiss
in sensitive headphones. This is why the final implementation sounds clean:
the codec never sees an all-zero idle stream, while the output remains
effectively silent to the listener.

## Codec SDK Note

ESP-ADF and the `donny681/esp-adf` fork are useful references for Ai-Thinker
Audio Kit board definitions and codec behavior, especially AC101/ES8388 board
variants. This firmware uses ESP-IDF directly. We previously tried
Espressif's standalone `esp_codec_dev` component, but the active bring-up path
has been rolled back to legacy I2S plus manual ES8388 register setup to match
the last known working board behavior. We do not build or depend on the full
ADF framework or the Ai-Thinker ADF fork.
