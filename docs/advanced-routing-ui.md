# Advanced Routing And UI Rework

GOU-60 moves routing beyond channel membership while keeping the server as the
only authority for mix decisions.

## Current V1 Decision

Direct user targeting is deferred. V1 continues to route audio by channels, with
dedicated talk buttons acting as named alternate TX routes. This avoids a second
parallel routing model while IFB, priority ducking, and mix-minus are still
channel-based.

The first advanced routing primitive is per-listener/per-talker gain:

```json
{
  "user_id": 20,
  "listen": [1, 2],
  "tx": [1],
  "vol": { "1": 0.8 },
  "talker_vol": { "12": 0.6, "14": 1.2 }
}
```

For listener `20`, channel `1` is reduced to `0.8`, user `12` is then reduced
to `0.6`, and user `14` is raised to `1.2`. Gains multiply in this order:

1. Per-channel listener gain.
2. Per-talker listener gain.
3. Priority ducking.
4. IFB program ducking.
5. Limiter.

Mix-minus still wins: a listener never receives their own mic back, even if
`talker_vol` contains their own user ID.

## Admin UI Direction

The admin UI now separates the advanced controls into clearer areas:

- Dashboard: online/offline, meters, warnings, queue health, limiter, buttons.
- Client editor modal: desired config for one client with structured controls
  for codec, talk mode, priority, routing, buttons, IFB, and talker gains.
- Mix matrix: client rows by channel columns for listen, regular talk/tx, and
  per-channel listener gain. The card also opens channel management.
- Per-talker gains: listener rows by talker columns.
- Channel management modal: numeric IDs and friendly labels.

The admin refresh loop must not reload the client editor unless the operator
clicks a row. Live meters should never erase in-progress edits.

## Client UI Direction

The local client UI remains a thin operator surface. It can submit full config
through the server, including `talker_vol`, but live audio behavior only changes
after the server returns the authoritative `config_update`.

The client UI should stay organized around field tasks:

- status and diagnostics
- mute/talk/toggle
- dedicated buttons
- route and mix controls
- local mic/speaker gain
- IFB state

Current client UI screenshots are maintained in
[Client UI Screenshots](client-ui-screenshots.md).

## Next Routing Steps

Presets are server-owned snapshots of desired client configs. They are stored
in the server state file, which still defaults to `intercom-state.json` for
compatibility, alongside channels and clients:

```json
{
  "id": "refs-game",
  "name": "Refs Game",
  "clients": [
    {
      "user_id": 10,
      "name": "Ref 1",
      "listen": [1, 2],
      "tx": [1],
      "vol": { "2": 0.6 },
      "talker_vol": { "12": 0.8 },
      "codec": "pcm48",
      "talk_mode": "open",
      "priority": false
    }
  ]
}
```

Applying a preset validates all live client codec constraints first. If validation
passes, the server updates desired configs, saves the state file, applies the
configs to live sessions, and pushes `config_update` events.

Preset API:

- `PUT /admin/api/presets/:preset_id`
- `POST /admin/api/presets/:preset_id`
- `DELETE /admin/api/presets/:preset_id`

Client templates live on the same page as presets, but they are user-id-free
defaults for one client instead of multi-client snapshots. They persist in
`intercom-state.json` under `templates`:

```json
{
  "id": "referee",
  "name": "Referee",
  "client": {
    "name": "Ref",
    "listen": [1, 2],
    "tx": [1],
    "vol": { "2": 0.6 },
    "talker_vol": { "12": 0.8 },
    "codec": "pcm48",
    "talk_mode": "ptt",
    "priority": false,
    "buttons": [],
    "ifb": { "enabled": false, "program": [], "interrupt": [], "duck_gain": 0.125 },
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
}
```

Template API:

- `PUT /admin/api/templates/:template_id`
- `POST /admin/api/templates/:template_id/apply` with `{ "user_id": 12 }`
- `DELETE /admin/api/templates/:template_id`

Applying a template creates or replaces the target client's desired config. If
the client is connected, the server validates live codec support, applies the
config to the live session, and pushes the normal `config_update`.

Lockout policy is part of both desired client config and client templates. The
admin UI presents these as "client may change" toggles. Unchecked controls are
sent to the client in `config_update`, returned from local `/state`, greyed out
in the local client UI, and rejected by the desktop/Pi local APIs.

Direct user targeting should be added only after choosing one model:

- channel aliases such as `user:12` that compile to channels, or
- explicit direct-target fields that the mixer resolves separately.

The current recommendation is channel aliases first, because it preserves the
single channel-based audio path and works naturally with buttons, IFB, and PA
routes.
