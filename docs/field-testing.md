# RedLine Field Testing Runbook

This runbook keeps field tests repeatable. Do not treat the prototype as
event-ready until the measurement table at the end has real numbers for the
target hardware and venue network.

## Bench Startup

1. Start the server with explicit binds and auth when on a shared LAN:

   ```sh
   INTERCOM_ADMIN_TOKEN=change-me cargo run -p server -- --admin-token change-me
   ```

2. Open the admin UI at `http://127.0.0.1:40002/admin/` or the server LAN
   address. Use Basic auth with any username and the admin token as password.
3. Preconfigure clients before handing out devices:
   - refs: listen production/ref channels, default TX to refs
   - director: listen refs/program, TX director/IFB interrupt
   - PA bridge: receive-only unless intentionally keyed
4. Start desktop/Pi clients with unique user IDs and advertised buttons. On a
   vMix or venue-audio PC, prefer the bridge app for multiple production routes:

   ```sh
   cargo run -p bridge-app -- --server 192.0.2.10:40000 --control ws://192.0.2.10:40001
   ```

   Add separate routes for program input, PA output, recorder feed, and local
   monitor output instead of running several terminal windows by hand.
5. Confirm the admin dashboard shows:
   - all expected clients online
   - expected codec per client
   - input meters move only when someone speaks
   - output meters move on listening clients
   - no decode, malformed packet, or source drop warnings

## Packet Loss And Jitter

Run both clients through the UDP impairment proxy:

```sh
cargo run -p server
cargo run -p netem -- --listen 127.0.0.1:41000 --server 127.0.0.1:40000 --drop-percent 2 --delay-ms 20 --jitter-ms 20
cargo run -p desktop -- --user-id 1 --server 127.0.0.1:41000 --codec pcm48
cargo run -p desktop -- --user-id 2 --server 127.0.0.1:41000 --codec pcm48
```

`netem` applies impairment in both directions and logs packet counters every 5
seconds by default. `--delay-ms 20 --jitter-ms 20` means per-packet delay varies
around 20 ms and is clamped at zero, so it approximates the common `20 ms +/-
20 ms` venue test case. Set `--stats-interval-ms 0` for quieter logs.

Test matrix:

| Scenario | Drop | Jitter | Codec | Jitter Buffer | Pass Criteria |
| --- | ---: | ---: | --- | ---: | --- |
| clean LAN reference | 0% | 0 ms | pcm48 | 20 ms | no underruns, clean speech |
| normal Wi-Fi | 1% | 10 ms | opus | 40 ms | intelligible speech, no stuck routes |
| poor Wi-Fi | 3% | 20 ms | opus | 60 ms | usable commands, no crashes |
| debug fallback | 2% | 20 ms | pcm16 | 60 ms | lower quality accepted, stable routing |
| stress | 5% | 30 ms | opus | 80 ms | degraded but recovers after impairment stops |

During each scenario, keep this command running in another terminal:

```sh
cargo run -p admin -- field-report --min-sessions 2 --max-queue-depth 3 --max-age-ms 1500 --require-audio
```

Use `--json` when saving output into the measurement log. The command exits
non-zero if the server has too few sessions, has not received audio when
`--require-audio` is set, has stale sessions, or any session source queue grows
past the threshold. Warnings call out decode errors, packet drops, and limiter
activity.

## Latency Measurement

Use a click or hand clap near the sender microphone and record sender/receiver
audio with a second device. Measure the waveform offset.

Record:

| Codec | Jitter Buffer | Network | One-Way Latency | Notes |
| --- | ---: | --- | ---: | --- |
| pcm48 | 20 ms | clean LAN | TBD | |
| pcm16 | 40 ms | venue Wi-Fi | TBD | |
| opus | 40 ms | venue Wi-Fi | TBD | |
| opus | 60 ms | roaming/walk test | TBD | |

Target guidance:

- Under 80 ms feels natural for RedLine.
- 80-150 ms is usable for production cues.
- Over 150 ms needs investigation before referee/talent use.

Recommended starting defaults before real venue measurements:

| Network/use case | Codec | Client jitter | Notes |
| --- | --- | ---: | --- |
| wired/local LAN quality reference | pcm48 | 20 ms | best quality, highest bandwidth |
| good Wi-Fi operations | opus | 40 ms | preferred default once Opus is stable on the device |
| busy venue Wi-Fi | opus | 60 ms | more resilient, slightly more latency |
| constrained CPU/debug fallback | pcm24 | 60 ms | middle quality without Opus CPU cost |
| last-resort compatibility | pcm16 | 60-80 ms | lowest bandwidth PCM, lower perceived quality |

