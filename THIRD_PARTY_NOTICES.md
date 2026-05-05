# Third-Party Notices

This repository includes source dependencies declared in the Cargo manifests and
a small curated set of redistributed model assets. Dependency license metadata
should be reviewed before producing binary releases.

## Redistributed Model Assets

### OpenAI Whisper

Files:

- `intercom-models/ggml-tiny-fp16.bin`
- `intercom-models/ggml-base-fp16.bin`
- `intercom-models/ggml-small-fp16.bin`

Whisper code and model weights are released under the MIT License by OpenAI.
See <https://github.com/openai/whisper>.

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

- Keep large model files in Git LFS.
- Do not add additional model weights until their redistribution terms are
  documented here.
- Do not publish binary releases until dependency license review has been
  repeated for the exact release artifact.

