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
`Clean read English speech.`, built with `transcription-whisper` on macOS. This
is a runtime sanity check, not a RedLine recommendation.

| Model/runtime | Mode | Cleanup | WER | CER | Avg latency | Real-time factor | Load time | CPU time |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Whisper large-v3-turbo | reliable | none | 3.90% | 0.28% | 4732.0 ms | 0.7x | 1602.7 ms | 15860.0 ms |
| Whisper large-v3-turbo Q8_0 | reliable | none | 3.90% | 0.28% | 6285.4 ms | 0.9x | 1339.1 ms | 20230.0 ms |
| Whisper large-v3-turbo Q5_0 | reliable | none | 3.90% | 0.28% | 4664.7 ms | 0.6x | 264.3 ms | 14280.0 ms |

In the local sandbox, the `macos-metal` build failed for all three Whisper
models before transcription with:

```text
ggml_metal_buffer_init: error: failed to allocate buffer, size = 7.33 MiB
```

That points at a local Metal runtime/sandbox problem rather than model accuracy.
Re-test Metal outside the restricted environment before using these numbers for
hardware sizing. RSS sampling was unavailable in this sandbox because both live
`ps` sampling and full `/usr/bin/time -l` reporting were restricted; CPU time
was still captured from the partial `time -l` output.

## Acceptance Notes

Benchmark segment settings independently: `fast`, `balanced`, `reliable`, chunk
length, overlap, VAD threshold, prompt/context behavior, and finalization delay.
Benchmark cleanup variants independently: raw processed ingest, built-in cleanup,
WebRTC/RNNoise/DeepFilterNet where available, and normalization on/off.

The model catalog should not change defaults based on guesses. Add or recommend
models in the System model manager only after benchmark reports show a clear
quality, latency, or resource advantage for RedLine use cases.
