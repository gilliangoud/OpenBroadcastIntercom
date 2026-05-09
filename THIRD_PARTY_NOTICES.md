# Third-Party Notices

This repository includes source dependencies declared in the Cargo manifests,
external model download metadata, and a small curated set of redistributed
model assets. Dependency license metadata should be reviewed before producing
binary releases.

## Redistributed Model Assets

### OpenAI Whisper / whisper.cpp

External files, downloaded from `ggerganov/whisper.cpp` on Hugging Face:

- `intercom-models/ggml-large-v3-turbo.bin`
- `intercom-models/ggml-large-v3-turbo-q8_0.bin`
- `intercom-models/ggml-large-v3-turbo-q5_0.bin`

These files are not tracked in Git or Git LFS. The download manifest is
`intercom-models/manifest.json`; see `docs/model-assets.md`.

Whisper code and model weights are released under the MIT License by OpenAI,
and the whisper.cpp GGML conversions are distributed by the upstream
`ggerganov/whisper.cpp` model repository under MIT. See
<https://github.com/openai/whisper> and
<https://huggingface.co/ggerganov/whisper.cpp>.

### DeepFilterNet

Files:

- `deepfilternet-models/DeepFilterNet3_onnx.tar.gz`
- `deepfilternet-models/DeepFilterNet3_ll_onnx.tar.gz`

DeepFilterNet is dual-licensed under MIT or Apache-2.0 by its upstream
authors. See <https://github.com/Rikorose/DeepFilterNet>.

If you use DeepFilterNet models in published work, cite the relevant
DeepFilterNet papers listed in the upstream project documentation.

### Supertonic

Files:

- `server/assets/supertonic/onnx/*.onnx`
- `server/assets/supertonic/onnx/*.json`
- `server/assets/supertonic/voice_styles/*.json`

Supertonic sample code is MIT licensed. The accompanying model is released
under the OpenRAIL-M license by Supertone Inc. See
<https://github.com/supertone-inc/supertonic>.

## Notes for Maintainers

- Keep Whisper `.bin` files out of Git and Git LFS; update
  `intercom-models/manifest.json` instead.
- Keep any remaining LFS model assets tightly scoped in `.gitattributes`.
- Do not add additional model weights until their redistribution terms are
  documented here.
- Do not publish binary releases until dependency license review has been
  repeated for the exact release artifact.
