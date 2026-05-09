# IFB And Talent Workflow Design

This document is the workflow design output for GOU-69. It turns the current
Intercom Suite routing primitives into practical event templates for talent,
referees, director, producer, PA, and program audio.

## Current Primitives

The design uses these server-owned primitives:

- `listen`: channels a client hears.
- `tx`: regular/default talk channels.
- `talk_mode`: `muted`, `ptt`, or `open` for the regular mic route.
- `buttons`: server-configured dedicated actions. Important actions here are
  `transmit`, `alert`, `set_talk_mode`, `route_edit`, and `apply_preset`.
- `ifb`: listener-side program ducking with `{enabled, program, interrupt,
  duck_gain}`.
- `priority_channels`: source-side channel-scoped priority ducking.
- `emergency`: source-side all-call or selected-group override.
- `direct call`: instant user-targeted audio, with optional recipient ducking.
- `alerts`: visible call alerts to a user or live listeners of a channel.
- `lockout`: admin-owned permission policy for local client controls.
- `stereo`: listener-side pan controls for supported receive codecs.
- `bridge` role: software client for program input, PA output, vMix, or virtual
  audio cable workflows.

The rule of thumb is: use channels for groups and repeatable workflows, use
direct calls for one-to-one interrupts or replies, and use emergency only for
show-critical override.

## Recommended Channel Plan

These IDs are examples. The important part is to keep channel meaning stable
across presets and templates.

| Channel | Name | Purpose |
| --- | --- | --- |
| `0` | open | Default open intercom channel for newly enrolled operators. |
| `1` | Program | Clean program audio feed into talent IFB. Usually from a bridge input. |
| `2` | Production PL | Director, producer, technical crew. |
| `3` | Referee PL | Referee team regular intercom. |
| `4` | Director IFB | Director interrupt to talent/referees. |
| `5` | Producer Cue | Producer-only cueing to talent or floor. |
| `6` | PA | Route to PA bridge output. Treat as dangerous and lock down. |
| `7` | Utility | Spare channel for venue, medic, replay, or floor manager. |

Emergency is intentionally not part of the default channel plan. Intercom Suite
has a native emergency override path that bypasses normal listen membership, so
emergency should not be modeled as just another operator channel by default.

Channel `1` should be a clean program feed. If the director microphone is mixed
into program before it enters Intercom Suite, program-minus-director cannot be
guaranteed inside Intercom Suite. Feed a clean program source whenever possible.

## Role Templates

### Director

Purpose: controls show communication, interrupts talent, can reach PA only when
explicitly configured.

Recommended config:

- `listen`: `[1, 2, 3, 5]`
- `tx`: `[2]`
- `talk_mode`: `ptt`
- `priority_channels`: `[2, 4]`
- `buttons`:
  - `talent`: momentary transmit to channel `4`, `duck=true`
  - `producer`: momentary transmit to channel `5`
  - `pa`: guarded/latching transmit to channel `6`
  - `reply`: direct reply to last caller once the client UI exposes this cleanly
- `vol`: program low enough to avoid masking intercom, for example
  `{"1":0.55,"2":1.0,"3":0.8,"5":0.8}`
- `lockout`: allow local volumes, lock routing, IFB, buttons, codec, and PA
  route controls during a show.

### Producer

Purpose: manages cueing and production coordination without always interrupting
talent.

Recommended config:

- `listen`: `[1, 2, 5]`
- `tx`: `[2]`
- `talk_mode`: `ptt`
- `buttons`:
  - `cue`: momentary transmit to channel `5`, `duck=true`
  - `director`: direct transmit to director user, `duck=false`
  - `alert-talent`: send alert to talent channel or selected talent users
- `vol`: `{"1":0.5,"2":1.0,"5":0.9}`

Producer cueing should not share the director IFB channel unless both roles
must have identical interrupt behavior. Keep producer cueing separate so talent
can see and hear whether the interrupt is from director or producer.

### Talent

Purpose: listen to program and receive interruption. Usually listen-only.

Recommended config:

- `listen`: `[1, 4, 5]`
- `tx`: `[]`
- `talk_mode`: `muted`
- `ifb`: `{"enabled":true,"program":[1],"interrupt":[4,5],"duck_gain":0.125}`
- `buttons`:
  - optional `reply`: momentary direct transmit to last caller or director
  - optional `ack`: acknowledge visible alert
- `stereo`: optional. Put program left and interrupts right for operators who
  prefer spatial separation: `{"enabled":true,"channel_pan":{"1":-0.6,"4":0.7,"5":0.7}}`
- `lockout`: allow local volume only. Lock channels, talk mode, buttons, IFB,
  codec, priority, and local API unless the talent device is supervised.

Talent listen-only should be a real server config, not a UI convention. Empty
`tx` plus `talk_mode=muted` prevents accidental regular transmit, while a
dedicated server-owned reply button can still bypass mute if intentionally
configured.

### Referee

