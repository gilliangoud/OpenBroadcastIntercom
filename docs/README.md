# RedLine Documentation

[Root README](../README.md)

This is the documentation map for RedLine. Start with the root README for a
short product and local-run overview, then use these pages for deeper setup,
reference, and production workflows.

## Getting Started

| Document | Use It For |
| --- | --- |
| [Architecture](architecture.md) | System overview, ports, audio/control flow, client identity, routing, processing, and security model. |
| [Command Reference](command-reference.md) | Server, client, bridge, Pi, GPIO, admin CLI, and network impairment commands. |
| [API And Protocol Reference](api-reference.md) | Admin HTTP API, local client APIs, and WebSocket control messages. |
| [Field Testing Runbook](field-testing.md) | Bench startup, packet-loss checks, latency tests, walking tests, profiling, and regression checks. |

## Clients And Platforms

| Document | Use It For |
| --- | --- |
| [Native App](native-app.md) | Tauri native wrapper development, lifecycle, packaging, settings, and branding. |
| [Mobile Tauri Clients](mobile-clients.md) | iOS/Android runtime model, permissions, local toolchain, and validation. |
| [Client UI Screenshots](client-ui-screenshots.md) | Current screenshots for mobile, Tauri, native, desktop, Pi, and bridge UI surfaces. |
| [Hardware](hardware.md) | Pi/ESP32 hardware direction, buttons, tally LEDs, audio requirements, and bring-up gates. |
| [ESP32 README](../clients/esp32/README.md) | ESP32 firmware-specific build, flash, board, audio, and sidetone details. |

## Production Workflows

| Document | Use It For |
| --- | --- |
| [Bridge App](bridge-app.md) | Multi-route bridge setup, vMix browser source outputs, NDI routes, and packaging notes. |
| [IFB And Talent Workflows](ifb-talent-workflows.md) | Director, producer, talent, referee, program, PA, and IFB routing patterns. |
| [Advanced Routing And UI Rework](advanced-routing-ui.md) | Routing model and admin/client UI direction. |

## Audio, Models, And Transcription

| Document | Use It For |
| --- | --- |
| [Codec Findings](codec-findings.md) | PCM/Opus behavior, server processing engines, stereo receive, macOS capture, and ESP32 findings. |
| [Model Assets](model-assets.md) | Curated model catalog, download policy, Core ML package notes, and LFS policy. |
| [Transcription Benchmarks](transcription-benchmarks.md) | Corpus format, benchmark harness, model comparison results, rolling-buffer tests, and acceptance notes. |

## Release And Repo Hygiene

| Document | Use It For |
| --- | --- |
| [Release Automation](release-automation.md) | Version source of truth, first-pass artifacts, and iOS signing notes. |
| [Generated Artifact Policy](generated-artifacts.md) | Which generated outputs belong in source control. |
| [Public Repository Hygiene](public-repo-hygiene.md) | What to keep, what to exclude, and the public pre-publish checklist. |
