# Transcription Benchmarks

RedLine transcription should be selected from measured behavior on real
intercom audio, not model reputation alone. The benchmark harness keeps the
first version simple: validate curated mono WAV corpora, run or import model
predictions, score WER/CER plus latency, and write repeatable JSON and Markdown
reports.

## Starter Audio

Use public samples only as a bootstrap. They help validate the harness and catch
obvious model/runtime problems, but they do not replace RedLine captures from
sports comms, referees, operators, venue noise, mobile clients, and actual
cleanup pipelines.

| Corpus | Use | Notes |
| --- | --- | --- |
| LibriSpeech dummy | Tiny CI smoke fixture | Very small, clean English clips with transcripts. |
| LibriSpeech ASR | Clean baseline | Good for sanity checks, not representative of intercom audio. |
| Hugging Face LibriSpeech test_wavs | Small downloadable smoke fixture | The suite runner can download three clean WAVs plus transcripts for local ASR checks. |
| MUSAN | Noise overlay source | Mix with speech to create repeatable crowd/music/noise cases. |
| Common Voice Spontaneous English | More natural speech | Useful for hesitations and less scripted delivery. |
| AMI Meeting Corpus | Overlapping multi-speaker speech | Useful for talk-over and room-mic stress cases. |

Do not commit raw benchmark audio unless it is explicitly sanitized and
approved. Keep local corpora under an ignored artifact directory or an external
storage location.

## Corpus Format

The v1 corpus is a JSON file next to its audio folder. Each segment points to a
mono WAV and carries enough metadata to reproduce the ASR result.

```json
{
  "version": 1,
  "name": "redline-local-smoke",
  "segments": [
    {
      "id": "iphone-operator-clean-001",
      "audio": "audio/iphone-operator-clean-001.wav",
      "expected_text": "Ref one check, clock is stopped at twelve seconds.",
      "device": { "kind": "mobile", "name": "iPhone 13" },
      "route": { "channel": "referees", "role": "operator" },
      "noise": { "kind": "venue", "snr_db": 12 },
      "cleanup": { "pipeline": "deepfilternet+normalization" },
      "codec": "opus",
      "gain": { "rms_dbfs": -23.4, "peak_dbfs": -6.2 },
      "vad": { "enabled": true, "threshold": 0.02 },
      "segment": {
        "mode": "reliable",
        "chunk_ms": 18000,
        "overlap_ms": 1600,
        "prompt": "Sports officiating intercom."
      }
    }
  ]
}
```

RedLine recording sessions already produce per-client mono WAV files named
`user-<id>.wav` plus `metadata.jsonl`. The `capture` subcommand turns those
sessions into this corpus format when given ground-truth transcripts and the
cleanup/segmentation settings used for the test case.

## Running The Harness

Validate a corpus:

```sh
python3 tools/transcription_benchmark.py validate path/to/corpus.json
```

Convert an existing RedLine recording session into a corpus:

```sh
python3 tools/transcription_benchmark.py capture intercom-recordings/session-123 \
  --transcripts path/to/session-123-transcripts.json \
  --out artifacts/transcription-benchmarks/session-123/corpus.json \
  --device-kind mobile \
  --device-name "iPhone 13" \
  --noise-kind venue \
  --cleanup-pipeline deepfilternet+normalization \
  --mode reliable \
  --chunk-ms 18000 \
  --overlap-ms 1600 \
  --prompt "Sports officiating intercom."
```

Transcript maps can key entries by `user-1`, `1`, or the generated segment id
`session-123-user-1`:

```json
{
  "users": {
    "user-1": {
      "text": "Ref one check, clock is stopped at twelve seconds.",
      "notes": "iPhone operator, venue noise under scoreboard music."
    }
  }
}
```

Score deterministic predictions:

```sh
python3 tools/transcription_benchmark.py score path/to/corpus.json \
  --predictions path/to/predictions.json \
  --model-id whisper-large-v3-turbo-q8_0 \
  --out-json artifacts/transcription-benchmarks/q8.json \
  --out-md artifacts/transcription-benchmarks/q8.md
```