Purpose: regular referee team talk, plus controlled paths to director and PA.

Recommended config:

- `listen`: `[3, 4]`
- `tx`: `[3]`
- `talk_mode`: `ptt`
- `buttons`:
  - `director`: momentary direct transmit to director user or channel `4`
  - `pa`: momentary transmit to channel `6`, usually with hardware guard
  - `alert-director`: send visual alert to director
- `ifb`: optional. Use `{"enabled":true,"program":[3],"interrupt":[4],"duck_gain":0.2}`
  if director should duck referee chatter.
- `lockout`: lock routes and buttons. Allow local speaker volume.

For on-ice or field use, use dedicated buttons for alternate routes. The
regular PTT should remain referee-team only so muscle memory is predictable.

### Program Bridge Input

Purpose: inject clean program audio into Intercom Suite.

Recommended config:

- client role: `bridge`
- bridge mode: `input`
- `listen`: `[]`
- `tx`: `[1]`
- `talk_mode`: `open`
- codec: `pcm48` on LAN or Opus high profile if bandwidth requires it
- processing: conservative normalization only, no aggressive voice cleanup
- `lockout`: admin-owned

Program input should not use a microphone profile. Treat it like production
audio: stable level, low noise floor, and no voice-isolation processing unless
the source really needs it.

For a vMix or venue audio machine with several feeds, use `bridge-app` instead
of starting separate bridge terminals. Create one `input` route for program and
separate `output` routes for PA, production monitor, recorder, or stream feeds.

### PA Bridge Output

Purpose: route selected intercom audio to a physical PA, vMix input, virtual
audio cable, or venue mixer.

Recommended config:

- client role: `bridge`
- bridge mode: `output`
- `listen`: `[6]`
- `tx`: `[]`
- `talk_mode`: `muted`
- codec: `pcm48`
- `lockout`: fully admin-owned

PA should be a dedicated channel with explicit buttons. Do not put PA in normal
production listen/TX routes. Add strong admin warnings when a non-bridge client
has regular TX to PA or `talk_mode=open` with PA in `tx`.

## Workflow Patterns

### Director Interrupt

Use listener-side IFB for normal director-to-talent interrupt:

- Talent `ifb.program`: `[1]`
- Talent `ifb.interrupt`: `[4]`
- Director button `talent`: transmit to channel `4`, `duck=true`

This keeps clients thin: the server detects active interrupt audio and ducks
only program channels for that listener. It also works for multiple talent
devices at once.

Use direct call with `duck=true` only when the director wants to interrupt one
person without reaching every listener on the director IFB channel.

### Producer-Only Cueing

Give producer cueing its own interrupt channel:

- Talent `ifb.interrupt`: `[4, 5]`
- Producer button `cue`: transmit to channel `5`
- Director button `talent`: transmit to channel `4`

Both channels can duck program, but they remain visible and separately
controllable in the admin UI.

### Program-Minus-Director

The reliable implementation is source hygiene:

- Program bridge sends clean program to channel `1`.
- Director talks on channel `4`, not into the program bridge input.
- Talent listens to channel `1` as program and channel `4` as interrupt.

If program audio already contains director talk before it reaches the server,
Intercom Suite cannot remove it without a later source-separation feature.

### Reply-To-Director

Use direct call history for one-to-one reply:

- Director direct-calls talent with `duck=true`.
- Talent client records last caller in state.
- Talent reply button transmits direct to the last caller.

For hardware clients with limited displays, the reply button should be
server-owned and exposed as a dedicated button action. The client should show
the last caller name when it can, but the server should remain authoritative.

### Talent Listen-Only

Use all of these together:

- `tx`: `[]`
- `talk_mode`: `muted`
- `lockout.allow_talk_mode`: `false`
- `lockout.allow_channels`: `false`
- `lockout.allow_buttons`: `false`
- optional server-owned `reply` button if talkback is allowed

This prevents accidental mic routing while preserving admin-controlled
exceptions.

### Referee To PA

Use a dedicated button, never regular TX:

- Referee regular `tx`: `[3]`
- Referee `pa` button: transmit to channel `6`
- PA bridge `listen`: `[6]`
- Add route validation warning when channel `6` appears in a normal `tx` list.

Momentary mode is recommended. Latching PA should require a guarded physical
control or an admin confirmation in the UI.

### Emergency Override

Use native emergency override for show-critical messaging:

- target all clients or selected channel groups
- set `mute_others=true` when intelligibility matters more than preserving
  program audio
- otherwise use a low `duck_gain`, for example `0.10`
- show source identity and active emergency state on all clients

Do not model emergency as just another loud channel. Emergency should bypass
normal listen membership and be visibly distinct.

## Recommended Preset

The following preset is a starting point for a small show with director,
producer, one talent, two referees, one program bridge, and one PA bridge.
Replace user IDs with the deployed aliases.

