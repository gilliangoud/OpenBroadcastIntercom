#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through Hugging Face Transformers Whisper models."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import time
from pathlib import Path
from typing import Any

import rolling_transcription_replay as replay


class TransformersWhisperBackend:
    def __init__(self, args: argparse.Namespace, pipe: Any, torch: Any):
        self.args = args
        self.pipe = pipe
        self.torch = torch
        self.started_load = time.perf_counter()
        self.first_transcription = True
        self.model_load_ms: float | None = None

    def transcribe_file(self, path: Path) -> str:
        generate_kwargs: dict[str, Any] = {}
        if self.args.language:
            generate_kwargs["language"] = self.args.language
        if self.args.task:
            generate_kwargs["task"] = self.args.task

        result = self.pipe(
            str(path),
            generate_kwargs=generate_kwargs or None,
            return_timestamps=False,
        )
        if self.first_transcription:
            self.model_load_ms = (time.perf_counter() - self.started_load) * 1000.0
            self.first_transcription = False
        if isinstance(result, dict):
            return str(result.get("text", "")).strip()
        return str(result).strip()

    def transcribe_timed(self, path: Path) -> tuple[str, float]:
        started = time.perf_counter()
        text = self.transcribe_file(path)
        return text, (time.perf_counter() - started) * 1000.0


def build_pipeline(args: argparse.Namespace) -> tuple[Any, Any, str, str]:
    try:
        import torch
        from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor, pipeline
    except Exception as exc:
        raise SystemExit(
            "Transformers Whisper dependencies are not available. "
            f"Python: {sys.executable}. Import error: {exc!r}"
        ) from exc

    if args.device == "auto":
        if torch.backends.mps.is_available():
            device = "mps"
        elif torch.cuda.is_available():
            device = "cuda:0"
        else:
            device = "cpu"
    else:
        device = args.device

    if args.dtype == "auto":
        dtype = torch.float16 if device.startswith(("cuda", "mps")) else torch.float32
    elif args.dtype == "float16":
        dtype = torch.float16
    else:
        dtype = torch.float32

    processor = AutoProcessor.from_pretrained(args.model)
    load_kwargs: dict[str, Any] = {
        "torch_dtype": dtype,
        "use_safetensors": True,
    }
    if importlib.util.find_spec("accelerate"):
        load_kwargs["low_cpu_mem_usage"] = True
    model = AutoModelForSpeechSeq2Seq.from_pretrained(args.model, **load_kwargs)
    model.to(device)
    pipe = pipeline(
        "automatic-speech-recognition",
        model=model,
        tokenizer=processor.tokenizer,
        feature_extractor=processor.feature_extractor,
        torch_dtype=dtype,
        device=device,
    )
    return pipe, torch, device, str(dtype).replace("torch.", "")


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    corpus = replay.load_corpus(args.corpus)
    corpus_dir = args.corpus.resolve().parent
    pipe, torch, device, dtype = build_pipeline(args)
    backend = TransformersWhisperBackend(args, pipe, torch)
    predictions: dict[str, dict[str, Any]] = {}

    for segment in corpus["segments"]:
        segment_id = segment["id"]
        audio_path = (corpus_dir / segment["audio"]).resolve()
        if args.mode == "offline":
            text, latency_ms = backend.transcribe_timed(audio_path)
            chunks: list[dict[str, Any]] = []
            partials: list[dict[str, Any]] = []
            live_metrics: dict[str, Any] | None = None
        else:
            text, latency_ms, chunks, partials, live_metrics = replay.transcribe_rolling_buffer(
                backend,
                audio_path,
                config=replay.config_from_args(args),
                temp_prefix="redline-transformers-whisper-rolling-",
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
        "runtime": "transformers-whisper",
        "backend": {
            "device": device,
            "dtype": dtype,
            "mps": device == "mps",
            "target_os": sys.platform,
        },
        "model": args.model,
        "model_load_ms": backend.model_load_ms,
        "mode": args.mode,
        "rolling_buffer": (
            replay.config_from_args(args).public_dict()
            if args.mode == "rolling-buffer"
            else None
        ),
        "language": args.language,
        "task": args.task,
        "segments": predictions,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--model", required=True, help="Transformers model id or local path")
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--mode", choices=["offline", "rolling-buffer"], default="offline")
    parser.add_argument("--device", default="auto", help="auto, cpu, mps, cuda:0, ...")
    parser.add_argument("--dtype", choices=["auto", "float16", "float32"], default="auto")
    parser.add_argument("--language", default="en")
    parser.add_argument("--task", default="transcribe")
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
