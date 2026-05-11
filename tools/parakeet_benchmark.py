#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through NVIDIA NeMo ASR models.

This adapter targets Parakeet/Nemotron checkpoints from Hugging Face. It keeps
NeMo imports lazy because the dependency stack is large and platform-specific.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import wave
from pathlib import Path
from typing import Any

import rolling_transcription_replay as replay


def load_corpus(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("segments"), list):
        raise SystemExit(f"{path} is not a benchmark corpus")
    return data


def wav_duration_ms(path: Path) -> float:
    with wave.open(str(path), "rb") as handle:
        return handle.getnframes() / handle.getframerate() * 1000.0


def output_text(value: Any) -> str:
    if hasattr(value, "text"):
        return str(value.text).strip()
    if isinstance(value, dict):
        for key in ("text", "pred_text", "transcript", "transcription"):
            raw = value.get(key)
            if raw is not None:
                return str(raw).strip()
    return str(value).strip()


def select_device(requested: str) -> tuple[str, Any | None]:
    try:
        import torch
    except Exception:
        return "cpu", None

    if requested != "auto":
        return requested, torch
    if torch.cuda.is_available():
        return "cuda", torch
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps", torch
    return "cpu", torch


def transcribe_one(asr_model: Any, audio_path: Path, *, batch_size: int, timestamps: bool) -> str:
    kwargs: dict[str, Any] = {"batch_size": batch_size}
    if timestamps:
        kwargs["timestamps"] = True
    try:
        output = asr_model.transcribe([str(audio_path)], **kwargs)
    except TypeError:
        kwargs.pop("batch_size", None)
        output = asr_model.transcribe([str(audio_path)], **kwargs)
    first = output[0] if isinstance(output, (list, tuple)) and output else output
    return output_text(first)


class NemoAsrBackend:
    def __init__(self, asr_model: Any, args: argparse.Namespace):
        self.asr_model = asr_model
        self.args = args

    def transcribe_file(self, path: Path) -> str:
        return transcribe_one(
            self.asr_model,
            path,
            batch_size=self.args.batch_size,
            timestamps=self.args.timestamps,
        )

    def transcribe_timed(self, path: Path) -> tuple[str, float]:
        started = time.perf_counter()
        text = self.transcribe_file(path)
        return text, (time.perf_counter() - started) * 1000.0


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    try:
        import nemo.collections.asr as nemo_asr
    except Exception as exc:
        raise SystemExit(
            "NVIDIA NeMo ASR is not available. Install a NeMo ASR-capable Python "
            f"environment before running this adapter. Python: {sys.executable}. "
            f"Import error: {exc!r}"
        ) from exc

    device, torch = select_device(args.device)
    corpus = load_corpus(args.corpus)
    corpus_dir = args.corpus.resolve().parent
    predictions: dict[str, dict[str, Any]] = {}

    started_load = time.perf_counter()
    asr_model = nemo_asr.models.ASRModel.from_pretrained(model_name=args.model)
    if torch is not None and hasattr(asr_model, "to"):
        asr_model = asr_model.to(device)
    if hasattr(asr_model, "eval"):
        asr_model.eval()
    model_load_ms = (time.perf_counter() - started_load) * 1000.0
    backend = NemoAsrBackend(asr_model, args)

    for segment in corpus["segments"]:
        segment_id = segment["id"]
        audio_path = (corpus_dir / segment["audio"]).resolve()
        if args.mode in {"offline", "streaming-reference"}:
            text, latency_ms = backend.transcribe_timed(audio_path)
            chunks: list[dict[str, Any]] = []
            partials: list[dict[str, Any]] = []
            live_metrics: dict[str, Any] | None = None
        else:
            text, latency_ms, chunks, partials, live_metrics = replay.transcribe_rolling_buffer(
                backend,
                audio_path,
                config=replay.config_from_args(args),
                temp_prefix="redline-nemo-rolling-",
            )
        predictions[segment_id] = {
            "text": text,
            "latency_ms": latency_ms,
            "audio_duration_ms": wav_duration_ms(audio_path),
            "chunks": chunks,
        }
        if partials:
            predictions[segment_id]["partials"] = partials
        if live_metrics is not None:
            predictions[segment_id]["live_metrics"] = live_metrics

    return {
        "model_id": args.model_id,
        "runtime": "nemo-asr",
        "backend": {
            "nemo": True,
            "torch": torch is not None,
            "device": device,
            "target_os": sys.platform,
        },
        "model": args.model,
        "model_load_ms": model_load_ms,
        "mode": args.mode,
        "rolling_buffer": (
            replay.config_from_args(args).public_dict()
            if args.mode == "rolling-buffer"
            else None
        ),
        "batch_size": args.batch_size,
        "timestamps": args.timestamps,
        "streaming_context": {
            "left_context_secs": args.left_context_secs,
            "chunk_secs": args.chunk_secs,
            "right_context_secs": args.right_context_secs,
        },
        "segments": predictions,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--model", default="nvidia/parakeet-unified-en-0.6b")
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--device", default="auto", choices=["auto", "cpu", "mps", "cuda"])
    parser.add_argument("--mode", default="offline", choices=["offline", "streaming-reference", "rolling-buffer"])
    parser.add_argument("--batch-size", type=int, default=1)
    parser.add_argument("--timestamps", action="store_true")
    parser.add_argument("--left-context-secs", type=float, default=5.6)
    parser.add_argument("--chunk-secs", type=float, default=0.56)
    parser.add_argument("--right-context-secs", type=float, default=0.56)
    replay.add_rolling_replay_args(parser)
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
