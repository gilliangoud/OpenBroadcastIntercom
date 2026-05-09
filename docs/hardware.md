# RedLine Hardware Plan

This is a working hardware path for the first low-cost units. Prices and stock
change often; verify before ordering.

## Recommended First Path

Use both Pi and ESP32 during bring-up:

- Raspberry Pi Zero 2 W or Pi 4/5 as the first field/reference client.
- Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S with ES8388 as the first
  received embedded audio target.
- ESP32-S3-Korvo-2 remains a stronger later target if we need more CPU headroom
  for embedded Opus or voice processing.

Reasoning:

- The Pi client already exists and gives us a stable Linux baseline for audio,
  GPIO, Opus, and field testing.
- Ai-Thinker ESP32 Audio Kit V2.2 has an ESP32-A1S module, ES8388 codec on
  common V2.2 boards, onboard audio connectors, speaker amp control, buttons,
  and enough I/O to validate the embedded architecture quickly.
- ESP32-S3-Korvo-2 has a current ESP-IDF/ADF board support package, ESP32-S3,
  16 MB flash, 8 MB PSRAM, two microphones, mono codec, speaker output, battery
  socket, and enough onboard peripherals for a later higher-headroom unit.
- ESP32-LyraT V4.3 has a stereo codec, headphone output, speaker outputs, dual
  microphones, battery charging, and an ESP32-WROVER-E module, but older LyraT
  variants may be EOL. Prefer V4.3 if using LyraT.

Sources:

- Espressif ESP32-S3-Korvo-2 user guide:
  https://docs.espressif.com/projects/esp-adf/en/latest/design-guide/dev-boards/user-guide-esp32-s3-korvo-2.html
- Espressif ESP32-S3-Korvo-2 board page:
  https://www.espressif.com/en/dev-board/esp32-s3-korvo-2-en
- Espressif ESP32-S3-Korvo-2 BSP:
  https://components.espressif.com/components/espressif/esp32_s3_korvo_2
- Espressif ESP32-LyraT V4.3 guide:
  https://espressif-docs.readthedocs-hosted.com/projects/esp-adf/en/latest/design-guide/dev-boards/get-started-esp32-lyrat.html
- Raspberry Pi Zero 2 W:
  https://www.raspberrypi.com/products/raspberry-pi-zero-2-w/
- Ai-Thinker ESP32-A1S AudioKit SDK:
  https://github.com/Ai-Thinker-Open/ESP32-A1S-AudioKit

## Prototype BOM

### Pi Reference Unit

| Item | Qty | Notes |
| --- | ---: | --- |
| Raspberry Pi Zero 2 W | 1 | Linux reference client; official product page lists it as a 65 mm x 30 mm, $15-class board with Wi-Fi. |
| USB audio dongle or Pi-compatible I2S audio HAT | 1 | Choose based on headset connector; USB is fastest for bring-up. |
| Momentary PTT button | 1 | Normally-open, wired to GPIO and ground. |
| Momentary dedicated buttons | 2-4 | Director, PA, spare channel buttons. |
| Pull-up wiring | as needed | Use internal pull-ups when possible; active-low config is default. |
| 5 V USB battery pack | 1 | Use a pack that can supply stable current under Wi-Fi load. |
| Enclosure | 1 | Must expose PTT, dedicated buttons, headset, charging, and status LEDs. |

### ESP32 Embedded Unit

| Item | Qty | Notes |
| --- | ---: | --- |
| Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S | 1 | Current embedded bring-up target with ES8388 codec on common V2.2 boards. |
| ESP32-S3-Korvo-2 | 1 | Later higher-headroom target: ESP32-S3, PSRAM, two microphones, codec, speaker output. |
| 4 ohm / 3 W speaker or headset wiring | 1 | Korvo-2 docs recommend a 4-ohm, 3-watt speaker for speaker output. |
| Li-ion battery with protection circuit | 1 | Korvo-2 docs explicitly recommend protected Li-ion cells. |
| Momentary PTT button | 1 | Use GPIO with debounce in firmware. |
| Momentary dedicated buttons | 2-4 | Director/PA/channel talk routes. |
| Rugged enclosure | 1 | Keep microphone opening short and clear. |

Optional breakout route if not using an audio dev board:

| Item | Qty | Notes |
| --- | ---: | --- |
| I2S MEMS microphone breakout | 1 | Adafruit SPH0645 breakout is a convenient digital mic reference. |
| I2S speaker amp / headphone codec | 1 | Needed for practical monitoring; prefer a codec with headphone driver for intercom. |
| ESP32-S3 module/dev board with PSRAM | 1 | Do not choose a no-PSRAM board for Opus work. |

## Button Layout

Minimum field unit:

- Large side/front PTT: regular referee/ref-team talk route.
- Small `Director` button: dedicated route to director/IFB interrupt channel.
- Small `PA` button: dedicated route to PA bridge channel.
- Optional `Spare` button: configured per event from the admin UI.

Default behavior:

- PTT is momentary.
- Director is momentary.
- PA should be guarded physically or configured as latching only if the operator
  explicitly wants it.
- Multiple active buttons transmit to the union of their configured transmit
  actions, including channel routes and direct-user routes.

## Audio And Mechanical Requirements

Microphone:

- Prefer MEMS microphones with SNR at or above the guidance in Espressif's
  microphone design notes.
- Keep the acoustic opening above 1 mm and the pickup path short.
- Do not bury the microphone behind foam, thick plastic, or a long tunnel.

Speaker/headset:

- For referee use, prefer headset/earpiece monitoring over an open speaker.
- For bench testing, speaker output is acceptable.
- Before committing to an enclosure, test that the headphone output level is
  sufficient in a noisy rink/venue.

Power:

- Disable Wi-Fi power save on ESP32 firmware.
- Validate battery runtime with Wi-Fi active and continuous receive audio.
- Put charging and power switch access on the enclosure exterior.

Durability:

- Buttons must be reachable with gloves.
- PTT must survive repeated hard presses.
- Cable strain relief matters more than a compact enclosure for early field
  testing.

## Open Decisions

Current GOU-59 hardware:

- Board: Ai-Thinker ESP32 Audio Kit V2.2 / ESP32-A1S.
- Codec path: ES8388 first; AC101 variants should be treated as a separate
  codec-driver follow-up.
- Initial firmware path: ESP-IDF project under `clients/esp32` with PCM16,
  PCM24, and PCM48. The ES8388/I2S hardware path stays fixed at `48 kHz`,
  `16-bit`, stereo; PCM16 and PCM24 are software-converted at the packet edge.
  Opus remains later after CPU and latency measurements.
- Current software baseline: `pcm48` sounds excellent on the Ai-Thinker ESP32
  Audio Kit V2.2 and should be treated as the recommended quality mode. `pcm16`
  is usable as a low-bandwidth fallback, with remaining resampling crackle
  tracked in GOU-85.
- Capture quality path: default to the ES8388 differential board-mic input,
  `9 dB` mic PGA gain, left capture channel, high-pass/DC blocker enabled, and
  software mic gain at `100%`. Use admin capture-health meters to confirm no
  raw or software clipping before trying higher-rate ESP32 codecs.
- Server-owned audio config: the above values are the flashed fallback. When
  `esp32_audio.enabled=true` is configured for a client, the server overrides
  ADC input, PGA gain, capture channel, high-pass, software gains, and sidetone
  mode/gain at runtime through `config_update`.
- Local monitoring: optional ESP32 sidetone is configured in
  `RedLine ESP32 Client -> Local sidetone / self-monitor`. Firmware sidetone
  is the safe bring-up mode with one local audio-frame of delay. ES8388
  line-bypass sidetone fields are retained in config JSON for visibility, but
  codec-bypass sidetone is currently forced off until the fixed playback/capture
  baseline is stable.

These are packaging/product inputs for GOU-64 and the first physical unit:

- Headset connector: 3.5 mm TRRS, 3.5 mm TRS plus separate mic, USB-C audio,
  or speaker/mic only.
- Battery target: expected runtime per event.
- Enclosure priority: handheld, belt pack, or mounted box.
- Required number of dedicated buttons on the first physical unit.

## Bring-Up Gate

Do not start serious ESP32 Opus optimization until:

- Audio input/output works reliably with `pcm48`, and `pcm16` fallback quality
  is either accepted or improved through GOU-85.
- Wi-Fi receive/transmit is stable with power save disabled.
- PSRAM buffers hold mic and jitter queues.
- CPU headroom is measured for PCM, then Opus decode, then Opus encode.
- Physical PTT and one dedicated button work reliably.
