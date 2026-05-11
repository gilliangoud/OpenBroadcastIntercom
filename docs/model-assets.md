# Model Assets

The repository keeps source code and small metadata in Git. Large model
weights are curated external downloads from Hugging Face and must not be
committed unless they are explicitly approved Git LFS assets.

## Catalog

The supported model assets are listed in `intercom-models/manifest.json`.
The manifest covers:

- `transcription`: Whisper/whisper.cpp GGML/GGUF assets.
- `deepfilternet_onnx`: DeepFilterNet3 ONNX archives used by the Tract runtime.
- `deepfilternet_coreml`: locally generated or future hosted Core ML packages.

The server admin System page reads this same catalog through
`/admin/api/models/catalog`, shows installed/missing/download state, and starts
manual downloads through `/admin/api/models/download`. Downloads never accept
custom URLs; the manifest must provide a Hugging Face URL and SHA-256 checksum.
Transcription catalog recommendations should be backed by
`docs/transcription-benchmarks.md` results before changing defaults.

## Current Models

| Model | Category | Local filename | Size | SHA-256 |
| --- | --- | --- | ---: | --- |
| Whisper large-v3-turbo | transcription | `ggml-large-v3-turbo.bin` | 1.5 GiB | `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69` |
| Whisper large-v3-turbo Q8_0 | transcription | `ggml-large-v3-turbo-q8_0.bin` | 834 MiB | `317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1` |
| Distil-Whisper large-v3 GGML | transcription | `ggml-distil-large-v3.bin` | 1.4 GiB | `2883a11b90fb10ed592d826edeaee7d2929bf1ab985109fe9e1e7b4d2b69a298` |
| Whisper large-v3-turbo Q5_0 | transcription | `ggml-large-v3-turbo-q5_0.bin` | 547 MiB | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` |
| DeepFilterNet3 ONNX | deepfilternet_onnx | `DeepFilterNet3_onnx.tar.gz` | 7.98 MiB | `c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616` |
| DeepFilterNet3 low-latency ONNX | deepfilternet_onnx | `DeepFilterNet3_ll_onnx.tar.gz` | 36.4 MiB | `5998e58e8ba0e09bb76986ef97b84afa065a571ef282d4a1222f341e3251cf3a` |

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

Download DeepFilterNet ONNX assets:

```sh
tools/download-whisper-models.py --category deepfilternet_onnx
```

Downloaded files are written to each model's catalog `destination_dir` by
default. The downloader and server both verify SHA-256 before installing.

## Core ML Packages

Core ML DeepFilterNet packages can be generated locally for validation before
hosting:

```sh
tools/convert-deepfilternet-coreml.py --input deepfilternet-models/DeepFilterNet3_onnx.tar.gz --archive
tools/convert-deepfilternet-coreml.py --input deepfilternet-models/DeepFilterNet3_ll_onnx.tar.gz --package-name DeepFilterNet3_ll_coreml --archive
swift tools/verify-deepfilternet-coreml.swift deepfilternet-coreml-models/DeepFilterNet3_ll_coreml
```

The package directory is installed under `deepfilternet-coreml-models/` and is
considered present when it contains `config.ini`, `enc.mlmodelc`,
`erb_dec.mlmodelc`, `df_dec.mlmodelc`, and `metadata.json`. Modern
`coremltools` no longer converts ONNX directly, so the converter falls back to
ONNX simplification plus a PyTorch bridge and traces a fixed sequence length
with `--sequence-len` defaulting to `1`. Once hosted, add the curated Hugging
Face URL and SHA-256 to the manifest to enable downloads. The Swift verifier
loads each compiled Core ML model with Apple's runtime and runs zero-input
predictions against the metadata shapes; it does not exercise the server audio
pipeline.

## LFS Policy

Do not track Whisper `.bin` files or generated Core ML packages in Git LFS. CI
and releases intentionally check out the repository without local model
downloads.

The only remaining LFS-backed assets are curated non-Whisper model archives or
embedded app/server model assets documented in `THIRD_PARTY_NOTICES.md`.