Prediction files can be either a segment map or a segment list:

```json
{
  "model_id": "fixture-model",
  "segments": {
    "iphone-operator-clean-001": {
      "text": "ref one check clock is stopped at twelve seconds",
      "latency_ms": 842.5
    }
  }
}
```

Run a local adapter command that prints the transcript for one audio file to
stdout:

```sh
python3 tools/transcription_benchmark.py run path/to/corpus.json \
  --model-id whisper-large-v3-turbo-q8_0 \
  --model-path intercom-models/ggml-large-v3-turbo-q8_0.bin \
  --command-template 'tools/asr-adapters/whisper_stdout.sh --model {model_path} --audio {audio}' \
  --out-json artifacts/transcription-benchmarks/q8.json \
  --out-md artifacts/transcription-benchmarks/q8.md
```

The command template is split without a shell and supports `{audio}`,
`{audio_path}`, `{segment_id}`, `{model_id}`, and `{model_path}` placeholders.

Run the macOS Whisper/Metal suite against every installed manifest transcription
model:

```sh
python3 tools/run_transcription_benchmarks.py \
  --build-adapter \
  --download-librispeech-smoke-corpus \
  --out-dir artifacts/transcription-benchmarks/hf-librispeech-cpu \
  --features transcription-whisper \
  --prompt "Clean read English speech."
```

That command builds `server/src/bin/transcription_benchmark_whisper.rs`, downloads
a small curated LibriSpeech WAV/transcript smoke corpus from Hugging Face, runs
each installed Whisper model once with the model loaded for the whole corpus,
and writes per-model reports plus `summary.md`. Use this only as a clean macOS
performance smoke test; real recommendations still require RedLine recording
sessions and noisy venue cases.

For Metal-specific checks, rebuild with:

```sh
cargo build -p server --release --features macos-metal --bin transcription_benchmark_whisper
python3 tools/run_transcription_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-cpu/corpus.json \
  --out-dir artifacts/transcription-benchmarks/hf-librispeech-metal \
  --features macos-metal
```

To test Apple MLX Whisper models, install `mlx-whisper` in an ignored local
venv and run the optional adapter from a non-sandboxed Apple Silicon session:

```sh
/opt/homebrew/bin/python3.12 -m venv artifacts/transcription-benchmarks/.venv-mlx
artifacts/transcription-benchmarks/.venv-mlx/bin/python -m pip install mlx-whisper
HF_HOME=artifacts/transcription-benchmarks/hf-cache \
  artifacts/transcription-benchmarks/.venv-mlx/bin/python tools/mlx_whisper_benchmark.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --model mlx-community/whisper-large-v3-turbo \
  --model-id mlx-whisper-large-v3-turbo \
  --out artifacts/transcription-benchmarks/mlx-whisper-large-v3-turbo/predictions.json \
  --language en \
  --prompt "Clean read English speech."
```

Then score the predictions with `tools/transcription_benchmark.py score`.
For repeatable multi-model MLX runs, use the suite runner:

```sh
python3 tools/run_mlx_whisper_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/mlx-expanded \
  --timeout 2400
```

The runner keeps the venv Python path unresolved on purpose. Resolving the
`bin/python` symlink bypasses the venv and can make `mlx_whisper` disappear
even when the package is installed.

To test WhisperKit/Core ML, install the CLI and run the suite:

```sh
brew install whisperkit-cli
python3 tools/run_whisperkit_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/whisperkit-coreml \
  --timeout 2400
```

The default suite covers `distil:large-v3`, `large-v3`, `small`, `base`, and
`tiny`. For an exact local model package downloaded from the Argmax
`whisperkit-coreml` repo, pass a path-backed model:

```sh
python3 tools/run_whisperkit_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/whisperkit-local \
  --no-default-models \
  --model whisperkit-large-v3-turbo=path:/path/to/openai_whisper-large-v3-v20240930_turbo_632MB
```

