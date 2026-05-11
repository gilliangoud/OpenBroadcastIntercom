#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through mlx-whisper.

This optional adapter is meant for Apple Silicon macOS machines. It imports
MLX lazily so normal CI and non-Metal environments can still import the rest of
the benchmark tooling.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import wave
from pathlib import Path
from typing import Any


def load_corpus(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("segments"), list):
        raise SystemExit(f"{path} is not a benchmark corpus")
    return data


def wav_duration_ms(path: Path) -> float:
    with wave.open(str(path), "rb") as handle:
        return handle.getnframes() / handle.getframerate() * 1000.0


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    try:
        import mlx_whisper
    except Exception as exc:
        raise SystemExit(
            "mlx-whisper is not available or cannot access Metal. "
            "Install it in a local venv and run from a non-sandboxed Apple Silicon session. "
            f"Python: {sys.executable}. Import error: {exc!r}"
        ) from exc

    corpus = load_corpus(args.corpus)
    corpus_dir = args.corpus.resolve().parent
    predictions: dict[str, dict[str, Any]] = {}
    started_load = time.perf_counter()
    first_segment = True
    model_load_ms: float | None = None

    for segment in corpus["segments"]:
        segment_id = segment["id"]
        audio_path = (corpus_dir / segment["audio"]).resolve()
        started = time.perf_counter()
        result = mlx_whisper.transcribe(
            str(audio_path),
            path_or_hf_repo=args.model,
            verbose=False,
            language=args.language,
            initial_prompt=args.prompt,
            condition_on_previous_text=args.condition_on_previous_text,
            fp16=not args.fp32,
        )
        latency_ms = (time.perf_counter() - started) * 1000.0
        if first_segment:
            model_load_ms = (time.perf_counter() - started_load) * 1000.0
            first_segment = False
        predictions[segment_id] = {
            "text": str(result.get("text", "")).strip(),
            "latency_ms": latency_ms,
            "audio_duration_ms": wav_duration_ms(audio_path),
            "language": result.get("language"),
        }

    return {
        "model_id": args.model_id,
        "runtime": "mlx-whisper",
        "backend": {
            "mlx": True,
            "metal": True,
            "target_os": sys.platform,
            "target_arch": "arm64",
        },
        "model": args.model,
        "model_load_ms": model_load_ms,
        "language": args.language,
        "condition_on_previous_text": args.condition_on_previous_text,
        "segments": predictions,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--model", required=True, help="MLX model path or Hugging Face repo")
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--language", default="en")
    parser.add_argument("--prompt")
    parser.add_argument(
        "--condition-on-previous-text",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    parser.add_argument("--fp32", action="store_true")
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