```json
{
  "id": "small-show-ifb",
  "name": "Small Show IFB",
  "clients": [
    {
      "user_id": 1,
      "name": "Director",
      "listen": [1, 2, 3, 5],
      "tx": [2],
      "vol": {"1":0.55,"2":1.0,"3":0.8,"5":0.8},
      "codec": "pcm48",
      "talk_mode": "ptt",
      "priority_channels": [2, 4],
      "buttons": [
        {"id":"talent","label":"Talent","mode":"momentary","actions":[{"type":"transmit","channels":[4],"users":[],"duck":true}]},
        {"id":"producer","label":"Producer","mode":"momentary","actions":[{"type":"transmit","channels":[5],"users":[],"duck":false}]},
        {"id":"pa","label":"PA","mode":"momentary","actions":[{"type":"transmit","channels":[6],"users":[],"duck":false}]}
      ]
    },
    {
      "user_id": 2,
      "name": "Producer",
      "listen": [1, 2, 5],
      "tx": [2],
      "vol": {"1":0.5,"2":1.0,"5":0.9},
      "codec": "pcm48",
      "talk_mode": "ptt",
      "buttons": [
        {"id":"cue","label":"Cue","mode":"momentary","actions":[{"type":"transmit","channels":[5],"users":[],"duck":true}]}
      ]
    },
    {
      "user_id": 10,
      "name": "Talent",
      "listen": [1, 4, 5],
      "tx": [],
      "vol": {"1":1.0,"4":1.0,"5":0.9},
      "codec": "pcm48",
      "talk_mode": "muted",
      "ifb": {"enabled":true,"program":[1],"interrupt":[4,5],"duck_gain":0.125},
      "stereo": {"enabled":true,"channel_pan":{"1":-0.6,"4":0.7,"5":0.7}},
      "lockout": {
        "allow_channels": false,
        "allow_volumes": true,
        "allow_codec": false,
        "allow_talk_mode": false,
        "allow_priority": false,
        "allow_buttons": false,
        "allow_ifb": false,
        "allow_device_selection": true,
        "allow_local_api": false
      }
    },
    {
      "user_id": 20,
      "name": "Ref 1",
      "listen": [3, 4],
      "tx": [3],
      "codec": "pcm48",
      "talk_mode": "ptt",
      "buttons": [
        {"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[],"users":[1],"duck":false}]},
        {"id":"pa","label":"PA","mode":"momentary","actions":[{"type":"transmit","channels":[6],"users":[],"duck":false}]}
      ]
    },
    {
      "user_id": 21,
      "name": "Ref 2",
      "listen": [3, 4],
      "tx": [3],
      "codec": "pcm48",
      "talk_mode": "ptt"
    },
    {
      "user_id": 90,
      "name": "Program Bridge",
      "role": "bridge",
      "listen": [],
      "tx": [1],
      "codec": "pcm48",
      "talk_mode": "open"
    },
    {
      "user_id": 91,
      "name": "PA Bridge",
      "role": "bridge",
      "listen": [6],
      "tx": [],
      "codec": "pcm48",
      "talk_mode": "muted"
    }
  ]
}
```

## Recommended Templates

Template IDs should describe role, not a specific person:

- `director-show-control`
- `producer-cue`
- `talent-ifb-listen-only`
- `referee-field`
- `program-bridge-input`
- `pa-bridge-output`

Apply templates to enrolled devices first, then apply a preset to set show-wide
channel and button assignments.

## Admin UI Requirements

These UI surfaces are needed for the workflows to be operational:

- Presets/templates page: role-oriented templates with preview before apply.
- Client editor: IFB program/interrupt controls, buttons, lockout, stereo, and
  bridge role must stay structured fields, not raw JSON.
- Routing page: show regular TX separately from listen, and flag PA/emergency
  routes when configured as normal TX.
- Dashboard: show IFB active state, active direct calls, emergency source, and
  bridge input/output status.
- Client UI: show current channel roster, active interrupt source, visible
  alerts, and last caller/reply state where supported.

## Gaps And Follow-Ups

Already covered or mostly covered:

- Direct calls and reply routing: GOU-65.
- Alerts and advanced button actions: GOU-66/GOU-67.
- Emergency override: implemented in the server/admin model.
- Broadcast bridge and program/PA audio: GOU-62.
- Cross-platform multi-route bridge app: GOU-62.
- Preset/template UX polish: GOU-77.

Follow-up implementation gaps:

1. Seeded workflow presets/templates. The admin should ship with optional
   built-in templates for director, producer, talent IFB, referee, program
   bridge, and PA bridge instead of requiring operators to recreate this design
   by hand.
2. Routing validation warnings. The server/admin state should warn when common
   workflow mistakes are detected, such as PA in regular TX, talent IFB enabled
   with no program channel, interrupt channels not in listen, program bridge not
   using open talk mode, or a listen-only talent device left locally editable.
3. IFB/reply operator affordances. The client UI should make active interrupt
   source, last caller, reply target, and alert acknowledgement obvious on small
   screens and hardware-backed clients.
