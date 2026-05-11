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

## Evaluation Matrix

Fill this table only from measured local runs. The first acceptance target is a
populated comparison for RedLine captures and at least one online smoke corpus.

| Model/runtime | Mode | Cleanup | WER | CER | Latency | Throughput | Memory | Notes |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| Whisper large-v3-turbo Q8_0 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Current high-accuracy baseline. |
| Whisper large-v3-turbo Q5_0 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Smaller baseline to compare quality loss. |
| Distil-Whisper large-v3.5 | reliable | TBD | TBD | TBD | TBD | TBD | TBD | Candidate if local adapter is practical. |
| Parakeet/Nemotron streaming | streaming | TBD | TBD | TBD | TBD | TBD | TBD | Candidate for low-latency local ASR. |
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

## Acceptance Notes

Benchmark segment settings independently: `fast`, `balanced`, `reliable`, chunk
length, overlap, VAD threshold, prompt/context behavior, and finalization delay.
Benchmark cleanup variants independently: raw processed ingest, built-in cleanup,
WebRTC/RNNoise/DeepFilterNet where available, and normalization on/off.

The model catalog should not change defaults based on guesses. Add or recommend
models in the System model manager only after benchmark reports show a clear
quality, latency, or resource advantage for RedLine use cases.
