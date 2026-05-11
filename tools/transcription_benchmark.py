#!/usr/bin/env python3
"""Validate and score RedLine transcription benchmark corpora.

The harness is intentionally model-agnostic. It can score fixed prediction
files for deterministic CI tests, or run a local adapter command that prints one
transcript per audio file to stdout.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import time
import unicodedata
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Any


class BenchmarkError(Exception):
    pass


@dataclass(frozen=True)
class Segment:
    id: str
    audio: str
    audio_path: Path
    expected_text: str
    metadata: dict[str, Any]
    duration_ms: float | None


@dataclass(frozen=True)
class Corpus:
    path: Path
    name: str
    version: int
    segments: list[Segment]


def normalize_text(text: str) -> str:
    normalized = unicodedata.normalize("NFKC", text).lower()
    normalized = re.sub(r"[^a-z0-9]+", " ", normalized)
    return " ".join(normalized.split())


def levenshtein(reference: list[str] | str, hypothesis: list[str] | str) -> int:
    if reference == hypothesis:
        return 0
    previous = list(range(len(hypothesis) + 1))
    for row_index, ref_value in enumerate(reference, start=1):
        current = [row_index]
        for column_index, hyp_value in enumerate(hypothesis, start=1):
            insert = current[column_index - 1] + 1
            delete = previous[column_index] + 1
            replace = previous[column_index - 1] + (0 if ref_value == hyp_value else 1)
            current.append(min(insert, delete, replace))
        previous = current
    return previous[-1]


def score_text(reference: str, hypothesis: str) -> dict[str, Any]:
    ref_norm = normalize_text(reference)
    hyp_norm = normalize_text(hypothesis)
    ref_words = ref_norm.split()
    hyp_words = hyp_norm.split()
    word_errors = levenshtein(ref_words, hyp_words)
    ref_chars = ref_norm.replace(" ", "")
    hyp_chars = hyp_norm.replace(" ", "")
    char_errors = levenshtein(ref_chars, hyp_chars)
    ref_word_count = len(ref_words)
    ref_char_count = len(ref_chars)
    return {
        "reference_normalized": ref_norm,
        "hypothesis_normalized": hyp_norm,
        "reference_words": ref_word_count,
        "hypothesis_words": len(hyp_words),
        "word_errors": word_errors,
        "wer": word_errors / ref_word_count if ref_word_count else 0.0,
        "reference_chars": ref_char_count,
        "hypothesis_chars": len(hyp_chars),
        "char_errors": char_errors,
        "cer": char_errors / ref_char_count if ref_char_count else 0.0,
    }


def read_wav_metadata(path: Path) -> tuple[int, int, float]:
    try:
        with wave.open(str(path), "rb") as handle:
            channels = handle.getnchannels()
            sample_rate = handle.getframerate()
            frames = handle.getnframes()
    except wave.Error as exc:
        raise BenchmarkError(f"{path} is not a readable WAV file: {exc}") from exc
    duration_ms = (frames / sample_rate) * 1000.0 if sample_rate else 0.0
    return channels, sample_rate, duration_ms


def compact_metadata(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        for key in ("name", "kind", "type", "pipeline", "mode"):
            if key in value and value[key] is not None:
                return str(value[key])
        if not value:
            return ""
        return ", ".join(f"{key}={value[key]}" for key in sorted(value))
    return str(value)


def load_corpus(path: Path, *, require_audio: bool = True) -> Corpus:
    path = path.resolve()
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise BenchmarkError(f"{path} is not valid JSON: {exc}") from exc

    if not isinstance(data, dict):
        raise BenchmarkError("benchmark corpus must be a JSON object")
    if data.get("version") != 1:
        raise BenchmarkError("benchmark corpus must declare version 1")
    raw_segments = data.get("segments")
    if not isinstance(raw_segments, list) or not raw_segments:
        raise BenchmarkError("benchmark corpus requires a non-empty segments array")

    segments: list[Segment] = []
    seen: set[str] = set()
    base_dir = path.parent
    reserved = {"id", "audio", "expected_text"}
    for index, raw in enumerate(raw_segments, start=1):
        if not isinstance(raw, dict):
            raise BenchmarkError(f"segment {index} must be an object")
        segment_id = raw.get("id")
        if not isinstance(segment_id, str) or not segment_id.strip():
            raise BenchmarkError(f"segment {index} has no id")
        if segment_id in seen:
            raise BenchmarkError(f"duplicate segment id: {segment_id}")
        seen.add(segment_id)

        audio = raw.get("audio")
        if not isinstance(audio, str) or not audio.strip():
            raise BenchmarkError(f"segment {segment_id} has no audio path")
        audio_path = (base_dir / audio).resolve()
        if require_audio and not audio_path.exists():
            raise BenchmarkError(f"segment {segment_id} audio does not exist: {audio_path}")

        expected_text = raw.get("expected_text")
        if not isinstance(expected_text, str) or not normalize_text(expected_text):
            raise BenchmarkError(f"segment {segment_id} needs expected_text")

        duration_ms: float | None = None
        if audio_path.exists():
            channels, sample_rate, duration_ms = read_wav_metadata(audio_path)
            if channels != 1:
                raise BenchmarkError(
                    f"segment {segment_id} must be mono WAV, got {channels} channels"
                )
            if sample_rate <= 0:
                raise BenchmarkError(f"segment {segment_id} has invalid sample rate {sample_rate}")

        metadata = {key: value for key, value in raw.items() if key not in reserved}
        segments.append(
            Segment(
                id=segment_id,
                audio=audio,
                audio_path=audio_path,
                expected_text=expected_text,
                metadata=metadata,
                duration_ms=duration_ms,
            )
        )

    name = data.get("name") if isinstance(data.get("name"), str) else path.stem
    return Corpus(path=path, name=name, version=1, segments=segments)


def load_predictions(path: Path) -> tuple[str | None, dict[str, dict[str, Any]]]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise BenchmarkError(f"{path} is not valid JSON: {exc}") from exc

    model_id = data.get("model_id") if isinstance(data, dict) else None
    if isinstance(data, dict) and "segments" in data:
        raw_segments = data["segments"]
    elif isinstance(data, dict) and "model_id" not in data:
        raw_segments = data
    else:
        raw_segments = data if isinstance(data, list) else None
    predictions: dict[str, dict[str, Any]] = {}

    if isinstance(raw_segments, dict):
        iterator = raw_segments.items()
    elif isinstance(raw_segments, list):
        iterator = []
        for item in raw_segments:
            if not isinstance(item, dict) or "id" not in item:
                raise BenchmarkError("prediction list entries must contain id")
            iterator.append((item["id"], item))
    else:
        raise BenchmarkError("predictions must be an object or list")

    for segment_id, raw_prediction in iterator:
        if not isinstance(segment_id, str) or not segment_id:
            raise BenchmarkError("prediction segment ids must be non-empty strings")
        if isinstance(raw_prediction, str):
            prediction = {"text": raw_prediction}
        elif isinstance(raw_prediction, dict):
            prediction = dict(raw_prediction)
        else:
            raise BenchmarkError(f"prediction for {segment_id} must be a string or object")
        text = prediction.get("text")
        if not isinstance(text, str):
            raise BenchmarkError(f"prediction for {segment_id} needs text")
        predictions[segment_id] = prediction

    return model_id, predictions


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    events: list[dict[str, Any]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip():
            continue
        try:
            raw = json.loads(line)
        except json.JSONDecodeError as exc:
            raise BenchmarkError(f"{path}:{line_number} is not valid JSON: {exc}") from exc
        if isinstance(raw, dict):
            events.append(raw)
    return events


def load_transcripts(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise BenchmarkError(f"{path} is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise BenchmarkError("transcripts must be a JSON object")
    users = data.get("users")
    return users if isinstance(users, dict) else data


def transcript_text(transcripts: dict[str, Any], *, user_id: str, segment_id: str) -> str | None:
    for key in (segment_id, f"user-{user_id}", user_id):
        if key not in transcripts:
            continue
        value = transcripts[key]
        if isinstance(value, str):
            return value
        if isinstance(value, dict) and isinstance(value.get("text"), str):
            return value["text"]
        raise BenchmarkError(f"transcript entry {key} must be a string or object with text")
    return None


def transcript_notes(transcripts: dict[str, Any], *, user_id: str, segment_id: str) -> str | None:
    for key in (segment_id, f"user-{user_id}", user_id):
        value = transcripts.get(key)
        if isinstance(value, dict) and isinstance(value.get("notes"), str):
            return value["notes"]
    return None


def target_key(target: Any) -> str:
    return json.dumps(target, sort_keys=True, separators=(",", ":"))


def aggregate_recording_metadata(events: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    users: dict[str, dict[str, Any]] = {}
    for event in events:
        if event.get("kind") != "ingest_frame" or "user_id" not in event:
            continue
        user_id = str(event["user_id"])
        user = users.setdefault(
            user_id,
            {
                "frames": 0,
                "targets": {},
                "codecs": set(),
                "talk_modes": set(),
                "peak": None,
                "rms_sum": 0.0,
                "rms_count": 0,
                "started_at_ms": None,
                "ended_at_ms": None,
                "user_name": None,
            },
        )
        user["frames"] += 1
        user["user_name"] = event.get("user_name") or user["user_name"]
        if "target" in event:
            user["targets"][target_key(event["target"])] = event["target"]
        if event.get("codec") is not None:
            user["codecs"].add(str(event["codec"]))
        if event.get("talk_mode") is not None:
            user["talk_modes"].add(str(event["talk_mode"]))
        if isinstance(event.get("peak"), (int, float)):
            peak = float(event["peak"])
            user["peak"] = peak if user["peak"] is None else max(user["peak"], peak)
        if isinstance(event.get("rms"), (int, float)):
            user["rms_sum"] += float(event["rms"])
            user["rms_count"] += 1
        if isinstance(event.get("timestamp_ms"), int):
            timestamp_ms = event["timestamp_ms"]
            started_at_ms = user["started_at_ms"]
            ended_at_ms = user["ended_at_ms"]
            user["started_at_ms"] = (
                timestamp_ms if started_at_ms is None else min(started_at_ms, timestamp_ms)
            )
            user["ended_at_ms"] = (
                timestamp_ms if ended_at_ms is None else max(ended_at_ms, timestamp_ms)
            )
    return users


def sorted_wavs(session_dir: Path) -> list[tuple[str, Path]]:
    wavs: list[tuple[str, Path]] = []
    for path in session_dir.glob("user-*.wav"):
        match = re.fullmatch(r"user-(\d+)\.wav", path.name)
        if match:
            wavs.append((match.group(1), path))
    return sorted(wavs, key=lambda item: int(item[0]))


def relative_path(path: Path, base: Path) -> str:
    return os.path.relpath(path, start=base)


def build_corpus_from_recording(
    session_dir: Path,
    *,
    transcripts: dict[str, Any],
    output_path: Path,
    name: str | None,
    device_kind: str,
    device_name: str | None,
    noise_kind: str,
    cleanup_pipeline: str,
    mode: str,
    chunk_ms: int | None,
    overlap_ms: int | None,
    prompt: str | None,
) -> dict[str, Any]:
    session_dir = session_dir.resolve()
    output_path = output_path.resolve()
    metadata_path = session_dir / "metadata.jsonl"
    metadata = aggregate_recording_metadata(load_jsonl(metadata_path))
    wavs = sorted_wavs(session_dir)
    if not wavs:
        raise BenchmarkError(f"no user-*.wav files found in {session_dir}")

    segments: list[dict[str, Any]] = []
    missing_transcripts: list[str] = []
    session_id = session_dir.name
    for user_id, wav_path in wavs:
        segment_id = f"{session_id}-user-{user_id}"
        text = transcript_text(transcripts, user_id=user_id, segment_id=segment_id)
        if text is None or not normalize_text(text):
            missing_transcripts.append(user_id)
            continue
        user = metadata.get(user_id, {})
        codecs = sorted(user.get("codecs", []))
        talk_modes = sorted(user.get("talk_modes", []))
        targets = list(user.get("targets", {}).values())
        rms_count = user.get("rms_count", 0)
        segment: dict[str, Any] = {
            "id": segment_id,
            "audio": relative_path(wav_path, output_path.parent),
            "expected_text": text,
            "device": {"kind": device_kind},
            "route": {"targets": targets},
            "noise": {"kind": noise_kind},
            "cleanup": {"pipeline": cleanup_pipeline},
            "codec": codecs[0] if len(codecs) == 1 else codecs,
            "talk_mode": talk_modes[0] if len(talk_modes) == 1 else talk_modes,
            "gain": {
                "peak_linear": user.get("peak"),
                "rms_linear": (
                    user["rms_sum"] / rms_count
                    if isinstance(rms_count, int) and rms_count > 0
                    else None
                ),
            },
            "segment": {
                "mode": mode,
                "chunk_ms": chunk_ms,
                "overlap_ms": overlap_ms,
                "prompt": prompt,
            },
            "source": {
                "recording_session": session_id,
                "metadata": relative_path(metadata_path, output_path.parent),
                "user_id": int(user_id),
                "user_name": user.get("user_name"),
                "frames": user.get("frames", 0),
                "started_at_ms": user.get("started_at_ms"),
                "ended_at_ms": user.get("ended_at_ms"),
            },
        }
        if device_name:
            segment["device"]["name"] = device_name
        notes = transcript_notes(transcripts, user_id=user_id, segment_id=segment_id)
        if notes:
            segment["notes"] = notes
        segments.append(segment)

    if missing_transcripts:
        raise BenchmarkError(
            "missing ground-truth transcript for user(s): "
            + ", ".join(missing_transcripts)
            + ". Provide keys like user-1, 1, or "
            + f"{session_id}-user-1 in --transcripts."
        )

    return {
        "version": 1,
        "name": name or session_id,
        "segments": segments,
    }


def score_corpus(
    corpus: Corpus,
    predictions: dict[str, dict[str, Any]],
    *,
    model_id: str,
    runtime: str | None = None,
) -> dict[str, Any]:
    missing = [segment.id for segment in corpus.segments if segment.id not in predictions]
    if missing:
        raise BenchmarkError(f"missing predictions for segments: {', '.join(missing)}")

    results: list[dict[str, Any]] = []
    total_word_errors = 0
    total_reference_words = 0
    total_char_errors = 0
    total_reference_chars = 0
    latencies: list[float] = []
    realtime_factors: list[float] = []

    for segment in corpus.segments:
        prediction = predictions[segment.id]
        prediction_text = prediction["text"]
        text_score = score_text(segment.expected_text, prediction_text)
        latency_ms = prediction.get("latency_ms")
        if isinstance(latency_ms, (int, float)):
            latencies.append(float(latency_ms))
            if segment.duration_ms and segment.duration_ms > 0:
                realtime_factors.append(float(latency_ms) / segment.duration_ms)
        else:
            latency_ms = None

        total_word_errors += text_score["word_errors"]
        total_reference_words += text_score["reference_words"]
        total_char_errors += text_score["char_errors"]
        total_reference_chars += text_score["reference_chars"]
        results.append(
            {
                "id": segment.id,
                "audio": segment.audio,
                "duration_ms": segment.duration_ms,
                "expected_text": segment.expected_text,
                "prediction_text": prediction_text,
                "latency_ms": latency_ms,
                "realtime_factor": (
                    float(latency_ms) / segment.duration_ms
                    if latency_ms is not None and segment.duration_ms
                    else None
                ),
                "metadata": segment.metadata,
                "score": text_score,
            }
        )

    summary = {
        "segments": len(results),
        "word_errors": total_word_errors,
        "reference_words": total_reference_words,
        "wer": total_word_errors / total_reference_words if total_reference_words else 0.0,
        "char_errors": total_char_errors,
        "reference_chars": total_reference_chars,
        "cer": total_char_errors / total_reference_chars if total_reference_chars else 0.0,
        "average_latency_ms": sum(latencies) / len(latencies) if latencies else None,
        "average_realtime_factor": (
            sum(realtime_factors) / len(realtime_factors) if realtime_factors else None
        ),
    }
    return {
        "version": 1,
        "corpus": {
            "name": corpus.name,
            "path": str(corpus.path),
            "segments": len(corpus.segments),
        },
        "model_id": model_id,
        "runtime": runtime,
        "summary": summary,
        "segments": results,
    }


def command_predictions(
    corpus: Corpus,
    *,
    command_template: str,
    model_id: str,
    model_path: str | None,
    timeout: float,
) -> dict[str, dict[str, Any]]:
    template_args = shlex.split(command_template)
    if not template_args:
        raise BenchmarkError("command template cannot be empty")

    predictions: dict[str, dict[str, Any]] = {}
    for segment in corpus.segments:
        replacements = {
            "audio": str(segment.audio_path),
            "audio_path": str(segment.audio_path),
            "segment_id": segment.id,
            "model_id": model_id,
            "model_path": model_path or "",
        }
        args = [arg.format(**replacements) for arg in template_args]
        started = time.perf_counter()
        try:
            completed = subprocess.run(
                args,
                check=False,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired as exc:
            raise BenchmarkError(f"{segment.id}: adapter timed out after {timeout:g}s") from exc
        latency_ms = (time.perf_counter() - started) * 1000.0
        if completed.returncode != 0:
            stderr = completed.stderr.strip()
            raise BenchmarkError(
                f"{segment.id}: adapter exited {completed.returncode}: {stderr}"
            )
        transcript = completed.stdout.strip()
        predictions[segment.id] = {"text": transcript, "latency_ms": latency_ms}
    return predictions


def percent(value: float | None) -> str:
    return "-" if value is None else f"{value * 100:.2f}%"


def number(value: float | None, suffix: str = "") -> str:
    return "-" if value is None else f"{value:.1f}{suffix}"


def markdown_escape(value: Any) -> str:
    text = "" if value is None else str(value)
    return text.replace("\n", " ").replace("|", "\\|")


def render_markdown(result: dict[str, Any]) -> str:
    summary = result["summary"]
    lines = [
        f"# Transcription Benchmark: {result['model_id']}",
        "",
        f"Corpus: `{result['corpus']['name']}`",
        "",
        "## Summary",
        "",
        "| Segments | WER | CER | Avg latency | Avg real-time factor |",
        "| ---: | ---: | ---: | ---: | ---: |",
        (
            f"| {summary['segments']} | {percent(summary['wer'])} | "
            f"{percent(summary['cer'])} | {number(summary['average_latency_ms'], ' ms')} | "
            f"{number(summary['average_realtime_factor'], 'x')} |"
        ),
        "",
        "## Segments",
        "",
        "| ID | Device | Noise | Cleanup | WER | CER | Latency | Expected | Prediction |",
        "| --- | --- | --- | --- | ---: | ---: | ---: | --- | --- |",
    ]
    for segment in result["segments"]:
        metadata = segment.get("metadata", {})
        lines.append(
            "| "
            + " | ".join(
                [
                    markdown_escape(segment["id"]),
                    markdown_escape(compact_metadata(metadata.get("device"))),
                    markdown_escape(compact_metadata(metadata.get("noise"))),
                    markdown_escape(compact_metadata(metadata.get("cleanup"))),
                    percent(segment["score"]["wer"]),
                    percent(segment["score"]["cer"]),
                    number(segment.get("latency_ms"), " ms"),
                    markdown_escape(segment["expected_text"]),
                    markdown_escape(segment["prediction_text"]),
                ]
            )
            + " |"
        )
    lines.append("")
    return "\n".join(lines)


def write_outputs(result: dict[str, Any], *, out_json: Path | None, out_md: Path | None) -> None:
    if out_json:
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_json.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if out_md:
        out_md.parent.mkdir(parents=True, exist_ok=True)
        out_md.write_text(render_markdown(result), encoding="utf-8")


def add_common_score_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--out-json", type=Path, help="write machine-readable benchmark results")
    parser.add_argument("--out-md", type=Path, help="write a Markdown benchmark report")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)

    validate = subcommands.add_parser("validate", help="validate corpus schema and WAV files")
    validate.add_argument("corpus", type=Path)

    capture = subcommands.add_parser(
        "capture",
        help="convert a RedLine recording session into a benchmark corpus",
    )
    capture.add_argument("session_dir", type=Path)
    capture.add_argument("--transcripts", required=True, type=Path)
    capture.add_argument("--out", required=True, type=Path)
    capture.add_argument("--name")
    capture.add_argument("--device-kind", default="recording")
    capture.add_argument("--device-name")
    capture.add_argument("--noise-kind", default="unknown")
    capture.add_argument("--cleanup-pipeline", default="recorded_ingest")
    capture.add_argument("--mode", default="reliable")
    capture.add_argument("--chunk-ms", type=int)
    capture.add_argument("--overlap-ms", type=int)
    capture.add_argument("--prompt")

    score = subcommands.add_parser("score", help="score a fixed prediction file")
    score.add_argument("corpus", type=Path)
    score.add_argument("--predictions", required=True, type=Path)
    score.add_argument("--model-id", help="override prediction model_id")
    score.add_argument("--runtime", help="runtime label stored in the result")
    add_common_score_args(score)

    run = subcommands.add_parser("run", help="run a local adapter command and score stdout")
    run.add_argument("corpus", type=Path)
    run.add_argument("--model-id", required=True)
    run.add_argument(
        "--command-template",
        required=True,
        help=(
            "adapter command printed through shlex; placeholders: "
            "{audio}, {audio_path}, {segment_id}, {model_id}, {model_path}"
        ),
    )
    run.add_argument("--model-path")
    run.add_argument("--timeout", type=float, default=300.0)
    add_common_score_args(run)

    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.command == "validate":
        corpus = load_corpus(args.corpus)
        print(f"ok: {corpus.name} has {len(corpus.segments)} mono WAV segment(s)")
        return 0

    if args.command == "capture":
        transcripts = load_transcripts(args.transcripts)
        corpus_data = build_corpus_from_recording(
            args.session_dir,
            transcripts=transcripts,
            output_path=args.out,
            name=args.name,
            device_kind=args.device_kind,
            device_name=args.device_name,
            noise_kind=args.noise_kind,
            cleanup_pipeline=args.cleanup_pipeline,
            mode=args.mode,
            chunk_ms=args.chunk_ms,
            overlap_ms=args.overlap_ms,
            prompt=args.prompt,
        )
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(corpus_data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        corpus = load_corpus(args.out)
        print(f"ok: wrote {args.out} with {len(corpus.segments)} segment(s)")
        return 0

    if args.command == "score":
        corpus = load_corpus(args.corpus)
        prediction_model_id, predictions = load_predictions(args.predictions)
        model_id = args.model_id or prediction_model_id
        if not model_id:
            raise BenchmarkError("model id must be provided by --model-id or predictions.model_id")
        result = score_corpus(
            corpus,
            predictions,
            model_id=model_id,
            runtime=args.runtime,
        )
        write_outputs(result, out_json=args.out_json, out_md=args.out_md)
        print(
            f"ok: {model_id} WER {percent(result['summary']['wer'])}, "
            f"CER {percent(result['summary']['cer'])}"
        )
        return 0

    if args.command == "run":
        corpus = load_corpus(args.corpus)
        predictions = command_predictions(
            corpus,
            command_template=args.command_template,
            model_id=args.model_id,
            model_path=args.model_path,
            timeout=args.timeout,
        )
        result = score_corpus(corpus, predictions, model_id=args.model_id, runtime="command")
        write_outputs(result, out_json=args.out_json, out_md=args.out_md)
        print(
            f"ok: {args.model_id} WER {percent(result['summary']['wer'])}, "
            f"CER {percent(result['summary']['cer'])}"
        )
        return 0

    raise BenchmarkError(f"unknown command {args.command}")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(2)
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
