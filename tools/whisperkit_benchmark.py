#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through WhisperKit/Core ML.

The default path shells out to `whisperkit-cli transcribe`, which is the least
intrusive way to compare Core ML/ANE behavior without linking WhisperKit into
the RedLine server yet. A command template is also supported for locally built
Argmax CLI checkouts.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
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


def json_text(value: Any) -> str | None:
    if isinstance(value, str):
        return value.strip() or None
    if isinstance(value, dict):
        for key in ("text", "transcript", "transcription"):
            text = json_text(value.get(key))
            if text:
                return text
        for key in ("segments", "results"):
            raw_segments = value.get(key)
            if isinstance(raw_segments, list):
                pieces = [json_text(item) for item in raw_segments]
                text = " ".join(piece for piece in pieces if piece)
                if text.strip():
                    return text.strip()
    if isinstance(value, list):
        pieces = [json_text(item) for item in value]
        text = " ".join(piece for piece in pieces if piece)
        return text.strip() or None
    return None


def strip_known_prefix(line: str) -> str:
    return re.sub(
        r"^\s*(transcription|transcript|text|result)\s*[:=-]\s*",
        "",
        line,
        flags=re.IGNORECASE,
    ).strip()


def looks_like_log_line(line: str) -> bool:
    lowered = line.strip().lower()
    if not lowered:
        return True
    prefixes = (
        "download",
        "loading",
        "loaded",
        "model",
        "audio",
        "transcribing",
        "done",
        "total",
        "time",
        "warning",
        "error:",
        "[",
    )
    return lowered.startswith(prefixes)


def extract_transcript(stdout: str, stderr: str, *, text_regex: str | None = None) -> str:
    combined = stdout.strip()
    if text_regex:
        match = re.search(text_regex, stdout, flags=re.MULTILINE | re.DOTALL)
        if not match:
            match = re.search(text_regex, stderr, flags=re.MULTILINE | re.DOTALL)
        if not match:
            raise RuntimeError(f"text regex did not match WhisperKit output: {text_regex}")
        return match.group(1).strip()

    if combined:
        try:
            text = json_text(json.loads(combined))
        except json.JSONDecodeError:
            text = None
        if text:
            return text

    for stream in (stdout, stderr):
        for line in reversed(stream.splitlines()):
            stripped = line.strip()
            if not stripped:
                continue
            try:
                text = json_text(json.loads(stripped))
            except json.JSONDecodeError:
                text = None
            if text:
                return text
            prefixed = strip_known_prefix(stripped)
            if prefixed != stripped and prefixed:
                return prefixed

    candidates = [
        strip_known_prefix(line)
        for line in (stdout + "\n" + stderr).splitlines()
        if not looks_like_log_line(strip_known_prefix(line))
    ]
    candidates = [candidate for candidate in candidates if candidate]
    if candidates:
        return candidates[-1]
    raise RuntimeError("could not extract transcript from WhisperKit output")


def build_command(args: argparse.Namespace, audio_path: Path) -> list[str]:
    replacements = {
        "audio": str(audio_path),
        "audio_path": str(audio_path),
        "model": args.model or "",
        "model_prefix": args.model_prefix or "",
        "model_path": str(args.model_path) if args.model_path else "",
        "language": args.language or "",
        "prompt": args.prompt or "",
    }
    if args.command_template:
        return [part.format(**replacements) for part in shlex.split(args.command_template)]

    command = [args.cli, "transcribe", "--audio-path", str(audio_path)]
    if args.model_path:
        command.extend(["--model-path", str(args.model_path)])
    if args.model:
        command.extend(["--model", args.model])
    if args.model_prefix:
        command.extend(["--model-prefix", args.model_prefix])
    if args.language:
        command.extend(["--language", args.language])
    if args.prompt:
        command.extend(["--prompt", args.prompt])
    if args.audio_encoder_compute_units:
        command.extend(["--audio-encoder-compute-units", args.audio_encoder_compute_units])
    if args.text_decoder_compute_units:
        command.extend(["--text-decoder-compute-units", args.text_decoder_compute_units])
    if args.verbose:
        command.append("--verbose")
    return command


def run_segment(args: argparse.Namespace, audio_path: Path) -> tuple[str, float]:
    started = time.perf_counter()
    completed = subprocess.run(
        build_command(args, audio_path),
        check=False,
        capture_output=True,
        text=True,
        timeout=args.segment_timeout,
    )
    latency_ms = (time.perf_counter() - started) * 1000.0
    if completed.returncode != 0:
        stderr = completed.stderr.strip()
        stdout = completed.stdout.strip()
        raise RuntimeError(stderr or stdout or f"WhisperKit exited {completed.returncode}")
    try:
        text = extract_transcript(completed.stdout, completed.stderr, text_regex=args.text_regex)
    except RuntimeError as exc:
        if args.mode == "rolling-buffer" and "could not extract transcript" in str(exc):
            text = ""
        else:
            raise
    return text, latency_ms


class WhisperKitBackend:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.started_load = time.perf_counter()
        self.first_transcription = True
        self.model_load_ms: float | None = None

    def transcribe_file(self, path: Path) -> str:
        text, _latency_ms = run_segment(self.args, path)
        if self.first_transcription:
            self.model_load_ms = (time.perf_counter() - self.started_load) * 1000.0
            self.first_transcription = False
        return text

    def transcribe_timed(self, path: Path) -> tuple[str, float]:
        started = time.perf_counter()
        text = self.transcribe_file(path)
        return text, (time.perf_counter() - started) * 1000.0


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    corpus = load_corpus(args.corpus)
    corpus_dir = args.corpus.resolve().parent
    predictions: dict[str, dict[str, Any]] = {}
    backend = WhisperKitBackend(args)

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
                temp_prefix="redline-whisperkit-rolling-",
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
        "runtime": "whisperkit-coreml",
        "backend": {
            "coreml": True,
            "ane_or_gpu": True,
            "cli_per_window": args.mode == "rolling-buffer",
            "target_os": sys.platform,
        },
        "model": args.model,
        "model_prefix": args.model_prefix,
        "model_path": str(args.model_path) if args.model_path else None,
        "model_load_ms": backend.model_load_ms,
        "mode": args.mode,
        "rolling_buffer": (
            replay.config_from_args(args).public_dict()
            if args.mode == "rolling-buffer"
            else None
        ),
        "language": args.language,
        "prompt": args.prompt,
        "compute_units": {
            "audio_encoder": args.audio_encoder_compute_units,
            "text_decoder": args.text_decoder_compute_units,
        },
        "segments": predictions,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--cli", default="whisperkit-cli")
    parser.add_argument("--mode", choices=["offline", "rolling-buffer"], default="offline")
    parser.add_argument("--model")
    parser.add_argument("--model-prefix")
    parser.add_argument("--model-path", type=Path)
    parser.add_argument("--language", default="en")
    parser.add_argument("--prompt")
    parser.add_argument("--audio-encoder-compute-units")
    parser.add_argument("--text-decoder-compute-units")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--text-regex")
    parser.add_argument("--command-template")
    parser.add_argument("--segment-timeout", type=float, default=900.0)
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