If using a locally built Argmax checkout instead of the Homebrew CLI, provide a
command template:

```sh
python3 tools/run_whisperkit_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/whisperkit-swift-run \
  --command-template 'swift run argmax-cli transcribe --model-path {model_path} --audio-path {audio_path}'
```

To test NVIDIA Parakeet/Nemotron models through NeMo, use a separate ignored
Python environment because NeMo has a large dependency stack:

```sh
/opt/homebrew/bin/python3.12 -m venv artifacts/transcription-benchmarks/.venv-nemo
artifacts/transcription-benchmarks/.venv-nemo/bin/python -m pip install -U pip wheel
artifacts/transcription-benchmarks/.venv-nemo/bin/python -m pip install 'nemo_toolkit[asr]'
python3 tools/run_parakeet_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/parakeet-nemo \
  --device auto \
  --timeout 3600
```

The default NeMo suite covers `nvidia/parakeet-unified-en-0.6b`,
`nvidia/nemotron-speech-streaming-en-0.6b`, and
`nvidia/parakeet-tdt-0.6b-v2`. The adapter currently scores the same offline
corpus path for all three models and records the streaming context parameters
that should be used for later streaming-specific captures. This keeps the
comparison apples-to-apples until we add live chunked benchmark fixtures.

To test Moonshine, prefer the ONNX backend when it is available because Useful
Sensors recommends ONNX for on-device applications. The runner also supports a
Transformers backend for quick local checks:

```sh
/opt/homebrew/bin/python3.12 -m venv artifacts/transcription-benchmarks/.venv-moonshine
artifacts/transcription-benchmarks/.venv-moonshine/bin/python -m pip install -U pip wheel
artifacts/transcription-benchmarks/.venv-moonshine/bin/python -m pip install \
  'useful-moonshine-onnx @ git+https://github.com/moonshine-ai/moonshine.git#subdirectory=moonshine-onnx'
python3 tools/run_moonshine_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/moonshine-onnx \
  --backend onnx \
  --mode offline \
  --mode chunked \
  --chunk-ms 2000
```

If the ONNX package install is blocked by Git LFS, use Transformers:

```sh
artifacts/transcription-benchmarks/.venv-moonshine/bin/python -m pip install \
  transformers torch soundfile
python3 tools/run_moonshine_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/moonshine-transformers \
  --backend transformers \
  --mode offline \
  --mode chunked \
  --mode rolling-buffer \
  --chunk-ms 2000 \
  --window-ms 8000 \
  --step-ms 1000 \
  --commit-lag-ms 1500 \
  --min-stable-passes 2
```

`offline` mode transcribes each benchmark WAV as one segment. `chunked` mode
splits each WAV into sequential chunks and transcribes each chunk independently,
then concatenates the chunk transcripts for scoring. This is closer to a live
feed than whole-file transcription, but it is not yet the same as RedLine's
server path because it does not replay Opus decode, VAD hangover, cleanup
state, context carry, partial/final transcript timing, or server-side
finalization.

`rolling-buffer` mode is the first RedLine live replay harness. It expects the
same processed mono WAVs produced by RedLine recording sessions; those WAVs are
tapped after Opus decode and server-side cleanup, which is also where live
transcription receives PCM. The replay scans 20 ms frames with RMS VAD, applies
hangover, schedules rolling transcription windows every `--step-ms`, keeps only
the most recent `--window-ms` of audio for partial jobs, emits stable partials
after `--min-stable-passes`, keeps the last `--commit-lag-ms` worth of words
uncommitted, and runs a final pass on VAD endpoint/talk release by default.

The rolling replay records final WER/CER, prefix-scored partial WER over time,
first-token latency, finalization latency, emission lag, stale jobs, dropped
jobs, queue delay, and a flicker ratio for revised partial words. This borrows
the same quality/latency/stability split used by
[SimulStreaming](https://github.com/ufal/SimulStreaming)-style evaluation
without trying to implement LAAL/DAL until we have word timestamps from the
candidate runtime. Parakeet/Nemotron-style native streaming models should plug
into the same benchmark outputs, but their adapter should feed true streaming
state instead of repeatedly decoding overlapping WAV windows.

Useful rolling-buffer sweeps:

```sh
python3 tools/run_moonshine_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/moonshine-rolling-8s \
  --backend transformers \
  --no-default-models \
  --model moonshine-tiny-transformers=UsefulSensors/moonshine-tiny \
  --mode rolling-buffer \
  --window-ms 8000 \
  --step-ms 1000 \
  --commit-lag-ms 1500 \
  --min-stable-passes 2

python3 tools/run_moonshine_benchmarks.py \
  --corpus artifacts/transcription-benchmarks/hf-librispeech-metal/corpus.json \
  --out-dir artifacts/transcription-benchmarks/moonshine-rolling-12s \
  --backend transformers \
  --no-default-models \
  --model moonshine-tiny-transformers=UsefulSensors/moonshine-tiny \
  --mode rolling-buffer \
  --window-ms 12000 \
  --step-ms 2000 \
  --commit-lag-ms 2000 \
  --min-stable-passes 2
```

## Evaluation Matrix

Fill this table only from measured local runs. The first acceptance target is a
populated comparison for RedLine captures and at least one online smoke corpus.

| Model/runtime | Mode | Cleanup | WER | CER | Latency | Throughput | Memory | Notes |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| Whisper large-v3-turbo Q8_0 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Current high-accuracy baseline. |
| Whisper large-v3-turbo Q5_0 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Smaller baseline to compare quality loss. |
| Distil-Whisper large-v3.5 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Candidate if local adapter is practical. |
| Parakeet/Nemotron streaming | streaming | TBD | TBD | TBD | TBD | TBD | TBD | Candidate for low-latency local ASR. |
| WhisperKit/Core ML | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Candidate to compare Core ML/ANE against MLX. |
| Moonshine | streaming | TBD | TBD | TBD | TBD | TBD | TBD | Candidate for low-resource devices. |

## Current macOS Smoke Result

Local run: clean Hugging Face LibriSpeech `test_wavs`, reliable mode, prompt
`Clean read English speech.`, built with `transcription-whisper` and
`macos-metal` on macOS. This is a runtime sanity check, not a RedLine
recommendation.

| Model/runtime | Backend | Mode | Cleanup | WER | CER | Avg latency | Real-time factor | Load time | Max RSS | CPU time |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Whisper large-v3-turbo | CPU | reliable | none | 3.90% | 0.28% | 4732.0 ms | 0.7x | 1602.7 ms | unavailable | 15860.0 ms |
| Whisper large-v3-turbo Q8_0 | CPU | reliable | none | 3.90% | 0.28% | 6285.4 ms | 0.9x | 1339.1 ms | unavailable | 20230.0 ms |
| Whisper large-v3-turbo Q5_0 | CPU | reliable | none | 3.90% | 0.28% | 4664.7 ms | 0.6x | 264.3 ms | unavailable | 14280.0 ms |
| Whisper large-v3-turbo | Metal | reliable | none | 3.90% | 0.28% | 2215.7 ms | 0.3x | 2224.8 ms | 1781.0 MB | 1730.0 ms |
| Whisper large-v3-turbo Q8_0 | Metal | reliable | none | 3.90% | 0.28% | 2175.2 ms | 0.3x | 567.9 ms | 1036.1 MB | 810.0 ms |
| Whisper large-v3-turbo Q5_0 | Metal | reliable | none | 3.90% | 0.28% | 2501.4 ms | 0.3x | 332.0 ms | 724.4 MB | 640.0 ms |

Metal failed inside the restricted Codex sandbox with
`ggml_metal_buffer_init: error: failed to allocate buffer, size = 7.33 MiB`, but
the same binary succeeded when run outside that sandbox. Run Metal benchmarks
from a normal terminal or with unrestricted local execution before using results
for hardware sizing. CPU RSS was unavailable because live `ps` sampling and full
`/usr/bin/time -l` reporting were restricted in the sandbox.

Mode comparison on the same Metal smoke corpus:

| Model/runtime | Mode | WER | Avg latency | Real-time factor | Max RSS | Load time |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Whisper large-v3-turbo | reliable | 3.90% | 2215.7 ms | 0.307x | 1781.0 MB | 2224.8 ms |
| Whisper large-v3-turbo | balanced | 3.90% | 2064.2 ms | 0.282x | 1796.0 MB | 1421.9 ms |
| Whisper large-v3-turbo | fast | 3.90% | 2252.5 ms | 0.308x | 1782.5 MB | 1690.4 ms |
| Whisper large-v3-turbo Q8_0 | reliable | 3.90% | 2175.2 ms | 0.291x | 1036.1 MB | 567.9 ms |
| Whisper large-v3-turbo Q8_0 | balanced | 3.90% | 2141.0 ms | 0.289x | 1047.0 MB | 851.9 ms |
| Whisper large-v3-turbo Q8_0 | fast | 3.90% | 2174.1 ms | 0.294x | 1031.8 MB | 558.0 ms |
| Whisper large-v3-turbo Q5_0 | reliable | 3.90% | 2501.4 ms | 0.340x | 724.4 MB | 332.0 ms |
| Whisper large-v3-turbo Q5_0 | balanced | 3.90% | 2372.2 ms | 0.324x | 740.2 MB | 339.8 ms |
| Whisper large-v3-turbo Q5_0 | fast | 3.90% | 2372.1 ms | 0.329x | 734.0 MB | 342.0 ms |

On this smoke corpus, all modes and model variants produced the same WER/CER.
Q8_0 is currently the best macOS Metal default candidate because it is nearly
as fast as the full model, loads much faster, and uses roughly 745 MB less peak
RSS. Q5_0 remains the low-memory candidate, but it was slower than Q8_0 under
Metal despite using less memory.

MLX comparison on the same smoke corpus, using `mlx-whisper` in a local Python
3.12 venv with `HF_HOME=artifacts/transcription-benchmarks/hf-cache`. These are
second-pass cached runs after every model was downloaded. The first segment
latency includes MLX model load because `mlx-whisper` caches the loaded model
inside the process on first use.

| Runtime/model | WER | CER | Avg latency | Real-time factor | First segment/load | Cached wall time | Max RSS | CPU time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| mlx-distil-whisper large-v3 | 1.30% | 0.28% | 1635.2 ms | 0.227x | 2249.5 ms | 6.89 s | 1960.1 MB | 3310.0 ms |
| mlx-whisper large-v3 | 1.30% | 0.28% | 3122.7 ms | 0.421x | 4356.6 ms | 11.10 s | 2410.3 MB | 7010.0 ms |
| mlx-whisper large-v3 8-bit | 1.30% | 0.28% | 2971.2 ms | 0.393x | 3715.5 ms | 11.22 s | 2186.8 MB | 5030.0 ms |
| mlx-whisper large-v3-turbo | 3.90% | 0.28% | 1806.6 ms | 0.250x | 2557.0 ms | 7.10 s | 1764.8 MB | 3480.0 ms |
| mlx-whisper large-v3-turbo q4 | 3.90% | 0.28% | 1753.1 ms | 0.241x | 2208.6 ms | 7.05 s | 775.0 MB | 2700.0 ms |
| mlx-distil-whisper medium.en | 3.90% | 0.28% | 938.7 ms | 0.131x | 1279.7 ms | 4.72 s | 1216.5 MB | 2610.0 ms |
| mlx-whisper small | 3.90% | 0.28% | 670.1 ms | 0.088x | 974.0 ms | 3.92 s | 859.2 MB | 2770.0 ms |
| mlx-whisper base | 5.19% | 0.85% | 350.9 ms | 0.046x | 596.1 ms | 2.69 s | 473.3 MB | 2260.0 ms |
| mlx-whisper tiny | 6.49% | 1.70% | 283.2 ms | 0.038x | 506.2 ms | 2.51 s | 374.1 MB | 2150.0 ms |

Additional medium/small/base quantization pass on the same cached setup:

| Runtime/model | WER | CER | Avg latency | Real-time factor | First segment/load | Cached wall time | Max RSS | CPU time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| mlx-whisper medium | 2.60% | 0.00% | 1875.1 ms | 0.249x | 2849.1 ms | 7.59 s | 1835.3 MB | 4460.0 ms |
| mlx-whisper medium 8-bit | 2.60% | 0.00% | 1748.9 ms | 0.231x | 2313.3 ms | 7.22 s | 1296.3 MB | 3760.0 ms |
| mlx-whisper medium q4 | 3.90% | 1.13% | 1803.9 ms | 0.221x | 2069.3 ms | 7.33 s | 961.8 MB | 3570.0 ms |
| mlx-whisper small 8-bit | 3.90% | 0.28% | 713.2 ms | 0.094x | 1080.9 ms | 4.05 s | 681.0 MB | 2760.0 ms |
| mlx-whisper small q4 | 3.90% | 0.28% | 695.3 ms | 0.094x | 1025.4 ms | 3.72 s | 583.3 MB | 2560.0 ms |
| mlx-whisper base 8-bit | 5.19% | 0.85% | 362.6 ms | 0.049x | 630.7 ms | 2.74 s | 430.0 MB | 2250.0 ms |
| mlx-whisper base q4 | 5.19% | 1.13% | 364.0 ms | 0.049x | 637.2 ms | 2.74 s | 409.1 MB | 2300.0 ms |

MLX is still the strongest macOS-specific runtime found so far. For clean
speech, `mlx-community/distil-whisper-large-v3` is the best accuracy/speed
candidate in this set. `mlx-community/whisper-large-v3-turbo-q4` is the current
low-memory MLX candidate because it matched turbo accuracy here while using less
than half the peak RSS. Full large-v3 and large-v3 8-bit improved WER on this
fixture, but they were materially slower than distil-large, so they need noisy
RedLine captures to justify the extra latency and memory. In the middle of the
range, medium 8-bit looks useful: it preserved medium accuracy while cutting
roughly 539 MB from peak RSS. Small q4 is the best low-resource small-class
variant in this pass. Base q4 saved very little memory over base 8-bit and lost
character accuracy, so it is not a priority candidate.

WhisperKit/Core ML comparison on the same smoke corpus, using
`whisperkit-cli 1.0.0` with `cpuAndNeuralEngine` for both audio encoder and text
decoder compute units. These are cached runs after model download and Core ML
compilation:

| Runtime/model | WER | CER | Avg latency | Real-time factor | First segment/load | Cached wall time | Max RSS | CPU time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| whisperkit distil-large-v3 | 1.30% | 0.28% | 5641.9 ms | 0.760x | 6140.3 ms | 17.00 s | 112.8 MB | 3550.0 ms |
| whisperkit small | 3.90% | 0.28% | 4285.0 ms | 0.580x | 4111.3 ms | 12.99 s | 146.5 MB | 4170.0 ms |
| whisperkit base | 5.19% | 0.85% | 3508.2 ms | 0.493x | 3450.7 ms | 10.66 s | 102.7 MB | 3040.0 ms |
| whisperkit tiny | 6.49% | 1.70% | 3476.6 ms | 0.481x | 3506.6 ms | 10.53 s | 102.8 MB | 2740.0 ms |

WhisperKit is not faster than MLX on this machine, but its process RSS is much
lower. That makes it worth keeping as a potential low-memory Core ML fallback,
not the primary macOS runtime. The openai `large-v3` variant returned no
transcript text from `whisperkit-cli` for the debug clip and is marked failed
until we test a path-backed package from the Argmax model repo.

Parakeet/NeMo comparison on the same smoke corpus, using the local
`artifacts/transcription-benchmarks/.venv-nemo` environment with NeMo ASR and
PyTorch MPS:

| Runtime/model | Device | WER | CER | Avg latency | Real-time factor | Load time | Cached wall time | Max RSS | CPU time |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| parakeet-tdt-0.6b-v2 | mps | 2.60% | 1.13% | 811.8 ms | 0.1x | 9619.1 ms | 22.71 s | 5232.0 MB | 23130.0 ms |

Parakeet TDT V2 is fast after the model is loaded, but it used far more memory
and CPU time than the MLX Whisper candidates. `nvidia/parakeet-unified-en-0.6b`
did not instantiate with the PyPI NeMo 2.7.3 package on macOS because the model
config contains `att_chunk_context_size`, which the installed encoder rejected.
Keep Unified in the candidate set, but test it through NeMo main/nightly or an
ONNX/runtime-specific path before using it for product decisions.

Moonshine comparison on the same smoke corpus, using the Transformers backend
in `artifacts/transcription-benchmarks/.venv-moonshine`. The preferred
`moonshine_onnx` package install stalled in `git-lfs filter-process` while
cloning the upstream repository, so these results are not the ONNX numbers we
ultimately want for an on-device/server runtime:

| Runtime/model | Backend | Mode | WER | CER | Avg latency | Real-time factor | Load time | Cached wall time | Max RSS | CPU time |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| moonshine tiny | transformers | offline | 1.30% | 0.85% | 811.9 ms | 0.1x | 7062.4 ms | 9.94 s | 506.1 MB | 5570.0 ms |
| moonshine tiny | transformers | chunked 2000 ms | 24.68% | 12.18% | 692.6 ms | 0.1x | 5746.8 ms | 8.29 s | 523.2 MB | 4930.0 ms |
| moonshine tiny | transformers | rolling-buffer 8000/1000 ms | 5.19% | 1.13% | 2723.4 ms | 0.3x | 7152.1 ms | 16.10 s | 591.2 MB | 11170.0 ms |
| moonshine base | transformers | offline | 3.90% | 0.57% | 683.5 ms | 0.1x | 6082.0 ms | 8.59 s | 586.9 MB | 5290.0 ms |
| moonshine base | transformers | chunked 2000 ms | 28.57% | 18.41% | 713.5 ms | 0.1x | 5695.7 ms | 8.25 s | 523.2 MB | 5090.0 ms |

Moonshine is worth keeping in the candidate set because offline tiny was both
fast and accurate on this clean fixture. The naive chunked mode is much worse,
which is exactly why the benchmark suite needs a RedLine live replay mode before
we choose a streaming default. Short chunks need VAD-aware boundaries, overlap
handling, context carry, and finalization rules; simply cutting every two
seconds is not representative enough for production quality.

The first rolling-buffer pass is much better than naive fixed chunks but still
behind full-utterance offline transcription on clean LibriSpeech. It emitted 16
stable partials across 26 rolling hypotheses, with 45.99% average prefix-scored
partial WER, 3169.4 ms average first-token latency, 558.4 ms finalization
latency, 266.4 ms average emission lag, and no stale or dropped jobs. That is
the right benchmark shape for RedLine UX decisions: final accuracy, partial
quality over time, and scheduler pressure are now visible separately.

## Acceptance Notes

Benchmark segment settings independently: `fast`, `balanced`, `reliable`, chunk
length, overlap, VAD threshold, prompt/context behavior, and finalization delay.
Benchmark cleanup variants independently: raw processed ingest, built-in cleanup,
WebRTC/RNNoise/DeepFilterNet where available, and normalization on/off.

The model catalog should not change defaults based on guesses. Add or recommend
models in the System model manager only after benchmark reports show a clear
quality, latency, or resource advantage for RedLine use cases.
