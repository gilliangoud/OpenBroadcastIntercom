#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through Moonshine ASR.

Moonshine is designed for short live-transcription windows. This adapter can
score whole files, replay a segment as independent chunks, or run a RedLine-like
rolling-buffer replay over processed ingest WAVs. RedLine recording WAVs are
tapped after Opus decode and server cleanup, which is the same PCM point used by
live transcription in the server.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import rolling_transcription_replay as replay


def normalize_transcribe_output(output: Any) -> str:
    if isinstance(output, str):
        return output.strip()
    if isinstance(output, dict):
        for key in ("text", "transcript", "transcription"):
            if key in output:
                return normalize_transcribe_output(output[key])
    if isinstance(output, (list, tuple)):
        pieces = [normalize_transcribe_output(item) for item in output]
        return " ".join(piece for piece in pieces if piece).strip()
    return str(output).strip()


class MoonshineBackend:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.model_load_ms: float | None = None

    def transcribe_file(self, path: Path) -> str:
        raise NotImplementedError

    def info(self) -> dict[str, Any]:
        return {}


class MoonshineOnnxBackend(MoonshineBackend):
    def __init__(self, args: argparse.Namespace):
        started = time.perf_counter()
        try:
            import moonshine_onnx
        except Exception as exc:
            raise SystemExit(
                "moonshine_onnx is not available. Install useful-moonshine-onnx in "
                f"the selected Python environment. Python: {sys.executable}. "
                f"Import error: {exc!r}"
            ) from exc
        super().__init__(args)
        self.moonshine_onnx = moonshine_onnx
        self.model_load_ms = (time.perf_counter() - started) * 1000.0

    def transcribe_file(self, path: Path) -> str:
        return normalize_transcribe_output(self.moonshine_onnx.transcribe(path, self.args.model))

    def info(self) -> dict[str, Any]:
        return {"onnx": True}


class MoonshineTransformersBackend(MoonshineBackend):
    def __init__(self, args: argparse.Namespace):
        started = time.perf_counter()
        try:
            import torch
            from transformers import AutoProcessor, MoonshineForConditionalGeneration
        except Exception as exc:
            raise SystemExit(
                "Transformers Moonshine dependencies are not available in the selected "
                f"Python environment. Python: {sys.executable}. Import error: {exc!r}"
            ) from exc
        super().__init__(args)
        self.torch = torch
        self.processor = AutoProcessor.from_pretrained(args.model)
        if args.device == "auto":
            if torch.cuda.is_available():
                self.device = "cuda"
            elif getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
                self.device = "mps"
            else:
                self.device = "cpu"
        else:
            self.device = args.device
        self.dtype = torch.float16 if self.device == "cuda" and args.fp16 else torch.float32
        self.model = (
            MoonshineForConditionalGeneration.from_pretrained(args.model)
            .to(self.device)
            .to(self.dtype)
        )
        self.model.eval()
        self.model_load_ms = (time.perf_counter() - started) * 1000.0

    def transcribe_file(self, path: Path) -> str:
        import soundfile as sf

        audio, sample_rate = sf.read(str(path), dtype="float32", always_2d=False)
        inputs = self.processor(
            audio,
            return_tensors="pt",
            sampling_rate=sample_rate,
        )
        inputs = {key: value.to(self.device) for key, value in inputs.items()}
        token_limit_factor = self.args.token_limit_factor / sample_rate
        seq_lens = inputs["attention_mask"].sum(dim=-1)
        max_length = max(1, int((seq_lens * token_limit_factor).max().item()))
        with self.torch.inference_mode():
            generated_ids = self.model.generate(**inputs, max_length=max_length)
        return self.processor.decode(generated_ids[0], skip_special_tokens=True).strip()

    def info(self) -> dict[str, Any]:
        return {"transformers": True, "torch": True, "device": self.device}


def build_backend(args: argparse.Namespace) -> MoonshineBackend:
    if args.backend == "onnx":
        return MoonshineOnnxBackend(args)
    if args.backend == "transformers":
        return MoonshineTransformersBackend(args)
    raise ValueError(f"unsupported backend {args.backend}")


def transcribe_offline(backend: MoonshineBackend, audio_path: Path) -> tuple[str, float, list[dict[str, Any]]]:
    started = time.perf_counter()
    text = backend.transcribe_file(audio_path)
    latency_ms = (time.perf_counter() - started) * 1000.0
    return text, latency_ms, []


