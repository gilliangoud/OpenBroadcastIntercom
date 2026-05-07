# Model Assets

The repository keeps source code and small metadata in Git. Whisper model
weights are external downloads from Hugging Face and must not be committed.

## Whisper Models

The supported Whisper/whisper.cpp models are listed in
`intercom-models/manifest.json`.

Current external models:

| Model | Local filename | Size | Upstream SHA |
| --- | --- | ---: | --- |
| large-v3-turbo | `ggml-large-v3-turbo.bin` | 1.5 GiB | `4af2b29d7ec73d781377bfd1758ca957a807e941` |
| large-v3-turbo Q8_0 | `ggml-large-v3-turbo-q8_0.bin` | 834 MiB | `01bf15bedffe9f39d65c1b6ff9b687ea91f59e0e` |
| large-v3-turbo Q5_0 | `ggml-large-v3-turbo-q5_0.bin` | 547 MiB | `e050f7970618a659205450ad97eb95a18d69c9ee` |

Download the default model:

```sh
tools/download-whisper-models.py
```

List available models:

```sh
tools/download-whisper-models.py --list
```

Download a specific model:

```sh
tools/download-whisper-models.py whisper-large-v3-turbo-q8_0
```

Downloaded files are written to `intercom-models/` by default. The server
already scans that folder via `--whisper-model-dir intercom-models`, and the
admin UI can select any downloaded `.bin` model from that folder.

The downloader verifies the upstream SHA before installing the file. These SHA
values come from the Hugging Face model card for `ggerganov/whisper.cpp`.

## LFS Policy

Do not track Whisper `.bin` files in Git LFS. CI and releases intentionally
check out the repository without LFS model downloads.

The only remaining LFS-backed assets are curated non-Whisper model archives or
embedded app/server model assets documented in `THIRD_PARTY_NOTICES.md`.
