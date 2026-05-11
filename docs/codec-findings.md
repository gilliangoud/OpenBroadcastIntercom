# Codec Findings

This document records the practical codec and audio-hardware findings from the
prototype work so far. It covers RedLine edge codecs, the server mixer
domain, Opus profiles, and the Ai-Thinker ESP32 Audio Kit V2.2 / ES8388 path.

## Current Architecture

- The server mixer domain is always 48 kHz mono PCM.
- Clients may send lower-rate audio, but the server decodes/resamples every
  incoming source into the 48 kHz mixer domain before routing, IFB ducking,
  per-talker gain, stereo panning, limiting, recording, and transcription taps.
- The server sends one mixed output stream per listener. Stereo receive is a
  listener-side output format; source ingest remains mono.
- UDP audio packets use protocol v2 with explicit targets: channel, direct user,
  or mixed server output.

## PCM Codecs

| Codec | Rate | Frame | Payload | Intended use |
| --- | ---: | ---: | ---: | --- |
| `pcm16` | 16 kHz mono | 10 ms / 160 samples | 320 bytes | lowest PCM bandwidth, debug fallback, low-power devices |
| `pcm24` | 24 kHz mono | 10 ms / 240 samples | 480 bytes | middle PCM quality tier for ESP32/Pi when Opus is not desired |
| `pcm48` | 48 kHz mono or stereo receive | 10 ms / 480 mono samples | 960 bytes mono, 1920 bytes stereo | quality reference, LAN/wired production paths |

Important naming decision: `pcm24` and `pcm48` are rate names, not bit-depth
names. All current PCM packet samples are signed 16-bit little-endian. `pcm24`
means 24 kHz PCM16; `pcm48` means 48 kHz PCM16.

Operational findings:
- `pcm48` is the clean quality reference. Use it when bandwidth is available.
- `pcm16` works, but perceived quality is noticeably lower because the edge
  bandwidth and sample rate are lower. On ESP32 it now uses linear interpolation
  on 16 kHz receive and a small FIR before 48 kHz to 16 kHz decimation, which
  reduces roughness without changing the codec's bandwidth target.
- `pcm24` is the useful middle tier for devices that cannot afford `pcm48` but
  should sound better than `pcm16`.
- Server-side processing can improve low-rate speech, but it cannot repair
  analog clipping or a bad capture path.

## Server Processing Engines

Processing is configured per client through the `processing` object:

- `engine = "built_in"`: the current low-latency high-pass, noise gate/VAD,
  transient suppressor, compressor/AGC, and presence enhancer. This is the
  default and has no external runtime dependency.
- `engine = "rnnoise"`: in-process Xiph RNNoise. It operates naturally on the
  server's 48 kHz / 10 ms mixer-domain frames and is the first AI noise
  suppression path. It is a good fit for noisy laptop and room microphones when
  preserving background ambience is less important.
- `engine = "webrtc"`: bundled WebRTC Audio Processing Module integration. It
  runs in-process in the server binary and provides high-pass filtering, noise
  suppression, AGC/limiting, and voice activity on 48 kHz / 10 ms frames.
- `engine = "deepfilternet"`: high-quality DeepFilterNet-style processing. It
  is opt-in per client, carries a `deep_filter_model` path, and runs compatible
  ONNX `.tar.gz`/`.tgz` model packages through Tract, or complete Core ML
  package directories on macOS builds with `processing-deepfilternet-coreml`.
  Each client uses a per-user worker thread so slow inference falls back instead
  of blocking audio ingest. `deep_filter_backend` can request `auto`, `tract`,
  or `coreml`, and `apple_compute_units` records the preferred Apple Core ML
  target. The admin UI scans `deepfilternet-models/` by default. PyTorch
  checkpoint `.zip` packages are not runtime models for the server backend.