def transcribe_chunked(
    backend: MoonshineBackend,
    audio_path: Path,
    *,
    chunk_ms: int,
    overlap_ms: int,
) -> tuple[str, float, list[dict[str, Any]]]:
    if chunk_ms <= 0:
        raise ValueError("chunk_ms must be positive")
    if overlap_ms < 0 or overlap_ms >= chunk_ms:
        raise ValueError("overlap_ms must be non-negative and smaller than chunk_ms")

    channels, sample_width, sample_rate, total_frames, _duration_ms = replay.wav_metadata(audio_path)
    if channels != 1:
        raise ValueError(f"Moonshine benchmark expects mono WAV input, got {channels} channels")
    if sample_width != 2:
        raise ValueError(f"Moonshine benchmark expects 16-bit PCM WAV input, got {sample_width} bytes")

    chunk_frames = max(1, int(sample_rate * chunk_ms / 1000.0))
    step_frames = max(1, int(sample_rate * (chunk_ms - overlap_ms) / 1000.0))
    total_latency_ms = 0.0
    chunk_results: list[dict[str, Any]] = []
    pieces: list[str] = []

    with tempfile.TemporaryDirectory(prefix="redline-moonshine-chunks-") as tmp:
        tmp_dir = Path(tmp)
        chunk_count = max(1, math.ceil(max(total_frames - chunk_frames, 0) / step_frames) + 1)
        for index in range(chunk_count):
            start_frame = min(index * step_frames, total_frames)
            if start_frame >= total_frames:
                break
            frame_count = min(chunk_frames, total_frames - start_frame)
            chunk_path = tmp_dir / f"chunk-{index:04d}.wav"
            replay.write_chunk(audio_path, chunk_path, start_frame=start_frame, frame_count=frame_count)
            started = time.perf_counter()
            text = backend.transcribe_file(chunk_path)
            latency_ms = (time.perf_counter() - started) * 1000.0
            total_latency_ms += latency_ms
            if text:
                pieces.append(text)
            chunk_results.append(
                {
                    "index": index,
                    "start_ms": start_frame / sample_rate * 1000.0,
                    "duration_ms": frame_count / sample_rate * 1000.0,
                    "latency_ms": latency_ms,
                    "text": text,
                }
            )
    return " ".join(pieces).strip(), total_latency_ms, chunk_results


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    backend = build_backend(args)
    corpus = replay.load_corpus(args.corpus)
    corpus_dir = args.corpus.resolve().parent
    predictions: dict[str, dict[str, Any]] = {}

    for segment in corpus["segments"]:
        segment_id = segment["id"]
        audio_path = (corpus_dir / segment["audio"]).resolve()
        if args.mode == "offline":
            text, latency_ms, chunks = transcribe_offline(backend, audio_path)
            partials: list[dict[str, Any]] = []
            live_metrics: dict[str, Any] | None = None
        elif args.mode == "chunked":
            text, latency_ms, chunks = transcribe_chunked(
                backend,
                audio_path,
                chunk_ms=args.chunk_ms,
                overlap_ms=args.overlap_ms,
            )
            partials = []
            live_metrics = None
        else:
            text, latency_ms, chunks, partials, live_metrics = replay.transcribe_rolling_buffer(
                backend,
                audio_path,
                config=replay.config_from_args(args),
                temp_prefix="redline-moonshine-rolling-",
            )
        predictions[segment_id] = {
            "text": text,
            "latency_ms": latency_ms,
            "audio_duration_ms": replay.wav_duration_ms(audio_path),
            "chunks": chunks,
        }
        if partials:
            predictions[segment_id]["partials"] = partials
        if live_metrics is not None:
            predictions[segment_id]["live_metrics"] = live_metrics

    return {
        "model_id": args.model_id,
        "runtime": "moonshine",
        "backend": {
            "target_os": sys.platform,
            **backend.info(),
        },
        "model": args.model,
        "model_load_ms": backend.model_load_ms,
        "mode": args.mode,
        "chunk_ms": args.chunk_ms if args.mode == "chunked" else None,
        "overlap_ms": args.overlap_ms if args.mode == "chunked" else None,
        "rolling_buffer": (
            {
                **replay.config_from_args(args).public_dict(),
            }
            if args.mode == "rolling-buffer"
            else None
        ),
        "segments": predictions,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--model", default="moonshine/tiny")
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--backend", choices=["onnx", "transformers"], default="onnx")
    parser.add_argument("--mode", choices=["offline", "chunked", "rolling-buffer"], default="offline")
    parser.add_argument("--chunk-ms", type=int, default=2000)
    parser.add_argument("--overlap-ms", type=int, default=0)
    replay.add_rolling_replay_args(parser)
    parser.add_argument("--device", choices=["auto", "cpu", "mps", "cuda"], default="auto")
    parser.add_argument("--fp16", action="store_true")
    parser.add_argument("--token-limit-factor", type=float, default=6.5)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    result = transcribe_corpus(args)
    output = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(output, encoding="utf-8")
    else:
        print(output, end="")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