## Walking Test

1. Place one listener near the server/AP and one talker walking the real route.
2. Keep the admin dashboard visible.
3. Walk through expected coverage edges, doors, rink boards, and crowd areas.
4. Speak short phrases every 5 seconds while pressing regular PTT and each
   dedicated button.
5. Note packet drops, queue depth spikes, active-talker false triggers, and
   reconnect events.
6. Save an admin health snapshot after each route:

   ```sh
   cargo run -p admin -- field-report --json --require-audio > field-report-route-a.json
   ```

Pass criteria:

- Control reconnects automatically after Wi-Fi interruption.
- Dedicated buttons do not stay active after disconnect/reconnect.
- Opus remains intelligible during normal walking.
- Admin warnings identify bad links quickly enough for an operator to react.

## CPU And Memory Profiling

Desktop/macOS:

```sh
ps -Ao pid,pcpu,pmem,command | rg 'target/debug/(server|desktop|app|pi)'
```

Linux/Pi:

```sh
ps -eo pid,pcpu,pmem,rss,cmd | grep -E 'target/debug/(server|pi|pi-gpio)'
```

Record steady-state CPU with:

- one talker, one listener
- three simultaneous talkers
- Opus on all clients
- PCM48 on all clients
- admin dashboard polling

For each run, record server, talker, listener, and admin browser CPU separately
where possible. Any sustained process above roughly one full core on the target
hardware should be treated as a blocker before adding more users.

## Operator Troubleshooting

Quiet audio:

- Check mic/speaker gain in the client local UI.
- Check per-channel `vol` in the admin client editor and mix matrix.
- Prefer `pcm48` to isolate codec quality from routing/mixer issues; use
  `pcm24` as the middle-quality PCM option when bandwidth is tighter.

Warbly or broken Opus:

- Confirm both client and server advertise/support Opus.
- Increase client `--jitter-ms` to 60.
- Check admin transport warnings for decode errors or queue drops.
- Fall back to `pcm48` on LAN, `pcm24` as a middle tier, or `pcm16` on
  constrained clients.

Stuck talking route:

- In admin, set talk mode to `muted`, clear/toggle active dedicated buttons,
  and release transient regular Talk if it is active.
- Restart the client if the physical button process is suspect.
- Confirm Pi GPIO latching buttons only trigger on press.

IFB/program issues:

- Confirm the talent client has IFB enabled.
- Confirm program channels are in `ifb.program`.
- Confirm director interrupt channels are in `ifb.interrupt`.
- Watch the dashboard IFB badge and output limiter while interrupt audio is active.

## Regression Checklist

Before any demo or venue test:

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- server help output includes auth and admin options
- desktop, app, Pi, and GPIO help output includes local auth/button options
- admin UI loads and does not overwrite edits during live meter refresh
- two desktop clients can talk over `pcm48`
- two desktop clients can talk over `opus`
- server admin can change a connected client's codec/talk mode/routes
- Pi local API can mute/unmute and press a dedicated button
- GPIO companion dry-run emits expected endpoints
- netem 2% loss / `20 ms +/- 20 ms` jitter remains usable
- `cargo run -p admin -- field-report --require-audio` passes while clients are talking

## Automated Coverage

The repeatable parts of this runbook are covered by workspace tests:

- `tools/netem` unit/integration tests validate drop probability, fixed delay,
  symmetric jitter bounds, CLI range validation, bidirectional UDP forwarding,
  and packet counters.
- `tools/admin` tests validate `field-report` pass/fail behavior for healthy
  sessions, missing audio, stale sessions, and backed-up source queues.
- `server/tests/control.rs` validates that admin state exposes live input/output
  meters, limiter activity, and transport health after real UDP audio is sent.
- `server/tests/forwarding.rs` validates mixer/routing behavior under the same
  local UDP packet path used by field tests.

Real walking tests, microphone/speaker quality judgments, AP roaming, and
physical latency measurements still require the target venue and hardware.

## Measurement Log

| Date | Venue/Network | Clients | Codec | Jitter Buffer | Loss/Jitter | Latency | CPU | Field Report | Result |
| --- | --- | --- | --- | ---: | --- | ---: | --- | --- | --- |
| TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |
