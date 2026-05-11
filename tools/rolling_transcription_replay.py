#!/usr/bin/env python3
"""Shared rolling-buffer replay utilities for RedLine ASR benchmarks."""

from __future__ import annotations

import argparse
import math
import re
import struct
import tempfile
import time
import unicodedata
import wave
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Protocol


I16_MAX = float(2**15 - 1)
DEFAULT_FRAME_MS = 20


class FileTranscriber(Protocol):
    def transcribe_file(self, path: Path) -> str:
        pass


@dataclass(frozen=True)
class RollingReplayConfig:
    window_ms: int = 8000
    step_ms: int = 1000
    commit_lag_ms: int = 1500
    min_stable_passes: int = 2
    final_pass_on_release: bool = True
    final_pass_scope: str = "utterance"
    max_overlap_words: int = 24
    frame_ms: int = DEFAULT_FRAME_MS
    vad_rms_threshold: float = 0.01
    vad_hangover_ms: int = 600
    vad_min_speech_ms: int = 120
    stale_job_ms: int = 30_000
    queue_limit: int = 8
    drop_busy_partials: bool = True

    def validate(self) -> None:
        if self.window_ms <= 0:
            raise ValueError("window_ms must be positive")
        if self.step_ms <= 0:
            raise ValueError("step_ms must be positive")
        if self.commit_lag_ms < 0:
            raise ValueError("commit_lag_ms must be non-negative")
        if self.min_stable_passes <= 0:
            raise ValueError("min_stable_passes must be positive")
        if self.final_pass_scope not in {"utterance", "window"}:
            raise ValueError("final_pass_scope must be utterance or window")
        if self.stale_job_ms < 0:
            raise ValueError("stale_job_ms must be non-negative")
        if self.queue_limit <= 0:
            raise ValueError("queue_limit must be positive")
        if self.frame_ms <= 0:
            raise ValueError("frame_ms must be positive")

    def public_dict(self) -> dict[str, Any]:
        data = asdict(self)
        data.pop("max_overlap_words", None)
        data.pop("frame_ms", None)
        return data


def load_corpus(path: Path) -> dict[str, Any]:
    import json

    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("segments"), list):
        raise SystemExit(f"{path} is not a benchmark corpus")
    return data


def wav_metadata(path: Path) -> tuple[int, int, int, int, float]:
    with wave.open(str(path), "rb") as handle:
        channels = handle.getnchannels()
        sample_width = handle.getsampwidth()
        sample_rate = handle.getframerate()
        frames = handle.getnframes()
    duration_ms = (frames / sample_rate) * 1000.0 if sample_rate else 0.0
    return channels, sample_width, sample_rate, frames, duration_ms


def wav_duration_ms(path: Path) -> float:
    return wav_metadata(path)[4]


def normalized_words(text: str) -> list[str]:
    normalized = unicodedata.normalize("NFKC", text).lower()
    normalized = re.sub(r"[^a-z0-9]+", " ", normalized)
    return normalized.split()


def common_prefix_word_count(left: str, right: str) -> int:
    left_words = normalized_words(left)
    right_words = normalized_words(right)
    count = 0
    for left_word, right_word in zip(left_words, right_words):
        if left_word != right_word:
            break
        count += 1
    return count


def common_prefix_text(texts: list[str]) -> str:
    if not texts:
        return ""
    raw_words = texts[-1].split()
    normalized = [normalized_words(text) for text in texts]
    prefix_len = min((len(words) for words in normalized), default=0)
    for index in range(prefix_len):
        word = normalized[0][index]
        if any(words[index] != word for words in normalized[1:]):
            prefix_len = index
            break
    return " ".join(raw_words[:prefix_len]).strip()


def trim_commit_lag(text: str, *, commit_lag_ms: int) -> str:
    words = text.split()
    if commit_lag_ms <= 0 or not words:
        return text.strip()
    lag_words = max(1, math.ceil((commit_lag_ms / 1000.0) * 2.5))
    if lag_words >= len(words):
        return ""
    return " ".join(words[:-lag_words]).strip()


def stitch_by_word_overlap(
    existing: str,
    update: str,
    *,
    max_overlap_words: int = 24,
) -> str:
    existing = existing.strip()
    update = update.strip()
    if not existing:
        return update
    if not update:
        return existing

    existing_raw = existing.split()
    update_raw = update.split()
    existing_norm = normalized_words(existing)
    update_norm = normalized_words(update)
    max_overlap = min(max_overlap_words, len(existing_norm), len(update_norm))
    for overlap in range(max_overlap, 0, -1):
        if existing_norm[-overlap:] == update_norm[:overlap]:
            return " ".join([*existing_raw, *update_raw[overlap:]]).strip()
    return " ".join([existing, update]).strip()


def write_chunk(
    source: Path,
    output: Path,
    *,
    start_frame: int,
    frame_count: int,
) -> None:
    with wave.open(str(source), "rb") as reader:
        params = reader.getparams()
        reader.setpos(start_frame)
        frames = reader.readframes(frame_count)
    with wave.open(str(output), "wb") as writer:
        writer.setparams(params)
        writer.writeframes(frames)


def frame_rms_linear(frame_bytes: bytes, sample_count: int) -> float:
    if sample_count <= 0 or not frame_bytes:
        return 0.0
    samples = struct.unpack("<" + "h" * sample_count, frame_bytes)
    square_sum = sum(sample * sample for sample in samples)
    return math.sqrt(square_sum / sample_count) / I16_MAX


def detect_voiced_ranges(
    audio_path: Path,
    *,
    config: RollingReplayConfig,
) -> tuple[list[dict[str, int]], list[dict[str, Any]]]:
    channels, sample_width, sample_rate, total_frames, _duration_ms = wav_metadata(audio_path)
    if channels != 1:
        raise ValueError(f"rolling replay expects mono WAV input, got {channels} channels")
    if sample_width != 2:
        raise ValueError(f"rolling replay expects 16-bit PCM WAV input, got {sample_width} bytes")
    frame_samples = max(1, int(sample_rate * config.frame_ms / 1000.0))
    hangover_frames = max(0, math.ceil(config.vad_hangover_ms / config.frame_ms))
    min_speech_frames = max(1, math.ceil(config.vad_min_speech_ms / config.frame_ms))

    ranges: list[dict[str, int]] = []
    frames: list[dict[str, Any]] = []
    active_start: int | None = None
    voiced_frames = 0
    silence_frames = 0
    frame_index = 0

    with wave.open(str(audio_path), "rb") as reader:
        while True:
            start_frame = frame_index * frame_samples
            if start_frame >= total_frames:
                break
            count = min(frame_samples, total_frames - start_frame)
            raw = reader.readframes(count)
            if not raw:
                break
            rms = frame_rms_linear(raw, count)
            voiced = rms >= config.vad_rms_threshold
            frames.append(
                {
                    "index": frame_index,
                    "start_ms": start_frame / sample_rate * 1000.0,
                    "duration_ms": count / sample_rate * 1000.0,
                    "rms": rms,
                    "voiced": voiced,
                }
            )

            if active_start is None and voiced:
                active_start = start_frame
                voiced_frames = 0
                silence_frames = 0
            if active_start is not None:
                if voiced:
                    voiced_frames += 1
                    silence_frames = 0
                else:
                    silence_frames += 1
                if silence_frames >= hangover_frames and voiced_frames >= min_speech_frames:
                    end_frame = min(total_frames, start_frame + count)
                    ranges.append({"start_frame": active_start, "end_frame": end_frame})
                    active_start = None
                    voiced_frames = 0
                    silence_frames = 0
                elif silence_frames >= hangover_frames:
                    active_start = None
                    voiced_frames = 0
                    silence_frames = 0
            frame_index += 1

    if active_start is not None and voiced_frames >= min_speech_frames:
        ranges.append({"start_frame": active_start, "end_frame": total_frames})
    return ranges, frames


def transcribe_rolling_buffer(
    transcriber: FileTranscriber,
    audio_path: Path,
    *,
    config: RollingReplayConfig,
    temp_prefix: str = "redline-rolling-",
) -> tuple[str, float, list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    config.validate()
    _channels, _sample_width, sample_rate, total_frames, duration_ms = wav_metadata(audio_path)
    voiced_ranges, vad_frames = detect_voiced_ranges(audio_path, config=config)
    if not voiced_ranges:
        return (
            "",
            0.0,
            [],
            [],
            {
                "mode": "rolling-buffer",
                **config.public_dict(),
                "vad_ranges": [],
                "vad_frames": vad_frames,
                "first_token_latency_ms": None,
                "finalization_latency_ms": None,
                "partial_updates": 0,
                "hypothesis_updates": 0,
                "stale_jobs": 0,
                "dropped_jobs": 0,
                "total_compute_ms": 0.0,
                "drop_busy_partials": config.drop_busy_partials,
                "average_emission_lag_ms": None,
                "max_job_queue_delay_ms": 0.0,
                "flicker_ratio": 0.0,
            },
        )

    total_latency_ms = 0.0
    jobs: list[dict[str, Any]] = []
    partials: list[dict[str, Any]] = []
    final_pieces: list[str] = []
    hypothesis_history: list[str] = []
    provisional_text = ""
    committed_text = ""
    worker_available_ms = 0.0
    pending_finishes: list[float] = []
    first_token_latency_ms: float | None = None
    finalization_latency_ms: float | None = None
    emission_lags: list[float] = []
    max_job_queue_delay_ms = 0.0
    stale_jobs = 0
    dropped_jobs = 0
    flicker_revisions = 0
    emitted_words = 0

    def process_window(
        tmp_dir: Path,
        *,
        kind: str,
        range_index: int,
        scheduled_at_ms: float,
        start_frame: int,
        end_frame: int,
    ) -> tuple[str | None, float | None, float | None, float | None]:
        nonlocal worker_available_ms, total_latency_ms, stale_jobs, dropped_jobs
        nonlocal max_job_queue_delay_ms

        pending_finishes[:] = [finish for finish in pending_finishes if finish > scheduled_at_ms]
        if kind == "partial" and len(pending_finishes) >= config.queue_limit:
            dropped_jobs += 1
            jobs.append(
                {
                    "kind": kind,
                    "range_index": range_index,
                    "scheduled_at_ms": scheduled_at_ms,
                    "start_ms": start_frame / sample_rate * 1000.0,
                    "end_ms": end_frame / sample_rate * 1000.0,
                    "dropped": True,
                }
            )
            return None, None, None, None

        process_started_ms = max(float(scheduled_at_ms), worker_available_ms)
        queue_delay_ms = process_started_ms - float(scheduled_at_ms)
        max_job_queue_delay_ms = max(max_job_queue_delay_ms, queue_delay_ms)
        if (
            kind == "partial"
            and config.drop_busy_partials
            and process_started_ms > float(scheduled_at_ms + config.step_ms)
        ):
            dropped_jobs += 1
            jobs.append(
                {
                    "kind": kind,
                    "range_index": range_index,
                    "scheduled_at_ms": scheduled_at_ms,
                    "start_ms": start_frame / sample_rate * 1000.0,
                    "end_ms": end_frame / sample_rate * 1000.0,
                    "queue_delay_ms": queue_delay_ms,
                    "dropped": True,
                    "drop_reason": "busy",
                }
            )
            return None, None, None, None
        if kind == "partial" and queue_delay_ms > config.stale_job_ms:
            stale_jobs += 1
            jobs.append(
                {
                    "kind": kind,
                    "range_index": range_index,
                    "scheduled_at_ms": scheduled_at_ms,
                    "start_ms": start_frame / sample_rate * 1000.0,
                    "end_ms": end_frame / sample_rate * 1000.0,
                    "queue_delay_ms": queue_delay_ms,
                    "stale": True,
                }
            )
            return None, None, None, None

        frame_count = max(0, end_frame - start_frame)
        if frame_count <= 0:
            return None, None, None, None
        chunk_path = tmp_dir / f"{kind}-{range_index:02d}-{len(jobs):04d}.wav"
        write_chunk(audio_path, chunk_path, start_frame=start_frame, frame_count=frame_count)
        started = time.perf_counter()
        text = transcriber.transcribe_file(chunk_path)
        latency_ms = (time.perf_counter() - started) * 1000.0
        total_latency_ms += latency_ms
        emitted_at_ms = process_started_ms + latency_ms
        worker_available_ms = emitted_at_ms
        pending_finishes.append(emitted_at_ms)
        emission_lags.append(emitted_at_ms - scheduled_at_ms)
        jobs.append(
            {
                "kind": kind,
                "range_index": range_index,
                "scheduled_at_ms": scheduled_at_ms,
                "start_ms": start_frame / sample_rate * 1000.0,
                "end_ms": end_frame / sample_rate * 1000.0,
                "duration_ms": frame_count / sample_rate * 1000.0,
                "process_started_ms": process_started_ms,
                "queue_delay_ms": queue_delay_ms,
                "emitted_at_ms": emitted_at_ms,
                "latency_ms": latency_ms,
                "text": text,
            }
        )
        return text, latency_ms, emitted_at_ms, queue_delay_ms

    with tempfile.TemporaryDirectory(prefix=temp_prefix) as tmp:
        tmp_dir = Path(tmp)
        for range_index, voiced_range in enumerate(voiced_ranges):
            range_start_frame = voiced_range["start_frame"]
            range_end_frame = voiced_range["end_frame"]
            range_start_ms = range_start_frame / sample_rate * 1000.0
            range_end_ms = range_end_frame / sample_rate * 1000.0
            next_end_ms = range_start_ms + config.step_ms
            local_history: list[str] = []

            while next_end_ms < range_end_ms:
                window_start_ms = max(range_start_ms, next_end_ms - config.window_ms)
                window_start_frame = int(sample_rate * window_start_ms / 1000.0)
                window_end_frame = int(sample_rate * next_end_ms / 1000.0)
                text, latency_ms, emitted_at_ms, _queue_delay_ms = process_window(
                    tmp_dir,
                    kind="partial",
                    range_index=range_index,
                    scheduled_at_ms=next_end_ms,
                    start_frame=window_start_frame,
                    end_frame=window_end_frame,
                )
                if text is not None and emitted_at_ms is not None:
                    previous_provisional = provisional_text
                    if window_start_frame <= range_start_frame:
                        provisional_text = text.strip()
                    else:
                        provisional_text = stitch_by_word_overlap(
                            committed_text or provisional_text,
                            text,
                            max_overlap_words=config.max_overlap_words,
                        )
                    if previous_provisional:
                        stable_prefix_words = common_prefix_word_count(
                            previous_provisional,
                            provisional_text,
                        )
                        flicker_revisions += max(
                            0,
                            len(normalized_words(previous_provisional)) - stable_prefix_words,
                        )
                    emitted_words += len(normalized_words(provisional_text))
                    local_history.append(provisional_text)
                    hypothesis_history.append(provisional_text)
                    if len(local_history) >= config.min_stable_passes:
                        stable_text = common_prefix_text(local_history[-config.min_stable_passes:])
                        stable_text = trim_commit_lag(
                            stable_text,
                            commit_lag_ms=config.commit_lag_ms,
                        )
                        if len(normalized_words(stable_text)) > len(normalized_words(committed_text)):
                            committed_text = stable_text
                            partial = {
                                "kind": "partial",
                                "text": committed_text,
                                "audio_end_ms": next_end_ms,
                                "emitted_at_ms": emitted_at_ms,
                                "latency_ms": latency_ms,
                                "stable_passes": config.min_stable_passes,
                            }
                            partials.append(partial)
                            if first_token_latency_ms is None and normalized_words(committed_text):
                                first_token_latency_ms = emitted_at_ms - range_start_ms
                next_end_ms += config.step_ms

            if config.final_pass_on_release:
                if config.final_pass_scope == "utterance":
                    final_start_frame = range_start_frame
                else:
                    final_start_ms = max(range_start_ms, range_end_ms - config.window_ms)
                    final_start_frame = int(sample_rate * final_start_ms / 1000.0)
                text, latency_ms, emitted_at_ms, _queue_delay_ms = process_window(
                    tmp_dir,
                    kind="final",
                    range_index=range_index,
                    scheduled_at_ms=range_end_ms,
                    start_frame=final_start_frame,
                    end_frame=range_end_frame,
                )
                if text is not None and emitted_at_ms is not None:
                    final_pieces.append(text)
                    final_text_so_far = stitch_by_word_overlap(
                        " ".join(final_pieces[:-1]),
                        text,
                        max_overlap_words=config.max_overlap_words,
                    )
                    partials.append(
                        {
                            "kind": "final",
                            "text": final_text_so_far,
                            "audio_end_ms": range_end_ms,
                            "emitted_at_ms": emitted_at_ms,
                            "latency_ms": latency_ms,
                            "stable_passes": config.min_stable_passes,
                        }
                    )
                    if first_token_latency_ms is None and normalized_words(text):
                        first_token_latency_ms = emitted_at_ms - range_start_ms
                    finalization_latency_ms = emitted_at_ms - range_end_ms
            else:
                final_pieces.append(committed_text or provisional_text)

    if config.final_pass_on_release:
        final_text = ""
        for piece in final_pieces:
            final_text = stitch_by_word_overlap(
                final_text,
                piece,
                max_overlap_words=config.max_overlap_words,
            )
    else:
        final_text = committed_text or provisional_text

    metric_ranges = [
        {
            "start_ms": item["start_frame"] / sample_rate * 1000.0,
            "end_ms": item["end_frame"] / sample_rate * 1000.0,
        }
        for item in voiced_ranges
    ]
    metrics = {
        "mode": "rolling-buffer",
        **config.public_dict(),
        "vad_ranges": metric_ranges,
        "vad_frame_count": len(vad_frames),
        "first_token_latency_ms": first_token_latency_ms,
        "finalization_latency_ms": finalization_latency_ms,
        "final_emitted_at_ms": (
            partials[-1]["emitted_at_ms"] if partials and partials[-1]["kind"] == "final" else None
        ),
        "endpoint_ms": metric_ranges[-1]["end_ms"] if metric_ranges else duration_ms,
        "partial_updates": len([partial for partial in partials if partial["kind"] == "partial"]),
        "hypothesis_updates": len(hypothesis_history),
        "stale_jobs": stale_jobs,
        "dropped_jobs": dropped_jobs,
        "drop_busy_partials": config.drop_busy_partials,
        "total_compute_ms": total_latency_ms,
        "average_emission_lag_ms": (
            sum(emission_lags) / len(emission_lags) if emission_lags else None
        ),
        "max_job_queue_delay_ms": max_job_queue_delay_ms,
        "flicker_ratio": flicker_revisions / emitted_words if emitted_words else 0.0,
        "audio_duration_ms": duration_ms,
        "audio_frames": total_frames,
    }
    return final_text.strip(), total_latency_ms, jobs, partials, metrics


def add_rolling_replay_args(parser: Any) -> None:
    parser.add_argument("--window-ms", type=int, default=8000)
    parser.add_argument("--step-ms", type=int, default=1000)
    parser.add_argument("--commit-lag-ms", type=int, default=1500)
    parser.add_argument("--min-stable-passes", type=int, default=2)
    parser.add_argument("--final-pass-on-release", action="store_true", default=True)
    parser.add_argument("--no-final-pass-on-release", dest="final_pass_on_release", action="store_false")
    parser.add_argument("--final-pass-scope", choices=["utterance", "window"], default="utterance")
    parser.add_argument("--max-overlap-words", type=int, default=24)
    parser.add_argument("--frame-ms", type=int, default=DEFAULT_FRAME_MS)
    parser.add_argument("--vad-rms-threshold", type=float, default=0.01)
    parser.add_argument("--vad-hangover-ms", type=int, default=600)
    parser.add_argument("--vad-min-speech-ms", type=int, default=120)
    parser.add_argument("--stale-job-ms", type=int, default=30_000)
    parser.add_argument("--queue-limit", type=int, default=8)
    parser.add_argument("--drop-busy-partials", action=argparse.BooleanOptionalAction, default=True)


def config_from_args(args: Any) -> RollingReplayConfig:
    return RollingReplayConfig(
        window_ms=args.window_ms,
        step_ms=args.step_ms,
        commit_lag_ms=args.commit_lag_ms,
        min_stable_passes=args.min_stable_passes,
        final_pass_on_release=args.final_pass_on_release,
        final_pass_scope=args.final_pass_scope,
        max_overlap_words=args.max_overlap_words,
        frame_ms=args.frame_ms,
        vad_rms_threshold=args.vad_rms_threshold,
        vad_hangover_ms=args.vad_hangover_ms,
        vad_min_speech_ms=args.vad_min_speech_ms,
        stale_job_ms=args.stale_job_ms,
        queue_limit=args.queue_limit,
        drop_busy_partials=args.drop_busy_partials,
    )