The `pipeline` array can run processing stages in order. An empty pipeline runs
the selected `engine` as one stage. Useful presets are `webrtc -> built_in` for
OS-independent laptop mic cleanup, `webrtc -> rnnoise -> built_in` for more
aggressive cleanup, and `rnnoise -> built_in` when AI suppression should run
without WebRTC AGC.

Processing status reports the requested engine, per-stage availability,
fallback detail, input/output RMS, gate state, gain reduction, and loudness
normalization gain. This is intentionally visible in admin state so operators
can tell the difference between "WebRTC/RNNoise/DeepFilterNet is active",
"DeepFilterNet is configured but currently falling back", and "the leveler is
boosting/attenuating a specific client."

The `processing.normalization` block is a speech loudness leveler, not a noise
suppression engine. It runs after cleanup and before routing/mixer gains. Use it
to make whispering, normal speech, and loud speech arrive closer to the same
perceived level. Keep `noise_floor_rms` high enough that idle room tone is not
boosted. Good starting points are:

- Laptop mic: target RMS `0.14`, max boost `4`, max attenuation `8`, adaptation
  `250 ms`, noise floor `0.012`.
- ESP32/Ai-Thinker board mic: target RMS `0.13`, max boost `3`, max attenuation
  `8`, adaptation `300 ms`, noise floor `0.015`.
- Headset mic: target RMS `0.14`, max boost `3`, max attenuation `6`,
  adaptation `220 ms`, noise floor `0.010`.
- Production bridge input: target RMS `0.16`, max boost `1.5`, max attenuation
  `4`, adaptation `500 ms`, noise floor `0.006`.

On macOS, build the server with `--features macos-accelerated` to enable
the bundled cleanup engines plus whisper.cpp Metal support through `whisper-rs`
for built-in recording and live transcription. The RedLine Server Tauri app
uses that feature set when built with `--features native`. DeepFilterNet exposes
Apple backend preferences and local Core ML package discovery in the
config/status model. Complete Core ML package directories run through Core ML on
macOS builds that include `processing-deepfilternet-coreml`; ONNX archives keep
using the Tract runtime with an explicit fallback note when Core ML is requested.

## Opus Profiles

Opus is selected with `codec = "opus"` plus a separate profile. Stereo is a
separate listener setting and is supported for Opus profiles; stereo bitrates
use 2x the mono bitrate.

| Profile | Rate | Mono bitrate | Complexity | Bandwidth | Intended use |
| --- | ---: | ---: | ---: | --- | --- |
| `speech-16-low` / `speech_16_low` | 16 kHz | 20 kbps | 3 | wideband | low bandwidth speech |
| `speech-24-standard` / `speech_24_standard` | 24 kHz | 32 kbps | 5 | superwideband | default RedLine speech |
| `speech-48-high` / `speech_48_high` | 48 kHz | 56 kbps | 8 | fullband | high quality speech |
| `music-48` / `music_48` | 48 kHz | 80 kbps | 8 | fullband | program/music-like feeds |

Encoder settings used by the shared client/server Opus path:
- VBR enabled.
- constrained VBR enabled.
- in-band FEC enabled.
- packet-loss expectation: 5%.
- Opus signal mode follows the profile: speech profiles use voice-oriented
  settings, music profile uses music-oriented settings.

Operational findings:
- Opus should not be hidden behind a Cargo feature; server and normal Rust
  clients should support it by default.
- ESP32 should not advertise Opus until the firmware can both encode and decode
  the selected profile reliably.
- Opus quality problems that sound like silence or severe distortion are usually
  a profile/rate/layout mismatch, not a routing problem.

## Stereo Receive

- Stereo is a listener setting, not a codec identity.
- Mono source frames are panned into a per-listener stereo mix at the server.
- `pcm48` stereo outputs interleaved 16-bit left/right samples.
- Opus stereo is enabled by encoding/decoding two-channel Opus at the selected
  profile rate.
- Lower-rate PCM remains mono for now. If stereo is needed, use `pcm48` or Opus.

