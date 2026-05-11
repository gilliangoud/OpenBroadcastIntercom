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
import re
import struct
import sys
import tempfile
import time
import unicodedata
import wave
from pathlib import Path
from typing import Any


I16_MAX = float(2**15 - 1)
DEFAULT_FRAME_MS = 20


def load_corpus(path: Path) -> dict[str, Any]:
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
    frame_ms: int,
    vad_rms_threshold: float,
    vad_hangover_ms: int,
    vad_min_speech_ms: int,
) -> tuple[list[dict[str, int]], list[dict[str, Any]]]:
    channels, sample_width, sample_rate, total_frames, _duration_ms = wav_metadata(audio_path)
    if channels != 1:
        raise ValueError(f"Moonshine benchmark expects mono WAV input, got {channels} channels")
    if sample_width != 2:
        raise ValueError(f"Moonshine benchmark expects 16-bit PCM WAV input, got {sample_width} bytes")
    frame_samples = max(1, int(sample_rate * frame_ms / 1000.0))
    hangover_frames = max(0, math.ceil(vad_hangover_ms / frame_ms))
    min_speech_frames = max(1, math.ceil(vad_min_speech_ms / frame_ms))

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
            voiced = rms >= vad_rms_threshold
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

    channels, sample_width, sample_rate, total_frames, _duration_ms = wav_metadata(audio_path)
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
            write_chunk(audio_path, chunk_path, start_frame=start_frame, frame_count=frame_count)
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


def transcribe_rolling_buffer(
    backend: MoonshineBackend,
    audio_path: Path,
    *,
    window_ms: int,
    step_ms: int,
    commit_lag_ms: int,
    min_stable_passes: int,
    final_pass_on_release: bool,
    final_pass_scope: str,
    max_overlap_words: int,
    vad_rms_threshold: float,
    vad_hangover_ms: int,
    vad_min_speech_ms: int,
    stale_job_ms: int,
    queue_limit: int,
    frame_ms: int,
) -> tuple[str, float, list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    if window_ms <= 0:
        raise ValueError("window_ms must be positive")
    if step_ms <= 0:
        raise ValueError("step_ms must be positive")
    if commit_lag_ms < 0:
        raise ValueError("commit_lag_ms must be non-negative")
    if min_stable_passes <= 0:
        raise ValueError("min_stable_passes must be positive")
    if final_pass_scope not in {"utterance", "window"}:
        raise ValueError("final_pass_scope must be utterance or window")
    if stale_job_ms < 0:
        raise ValueError("stale_job_ms must be non-negative")
    if queue_limit <= 0:
        raise ValueError("queue_limit must be positive")
    if frame_ms <= 0:
        raise ValueError("frame_ms must be positive")

    _channels, _sample_width, sample_rate, total_frames, duration_ms = wav_metadata(audio_path)
    voiced_ranges, vad_frames = detect_voiced_ranges(
        audio_path,
        frame_ms=frame_ms,
        vad_rms_threshold=vad_rms_threshold,
        vad_hangover_ms=vad_hangover_ms,
        vad_min_speech_ms=vad_min_speech_ms,
    )
    if not voiced_ranges:
        return (
            "",
            0.0,
            [],
            [],
            {
                "mode": "rolling-buffer",
                "window_ms": window_ms,
                "step_ms": step_ms,
                "commit_lag_ms": commit_lag_ms,
                "min_stable_passes": min_stable_passes,
                "final_pass_on_release": final_pass_on_release,
                "final_pass_scope": final_pass_scope,
                "vad_rms_threshold": vad_rms_threshold,
                "vad_hangover_ms": vad_hangover_ms,
                "vad_min_speech_ms": vad_min_speech_ms,
                "vad_ranges": [],
                "vad_frames": vad_frames,
                "first_token_latency_ms": None,
                "finalization_latency_ms": None,
                "partial_updates": 0,
                "hypothesis_updates": 0,
                "stale_jobs": 0,
                "dropped_jobs": 0,
                "total_compute_ms": 0.0,
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
        if kind == "partial" and len(pending_finishes) >= queue_limit:
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
        if kind == "partial" and queue_delay_ms > stale_job_ms:
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
        text = backend.transcribe_file(chunk_path)
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

    with tempfile.TemporaryDirectory(prefix="redline-moonshine-rolling-") as tmp:
        tmp_dir = Path(tmp)
        for range_index, voiced_range in enumerate(voiced_ranges):
            range_start_frame = voiced_range["start_frame"]
            range_end_frame = voiced_range["end_frame"]
            range_start_ms = range_start_frame / sample_rate * 1000.0
            range_end_ms = range_end_frame / sample_rate * 1000.0
            next_end_ms = range_start_ms + step_ms
            local_history: list[str] = []

            while next_end_ms < range_end_ms:
                window_start_ms = max(range_start_ms, next_end_ms - window_ms)
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
                            max_overlap_words=max_overlap_words,
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
                    if len(local_history) >= min_stable_passes:
                        stable_text = common_prefix_text(local_history[-min_stable_passes:])
                        stable_text = trim_commit_lag(
                            stable_text,
                            commit_lag_ms=commit_lag_ms,
                        )
                        if len(normalized_words(stable_text)) > len(normalized_words(committed_text)):
                            committed_text = stable_text
                            partial = {
                                "kind": "partial",
                                "text": committed_text,
                                "audio_end_ms": next_end_ms,
                                "emitted_at_ms": emitted_at_ms,
                                "latency_ms": latency_ms,
                                "stable_passes": min_stable_passes,
                            }
                            partials.append(partial)
                            if first_token_latency_ms is None and normalized_words(committed_text):
                                first_token_latency_ms = emitted_at_ms - range_start_ms
                next_end_ms += step_ms

            if final_pass_on_release:
                if final_pass_scope == "utterance":
                    final_start_frame = range_start_frame
                else:
                    final_start_ms = max(range_start_ms, range_end_ms - window_ms)
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
                        max_overlap_words=max_overlap_words,
                    )
                    partials.append(
                        {
                            "kind": "final",
                            "text": final_text_so_far,
                            "audio_end_ms": range_end_ms,
                            "emitted_at_ms": emitted_at_ms,
                            "latency_ms": latency_ms,
                            "stable_passes": min_stable_passes,
                        }
                    )
                    if first_token_latency_ms is None and normalized_words(text):
                        first_token_latency_ms = emitted_at_ms - range_start_ms
                    finalization_latency_ms = emitted_at_ms - range_end_ms
            else:
                final_pieces.append(committed_text or provisional_text)

    if final_pass_on_release:
        final_text = ""
        for piece in final_pieces:
            final_text = stitch_by_word_overlap(
                final_text,
                piece,
                max_overlap_words=max_overlap_words,
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
        "window_ms": window_ms,
        "step_ms": step_ms,
        "commit_lag_ms": commit_lag_ms,
        "min_stable_passes": min_stable_passes,
        "final_pass_on_release": final_pass_on_release,
        "final_pass_scope": final_pass_scope,
        "frame_ms": frame_ms,
        "vad_rms_threshold": vad_rms_threshold,
        "vad_hangover_ms": vad_hangover_ms,
        "vad_min_speech_ms": vad_min_speech_ms,
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
        "stale_job_ms": stale_job_ms,
        "queue_limit": queue_limit,
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


def transcribe_corpus(args: argparse.Namespace) -> dict[str, Any]:
    backend = build_backend(args)
    corpus = load_corpus(args.corpus)
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
            text, latency_ms, chunks, partials, live_metrics = transcribe_rolling_buffer(
                backend,
                audio_path,
                window_ms=args.window_ms,
                step_ms=args.step_ms,
                commit_lag_ms=args.commit_lag_ms,
                min_stable_passes=args.min_stable_passes,
                final_pass_on_release=args.final_pass_on_release,
                final_pass_scope=args.final_pass_scope,
                max_overlap_words=args.max_overlap_words,
                vad_rms_threshold=args.vad_rms_threshold,
                vad_hangover_ms=args.vad_hangover_ms,
                vad_min_speech_ms=args.vad_min_speech_ms,
                stale_job_ms=args.stale_job_ms,
                queue_limit=args.queue_limit,
                frame_ms=args.frame_ms,
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
                "window_ms": args.window_ms,
                "step_ms": args.step_ms,
                "commit_lag_ms": args.commit_lag_ms,
                "min_stable_passes": args.min_stable_passes,
                "final_pass_on_release": args.final_pass_on_release,
                "final_pass_scope": args.final_pass_scope,
                "vad_rms_threshold": args.vad_rms_threshold,
                "vad_hangover_ms": args.vad_hangover_ms,
                "vad_min_speech_ms": args.vad_min_speech_ms,
                "stale_job_ms": args.stale_job_ms,
                "queue_limit": args.queue_limit,
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
    parser.add_argument("--window-ms", type=int, default=8000)
    parser.add_argument("--step-ms", type=int, default=1000)
    parser.add_argument("--commit-lag-ms", type=int, default=1500)
    parser.add_argument("--min-stable-passes", type=int, default=2)
    parser.add_argument("--final-pass-on-release", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--final-pass-scope", choices=["utterance", "window"], default="utterance")
    parser.add_argument("--max-overlap-words", type=int, default=24)
    parser.add_argument("--frame-ms", type=int, default=DEFAULT_FRAME_MS)
    parser.add_argument("--vad-rms-threshold", type=float, default=0.01)
    parser.add_argument("--vad-hangover-ms", type=int, default=600)
    parser.add_argument("--vad-min-speech-ms", type=int, default=120)
    parser.add_argument("--stale-job-ms", type=int, default=30_000)
    parser.add_argument("--queue-limit", type=int, default=8)
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
