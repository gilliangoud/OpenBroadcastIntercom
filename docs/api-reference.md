# API And Protocol Reference

[Docs index](README.md) | [Root README](../README.md)

RedLine uses JSON over HTTP for admin/local APIs and JSON over WebSocket for the
control plane. Audio packets are UDP and are intentionally not documented here
as a public stable API yet.

## Admin HTTP API

The admin UI/API is served under `/admin/` and `/admin/api` on the server admin
bind address. Set `--admin-token` or `INTERCOM_ADMIN_TOKEN` on shared networks.

Common endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /admin/api/state` | Live sessions, desired configs, devices, enrollment policy, bridge/tally/model/recording/transcription status, channels, metrics, meters, warnings, and packet health. |
| `PUT /admin/api/clients/:user_id` | Replace desired client config. |
| `PATCH /admin/api/clients/:user_id` | Update part of a desired client config. |
| `DELETE /admin/api/clients/:user_id` | Remove desired config without disconnecting a live client. |
| `POST /admin/api/devices/:client_uid/approve` | Approve a pending or rejected stable device identity. |
| `POST /admin/api/devices/:client_uid/reject` | Reject a stable device identity. |
| `PUT /admin/api/devices/:client_uid` / `PATCH /admin/api/devices/:client_uid` | Update device enrollment metadata. |
| `PUT /admin/api/channels/:channel_id` | Create or rename a channel. |
| `DELETE /admin/api/channels/:channel_id` | Delete a channel name without changing routing. |
| `PUT /admin/api/presets/:preset_id` / `POST /admin/api/presets/:preset_id` / `DELETE /admin/api/presets/:preset_id` | Save, apply, or delete presets. |
| `PUT /admin/api/templates/:template_id` / `POST /admin/api/templates/:template_id/apply` / `DELETE /admin/api/templates/:template_id` | Save, apply, or delete client templates. |
| `POST /admin/api/alerts` | Send a runtime call alert. |
| `POST /admin/api/alerts/:alert_id/cancel` | Cancel an active alert. |
| `POST /admin/api/announcements` | Send text alerts and/or spoken announcements. |
| `POST /admin/api/tts` | Compatibility endpoint for spoken announcements. |
| `POST /admin/api/emergency` | Start or stop emergency override audio. |
| `POST /admin/api/recording/start` / `POST /admin/api/recording/stop` | Start or stop a recording session. |
| `GET /admin/api/recording/status` / `GET /admin/api/recording/sessions` | Recording/transcription status and recent sessions. |
| `POST /admin/api/transcription/live/start` / `POST /admin/api/transcription/live/stop` | Start or stop live transcription. |
| `GET /admin/api/transcription/live/status` | Live transcription engine, queue, drop, and per-user worker status. |
| `GET /admin/api/transcription/models` / `PUT /admin/api/transcription/model` | List and select local Whisper models. |
| `GET /admin/api/models/catalog` | Curated transcription and audio-cleanup model catalog with installed/download status. |
| `POST /admin/api/models/download` | Manually download one curated model by `id`. |
| `GET /admin/api/models/downloads` | Recent model download progress and errors. |
| `GET /admin/api/transcripts` / `POST /admin/api/transcripts` | Query transcript segments or append a manual/test segment. |

Desired client config bodies can include `client_uid`, `role`, `name`, `listen`,
`tx`, `buttons`, `ifb`, `stereo`, `esp32_audio`, `processing`, `lockout`, `vol`,
`talker_vol`, `codec`, `opus_profile`, `talk_mode`, `priority`,
`priority_channels`, and tally mapping fields.

## Local Client APIs

Desktop and app-host clients serve a local browser UI/API on
`http://127.0.0.1:41002/` by default. Pi serves a local API on
`0.0.0.0:41001` by default. Set `--local-ui-token`, `--local-api-token`, or the
matching environment variables when exposing these beyond localhost.

Common local endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /health` | Basic health response. |
| `GET /state` | Local route, gain, codec, talk, button, alert, IFB, processing, playback, and roster state. |
| `PUT /config` | Submit a full config change through the server first. |
| `POST /talk-mode` | Set `muted`, `ptt`, or `open`. |
| `POST /talk/down`, `/talk/up`, `/talk/toggle` | Change regular PTT state. |
| `POST /mute`, `/unmute` | Mute or restore regular talk mode. |
| `POST /codec` | Request an edge codec change. |
| `POST /gain` | Change local mic/speaker gain where supported. |
| `POST /buttons/:id/down`, `/buttons/:id/up`, `/buttons/:id/toggle` | Send configured button events to the server. |
| `POST /alerts` | Send a call alert. |
| `POST /alerts/:id/ack`, `/alerts/:id/cancel` | Acknowledge or cancel alerts. |

The local API submits control changes to the server first. Live runtime state
changes only after the server accepts the update and pushes authoritative config
back to the client.

## Control WebSocket Messages

Clients and admin tools connect to the server control endpoint, default
`ws://127.0.0.1:40001`.

Common client/admin messages:

```json
{"type":"hello","requested_user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","codecs":["pcm16","pcm24","pcm48","opus"],"buttons":[{"id":"director","label":"Director"}],"capabilities":{}}
```

```json
{"type":"config","user_id":1,"listen":[1,2],"tx":[1],"codec":"opus","opus_profile":"speech_48_high","talk_mode":"ptt","priority_channels":[1]}
```

```json
{"type":"audio_codec","user_id":1,"codec":"pcm48"}
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
{"type":"capture_health","user_id":50,"health":{"selected":{"rms":0.08,"peak":0.22,"dc_offset":0.0},"raw_clipped_samples":0,"software_clipped_samples":0}}
```

```json
{"type":"ping","user_id":1}
```

```json
{"type":"status"}
```

Clients send `hello` on startup to enroll their stable `client_uid`, receive
their assigned numeric `user_id`, mark the persistent WebSocket as the
client-owned control connection, and advertise codecs, buttons, and capabilities
they actually support. If `codecs` is omitted, the server assumes `["pcm16"]`.

A `hello` response with `preconfigured:true` means the server already has
desired config for that UID or assigned user and will push it as
`config_update`; clients should not overwrite it with startup defaults.

Common responses and server-to-client events:

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
{"type":"status","sessions":[],"metrics":{"audio_packets_received":20,"mixed_packets_sent":18,"control_messages_received":3}}
```

```json
{"type":"config_update","user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","name":"Ref 1","listen":[1,2],"tx":[1],"codec":"opus","opus_profile":"speech_48_high","talk_mode":"ptt","regular_talk_active":true,"priority":false,"priority_channels":[1],"buttons":[],"active_buttons":[],"active_alerts":[],"recent_alerts":[]}
```

```json
{"type":"presence_update","user_id":1,"client_uid":"6f2f0d10-2ef7-4b1b-948f-6ad5b2622eb0","channels":[{"channel_id":1,"members":[{"user_id":1,"name":"Ref 1","present":true,"transmitting":false}]}]}
```

The server rejects codec changes if the server build or the specific registered
client does not support the requested codec.

## Config Shapes

Button config:

```json
{
  "buttons": [
    {
      "id": "director",
      "label": "Director",
      "mode": "momentary",
      "actions": [
        {"type": "transmit", "channels": [2], "users": [12], "duck": true}
      ]
    }
  ]
}
```

Processing config:

```json
{
  "processing": {
    "mode": "auto",
    "engine": "rnnoise",
    "profile": "voice_isolation",
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

Lockout config:

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

IFB config:

```json
{"ifb":{"enabled":true,"program":[1],"interrupt":[9],"duck_gain":0.125}}
```

Stereo receive config:

```json
{"stereo":{"enabled":true,"channel_pan":{"1":-1,"9":1}}}
```