## macOS Capture Findings

- The raw MacBook microphone can sound worse than normal voice apps because
  those apps often use platform voice processing and/or Voice Isolation.
- `VoiceProcessingIO` standard mode can still pass keyboard/chassis transients.
  macOS Voice Isolation suppresses those better, but the app cannot force Voice
  Isolation; the user controls it through macOS microphone mode UI.
- The desktop local UI now reads AVFoundation's active/preferred microphone
  mode and can open Apple's microphone-mode selector from the Stats modal.
- A desktop-side transient suppressor and low-level de-click silence gate are
  now applied before transmit to reduce keyboard bursts and chopped
  Voice-Isolation residue.
- If the Mac client is muted and ESP32 headphone clicks stop, the source is the
  Mac transmit stream, not the ESP32 headphone amp by itself.

## ESP32 / ES8388 Findings

Target board: Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S with ES8388.

Current firmware codec support:
- Advertises `pcm16`, `pcm24`, and `pcm48`.
- Opus is deferred.
- ES8388/I2S stays fixed at `48 kHz`, `16-bit`, stereo. PCM16 and PCM24 are
  software-converted at the packet edge; PCM48 stays native.

Capture path findings:
- The board-mic path must be diagnosed with capture health before increasing
  software gain.
- Start with differential board mic, PGA 9 dB, capture left, software gain
  100%, capture high-pass on, ES8388 ALC on, ES8388 noise gate on.
- Avoid `capture_channel = average` for differential mic bring-up; averaging can
  partially cancel the wanted voice signal.
- Fix analog gain staging first. Server DSP is a cleanup stage, not a fix for
  ES8388 clipping or a wrong analog input route.

Playback path findings:
- ESP32 playback quality improves substantially with higher network codec rates
  because the hardware path is already fixed at 48 kHz; `pcm48` avoids packet
  resampling, while `pcm16` and `pcm24` trade bandwidth for quality.
- Hardware testing confirmed `pcm48` sounds excellent on the Ai-Thinker ESP32
  Audio Kit V2.2. `pcm16` is improved with a lightweight interpolation/FIR
  resampler, but `pcm48` remains the quality baseline for this board.
- The firmware now reports playback queue depth, underflows, and overflows.
- `speaker_software_gain_percent` is an overall playback scaler in firmware.
  The ES8388 output driver is treated as enabled/muted, while normal level
  changes happen in the always-running software playback stream.
- ESP32 firmware has local diagnostic modes for hardware bring-up:
  `output-test`, `capture-test`, and `local-loopback`. Use these before server
  routing when playback is silent or only crackles.
- A small playback prefill plus fade-in/fade-out removes hard zero-to-audio
  edges that were heard as headphone clicks.
- The active firmware path uses legacy ESP-IDF I2S plus a manual ES8388 register
  sequence because that is the known-working route for this board. `output-test`
  cycles the useful ES8388 output profiles and PA GPIO polarity; if every pass
  is silent or crackly, the fault is in ES8388/I2S framing, clocking, PA enable,
  or board analog routing, not UDP receive or server mixing.
- Normal startup does not play a separate direct I2S boot test tone. It uses the
  same soft connecting cue as reconnecting/disconnected state after the playback
  task has started.
- ES8388 register dumps are now part of bring-up. They are printed after codec
  init and after server-owned audio config changes, with warnings when the
  chip differs from the intended control/power/format/mixer/output-route state.
- ES8388 bring-up should use 16-bit I2S slots by default because the codec
  registers and firmware buffers are configured for 16-bit sample words. The
  32-bit slot option is useful only while diagnosing wire framing.
- ESP32 microphone capture must not perform UDP sends on the audio core. A
  bounded TX packet queue lets capture drop during Wi-Fi stalls instead of
  starving I2S playback, which is heard as headset clicks.

## ES8388 Sidetone / Bypass Findings

There are two sidetone paths:

- Firmware sidetone: ESP32 reads mic samples, mixes them into I2S playback, and
  writes the combined frame. This is safer and predictable, but not zero-delay.
- ES8388 codec bypass: ES8388 routes analog input into the output mixer
  internally. This is near-zero-delay, but it is hardware-route-sensitive and
  can feed back immediately.

Current firmware status: codec-bypass sidetone is parsed for older admin JSON
but forced off in normal operation while GOU-59 is focused on a repeatable
playback/capture baseline. The findings below are retained for the later
near-zero-delay monitor follow-up.

Important ES8388 register finding:
- Register 38 (`DACCONTROL16`) selects which analog source feeds the output
  mixer (`LMIXSEL` / `RMIXSEL`).
- Registers 39 and 42 (`DACCONTROL17` / `DACCONTROL20`) enable DAC-to-mixer,
  input-to-mixer, and set mixer gain (`LI2LOVOL` / `RI2ROVOL`) from -15 dB to
  +6 dB.
- In codec-bypass mode the DAC-plus-input mixer value must use the ES8388
  line-bypass range (`0x40` through `0x78`, with `0x50` as 0 dB). The DAC-only
  value is `0x90`. Using the wrong `0xC0`-style value can leave the codec awake
  but prevent server/I2S playback from reaching the headphone mixer.
- Registers 46-49 (`DACCONTROL24` through `DACCONTROL27`) set the ES8388 output
  driver volume. We keep them at 0 dB normally, but mic-bypass gain above 200%
  adds output-driver boost up to the codec limit. This raises server playback
  too, so it is a last-stage loudness boost rather than a pure mic-only gain.
- Our original line-bypass path always selected LIN2/RIN2. That means changing
  line-bypass gain did not properly affect mic1/mic2 because the mixer source
  was still the raw line input path.
- For mic1, mic2, and differential ADC inputs, the firmware now selects the
  ES8388 ADC P/N input after the mic amplifier as the bypass source and applies
  the separate `mic_bypass_gain_percent`.
- For line1/line2 ADC inputs, the firmware keeps using raw line bypass and
  `codec_bypass_gain_percent`.

Current bypass gain controls:

| Setting | Applies to | Notes |
| --- | --- | --- |
| `firmware_gain_percent` | firmware sidetone only | mixed by ESP32 into playback |
| `codec_bypass_gain_percent` | line1/line2 bypass | ES8388 raw LIN/RIN mixer path |
| `mic_bypass_gain_percent` | mic1/mic2/differential bypass | 0-200% uses the ES8388 mixer gain; >200% also boosts output-driver volume |

Current ES8388 bypass source mapping:

| ADC input | Bypass source |
| --- | --- |
| `line1` | LIN1/RIN1 |
| `line2` | LIN2/RIN2 |
| `mic1` | ADC P after mic amplifier |
| `mic2` | ADC N after mic amplifier |
| `difference` + right capture | ADC N after mic amplifier |
| `difference` + left or average capture | ADC P after mic amplifier |

The firmware reports `active_bypass_source` in `capture_health.codec_config`.
Use that field in the admin UI when validating sidetone behavior.

## Source Notes

- Espressif ADF ES8388 driver docs describe ES8388 ADC input modes and mic PGA
  control:
  https://docs.espressif.com/projects/esp-adf/en/latest/api-reference/abstraction/es8388.html
- Espressif ADF ES8388 source shows the common line-bypass setup using
  `DACCONTROL16`, `DACCONTROL17`, `DACCONTROL20`, and `DACCONTROL21`:
  https://raw.githubusercontent.com/espressif/esp-adf/master/components/audio_hal/driver/es8388/es8388.c
- ES8388 datasheet/user-guide material documents `DACCONTROL16` source select,
  `DACCONTROL17` / `DACCONTROL20` mixer enable/gain bits, and output mixer gain
  range:
  https://pcbartists.com/wp-content/uploads/2022/12/es8388-datasheet-english.pdf
