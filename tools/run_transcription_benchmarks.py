#!/usr/bin/env python3
"""Run RedLine transcription benchmarks for installed local models."""

from __future__ import annotations

import argparse
import json
import resource
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import transcription_benchmark as tb
import rolling_transcription_replay as replay


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "intercom-models" / "manifest.json"
DEFAULT_OUT_DIR = ROOT / "artifacts" / "transcription-benchmarks" / "macos-smoke"
DEFAULT_ADAPTER = ROOT / "target" / "release" / "transcription_benchmark_whisper"
MACOS_SMOKE_SEGMENTS = [
    (
        "macos-say-referee-001",
        "Ref one check, clock is stopped at twelve seconds.",
        "referee check",
    ),
    (
        "macos-say-operator-002",
        "Operator to sideline, hold the restart until the signal.",
        "operator instruction",
    ),
    (
        "macos-say-penalty-003",
        "Penalty confirmed on blue twenty four, resume play on whistle.",
        "penalty confirmation",
    ),
]
LIBRISPEECH_SMOKE_BASE_URL = (
    "https://huggingface.co/desh2608/"
    "icefall-asr-librispeech-pruned-transducer-stateless7-streaming-small-rvb"
    "/resolve/main/test_wavs"
)
LIBRISPEECH_SMOKE_FILES = [
    "1089-134686-0001.wav",
    "1221-135766-0001.wav",
    "1221-135766-0002.wav",
    "trans.txt",
]


class SuiteError(Exception):
    pass


def load_manifest(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def model_path(model: dict[str, Any]) -> Path:
    return ROOT / model["destination_dir"] / model["filename"]


def installed_transcription_models(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        model
        for model in manifest.get("models", [])
        if model.get("category") == "transcription" and model_path(model).exists()
    ]


def generate_macos_say_corpus(out_dir: Path) -> Path:
    say = Path("/usr/bin/say")
    ffmpeg = shutil.which("ffmpeg")
    if not say.exists():
        raise SuiteError("macOS say command is not available")
    if not ffmpeg:
        raise SuiteError("ffmpeg is required to convert macOS say output to WAV")

    audio_dir = out_dir / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    segments: list[dict[str, Any]] = []
    for segment_id, text, note in MACOS_SMOKE_SEGMENTS:
        aiff_path = audio_dir / f"{segment_id}.aiff"
        wav_path = audio_dir / f"{segment_id}.wav"
        subprocess.run(
            [str(say), "-o", str(aiff_path), text],
            check=True,
            capture_output=True,
            text=True,
        )
        subprocess.run(
            [ffmpeg, "-y", "-hide_banner", "-loglevel", "error", "-i", str(aiff_path), "-ac", "1", "-ar", "16000", str(wav_path)],
            check=True,
            capture_output=True,
            text=True,
        )
        aiff_path.unlink(missing_ok=True)
        segments.append(
            {
                "id": segment_id,
                "audio": f"audio/{wav_path.name}",
                "expected_text": text,
                "device": {"kind": "macos_say", "name": "macOS synthetic voice"},
                "route": {"channel": "sports-comms"},
                "noise": {"kind": "clean_synthetic"},
                "cleanup": {"pipeline": "none"},
                "codec": "pcm16",
                "segment": {
                    "mode": "reliable",
                    "prompt": "Sports officiating intercom.",
                },
                "notes": note,
            }
        )

    corpus_path = out_dir / "corpus.json"
    corpus_path.write_text(
        json.dumps(
            {
                "version": 1,
                "name": "macos-say-sports-smoke",
                "segments": segments,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    tb.load_corpus(corpus_path)
    return corpus_path


def download_file(url: str, destination: Path, *, refresh: bool) -> None:
    if destination.exists() and destination.stat().st_size > 0 and not refresh:
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(url, headers={"User-Agent": "RedLine-transcription-benchmark/1.0"})
    with urllib.request.urlopen(request, timeout=60.0) as response:
        destination.write_bytes(response.read())


def download_librispeech_smoke_corpus(out_dir: Path, *, refresh: bool) -> Path:
    audio_dir = out_dir / "audio"
    for filename in LIBRISPEECH_SMOKE_FILES:
        destination = out_dir / filename if filename == "trans.txt" else audio_dir / filename
        download_file(f"{LIBRISPEECH_SMOKE_BASE_URL}/{filename}", destination, refresh=refresh)

    transcripts: dict[str, str] = {}
    for line in (out_dir / "trans.txt").read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        segment_id, text = line.split(" ", 1)
        transcripts[segment_id] = text

    segments = []
    for filename in sorted(path.name for path in audio_dir.glob("*.wav")):
        segment_id = filename.removesuffix(".wav")
        expected_text = transcripts.get(segment_id)
        if not expected_text:
            raise SuiteError(f"missing transcript for {filename}")
        segments.append(
            {
                "id": segment_id,
                "audio": f"audio/{filename}",
                "expected_text": expected_text,
                "device": {"kind": "online_fixture", "name": "LibriSpeech test_wavs"},
                "route": {"channel": "read-speech"},
                "noise": {"kind": "clean_read_speech"},
                "cleanup": {"pipeline": "none"},
                "codec": "pcm16",
                "segment": {
                    "mode": "reliable",
                    "prompt": "Clean read English speech.",
                },
            }
        )

    corpus_path = out_dir / "corpus.json"
    corpus_path.write_text(
        json.dumps({"version": 1, "name": "hf-librispeech-test-wavs", "segments": segments}, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    tb.load_corpus(corpus_path)
    return corpus_path


def build_adapter(features: str) -> None:
    subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "server",
            "--release",
            "--features",
            features,
            "--bin",
            "transcription_benchmark_whisper",
        ],
        cwd=ROOT,
        check=True,
    )


def sample_process(pid: int) -> tuple[int | None, float | None]:
    completed = subprocess.run(
        ["ps", "-o", "rss=,%cpu=", "-p", str(pid)],
        check=False,
        capture_output=True,
        text=True,
    )
    line = completed.stdout.strip()
    if not line:
        return None, None
    parts = line.split()
    if len(parts) < 2:
        return None, None
    try:
        return int(float(parts[0])), float(parts[1])
    except ValueError:
        return None, None


def parse_macos_time_l(stderr: str) -> dict[str, Any]:
    metrics: dict[str, Any] = {}
    for line in stderr.splitlines():
        parts = line.strip().split()
        if len(parts) < 2:
            continue
        if parts[-4:] == ["maximum", "resident", "set", "size"]:
            try:
                metrics["max_rss_kb"] = int(float(parts[0]) / 1024.0)
            except ValueError:
                pass
        for index, part in enumerate(parts):
            if part == "user" and index > 0:
                try:
                    metrics["user_time_ms"] = float(parts[index - 1]) * 1000.0
                except ValueError:
                    pass
            elif part == "sys" and index > 0:
                try:
                    metrics["system_time_ms"] = float(parts[index - 1]) * 1000.0
                except ValueError:
                    pass
    if "user_time_ms" in metrics or "system_time_ms" in metrics:
        metrics["child_cpu_time_ms"] = metrics.get("user_time_ms", 0.0) + metrics.get("system_time_ms", 0.0)
    return metrics


def run_profiled(args: list[str], *, timeout: float, sample_interval: float) -> dict[str, Any]:
    started = time.perf_counter()
    time_l = Path("/usr/bin/time")
    if time_l.exists():
        process = subprocess.Popen(
            [str(time_l), "-l", *args],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            process.kill()
            stdout, stderr = process.communicate()
            raise SuiteError(f"adapter timed out after {timeout:g}s: {stderr.strip() or stdout.strip()}") from exc
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        metrics = parse_macos_time_l(stderr)
        time_sysctl_blocked = "time: sysctl kern.clockrate: Operation not permitted" in stderr
        if process.returncode != 0 and not time_sysctl_blocked:
            raise SuiteError(f"adapter exited {process.returncode}: {stderr.strip() or stdout.strip()}")
        return {
            "elapsed_ms": elapsed_ms,
            "max_rss_kb": metrics.get("max_rss_kb"),
            "max_cpu_percent": None,
            "child_cpu_time_ms": metrics.get("child_cpu_time_ms"),
            "profile_method": "time -l" if not time_sysctl_blocked else "time -l partial",
            "samples": None,
            "stdout": stdout,
            "stderr": stderr,
        }

    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    process = subprocess.Popen(args, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    max_rss_kb = 0
    max_cpu_percent = 0.0
    samples = 0
    ps_available = True
    while process.poll() is None:
        elapsed = time.perf_counter() - started
        if elapsed > timeout:
            process.kill()
            stdout, stderr = process.communicate()
            raise SuiteError(f"adapter timed out after {timeout:g}s: {stderr.strip() or stdout.strip()}")
        if ps_available:
            try:
                rss_kb, cpu_percent = sample_process(process.pid)
            except PermissionError:
                ps_available = False
            else:
                if rss_kb is not None:
                    max_rss_kb = max(max_rss_kb, rss_kb)
                if cpu_percent is not None:
                    max_cpu_percent = max(max_cpu_percent, cpu_percent)
                samples += 1
        time.sleep(sample_interval)
    stdout, stderr = process.communicate()
    usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    if process.returncode != 0:
        raise SuiteError(f"adapter exited {process.returncode}: {stderr.strip() or stdout.strip()}")
    cpu_time_ms = (
        (usage_after.ru_utime - usage_before.ru_utime)
        + (usage_after.ru_stime - usage_before.ru_stime)
    ) * 1000.0
    fallback_max_rss_kb = usage_after.ru_maxrss if usage_after.ru_maxrss else None
    return {
        "elapsed_ms": elapsed_ms,
        "max_rss_kb": max_rss_kb or fallback_max_rss_kb,
        "max_cpu_percent": max_cpu_percent or None,
        "child_cpu_time_ms": cpu_time_ms,
        "profile_method": "ps" if ps_available and samples else "resource",
        "samples": samples,
        "stdout": stdout,
        "stderr": stderr,
    }


def enrich_result(
    result: dict[str, Any],
    *,
    model: dict[str, Any],
    predictions: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    path = model_path(model)
    result["model"] = {
        "id": model["id"],
        "name": model.get("name"),
        "runtime": model.get("runtime"),
        "path": str(path),
        "size_bytes": path.stat().st_size if path.exists() else None,
        "manifest_size": model.get("size"),
        "recommended": model.get("recommended", False),
        "default": model.get("default", False),
    }
    result["adapter"] = {
        "runtime": predictions.get("runtime"),
        "backend": predictions.get("backend"),
        "model_load_ms": predictions.get("model_load_ms"),
        "mode": predictions.get("mode"),
        "replay_mode": predictions.get("replay_mode"),
        "rolling_buffer": predictions.get("rolling_buffer"),
        "language": predictions.get("language"),
        "threads": predictions.get("threads"),
    }
    result["macos_profile"] = {
        key: profile[key]
        for key in (
            "elapsed_ms",
            "max_rss_kb",
            "max_cpu_percent",
            "child_cpu_time_ms",
            "profile_method",
            "samples",
        )
    }
    return result


def basic_model_info(model: dict[str, Any]) -> dict[str, Any]:
    path = model_path(model)
    return {
        "id": model["id"],
        "name": model.get("name"),
        "runtime": model.get("runtime"),
        "path": str(path),
        "size_bytes": path.stat().st_size if path.exists() else None,
        "manifest_size": model.get("size"),
        "recommended": model.get("recommended", False),
        "default": model.get("default", False),
    }


def markdown_number(value: Any, suffix: str = "") -> str:
    if value is None:
        return "-"
    if isinstance(value, int):
        return f"{value}{suffix}"
    if isinstance(value, float):
        return f"{value:.1f}{suffix}"
    return str(value)


def render_summary(results: list[dict[str, Any]]) -> str:
    lines = [
        "# macOS Transcription Benchmark Summary",
        "",
        "| Model | Backend | Replay | WER | CER | Avg latency | Realtime | First token | Finalize | Partials | Stale/drop | Load | Max RSS | CPU time |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for result in results:
        if result.get("error"):
            lines.append(
                "| "
                + " | ".join(
                    [
                        result["model"]["id"],
                        "failed",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        "-",
                        tb.markdown_escape(result["error"]),
                    ]
                )
                + " |"
            )
            continue
        summary = result["summary"]
        backend = result.get("adapter", {}).get("backend") or {}
        backend_label = "metal" if backend.get("metal") else "cpu"
        if backend.get("coreml"):
            backend_label += "+coreml"
        live_summary = summary.get("live") or {}
        stale_drop = "-"
        if live_summary:
            stale_drop = f"{live_summary.get('stale_jobs', 0)}/{live_summary.get('dropped_jobs', 0)}"
        rss_kb = result.get("macos_profile", {}).get("max_rss_kb")
        rss_mb = rss_kb / 1024.0 if isinstance(rss_kb, int) else None
        lines.append(
            "| "
            + " | ".join(
                [
                    result["model"]["id"],
                    backend_label,
                    tb.markdown_escape(result.get("adapter", {}).get("replay_mode")),
                    tb.percent(summary["wer"]),
                    tb.percent(summary["cer"]),
                    markdown_number(summary["average_latency_ms"], " ms"),
                    markdown_number(summary["average_realtime_factor"], "x"),
                    markdown_number(live_summary.get("average_first_token_latency_ms"), " ms"),
                    markdown_number(live_summary.get("average_finalization_latency_ms"), " ms"),
                    markdown_number(live_summary.get("partial_updates")),
                    stale_drop,
                    markdown_number(result.get("adapter", {}).get("model_load_ms"), " ms"),
                    markdown_number(rss_mb, " MB"),
                    markdown_number(result.get("macos_profile", {}).get("child_cpu_time_ms"), " ms"),
                ]
            )
            + " |"
        )
    lines.append("")
    return "\n".join(lines)


def run_suite(args: argparse.Namespace) -> int:
    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    if args.generate_macos_say_corpus:
        corpus_path = generate_macos_say_corpus(out_dir)
    elif args.download_librispeech_smoke_corpus:
        corpus_path = download_librispeech_smoke_corpus(out_dir, refresh=args.refresh_downloads)
    elif args.corpus:
        corpus_path = args.corpus.resolve()
    else:
        raise SuiteError("provide --corpus or --generate-macos-say-corpus")

    if args.build_adapter:
        build_adapter(args.features)
    adapter = args.adapter.resolve()
    if not adapter.exists():
        raise SuiteError(f"adapter binary does not exist: {adapter}. Rerun with --build-adapter.")

    manifest = load_manifest(args.manifest)
    models = installed_transcription_models(manifest)
    if args.models:
        wanted = set(args.models.split(","))
        models = [model for model in models if model["id"] in wanted]
    if not models:
        raise SuiteError("no installed transcription models matched")

    results: list[dict[str, Any]] = []
    for model in models:
        predictions_path = out_dir / f"{model['id']}-predictions.json"
        result_json = out_dir / f"{model['id']}-result.json"
        result_md = out_dir / f"{model['id']}-result.md"
        command = [
            str(adapter),
            "--corpus",
            str(corpus_path),
            "--model",
            str(model_path(model)),
            "--model-id",
            model["id"],
            "--out",
            str(predictions_path),
            "--mode",
            args.mode,
            "--replay-mode",
            args.replay_mode,
            "--language",
            args.language,
            "--prompt",
            args.prompt,
        ]
        command.extend(
            [
                "--window-ms",
                str(args.window_ms),
                "--step-ms",
                str(args.step_ms),
                "--commit-lag-ms",
                str(args.commit_lag_ms),
                "--min-stable-passes",
                str(args.min_stable_passes),
                "--final-pass-on-release",
                str(args.final_pass_on_release).lower(),
                "--final-pass-scope",
                args.final_pass_scope,
                "--vad-rms-threshold",
                str(args.vad_rms_threshold),
                "--vad-hangover-ms",
                str(args.vad_hangover_ms),
                "--vad-min-speech-ms",
                str(args.vad_min_speech_ms),
                "--stale-job-ms",
                str(args.stale_job_ms),
                "--queue-limit",
                str(args.queue_limit),
            ]
        )
        print(f"running {model['id']}...", flush=True)
        try:
            profile = run_profiled(command, timeout=args.timeout, sample_interval=args.sample_interval)
        except SuiteError as exc:
            results.append(
                {
                    "model": basic_model_info(model),
                    "error": str(exc),
                }
            )
            print(f"failed {model['id']}: {exc}", file=sys.stderr, flush=True)
            continue
        if not predictions_path.exists():
            results.append(
                {
                    "model": basic_model_info(model),
                    "error": "adapter did not write predictions",
                }
            )
            continue
        predictions_raw = json.loads(predictions_path.read_text(encoding="utf-8"))
        prediction_model_id, predictions = tb.load_predictions(predictions_path)
        corpus = tb.load_corpus(corpus_path)
        result = tb.score_corpus(
            corpus,
            predictions,
            model_id=prediction_model_id or model["id"],
            runtime=predictions_raw.get("runtime"),
        )
        result = enrich_result(result, model=model, predictions=predictions_raw, profile=profile)
        tb.write_outputs(result, out_json=result_json, out_md=result_md)
        results.append(result)

    summary = {
        "corpus": str(corpus_path),
        "features": args.features,
        "mode": args.mode,
        "replay_mode": args.replay_mode,
        "rolling_buffer": (
            replay.config_from_args(args).public_dict()
            if args.replay_mode == "rolling-buffer"
            else None
        ),
        "language": args.language,
        "results": results,
    }
    (out_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    (out_dir / "summary.md").write_text(render_summary(results), encoding="utf-8")
    print(f"wrote {out_dir / 'summary.md'}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--corpus", type=Path)
    parser.add_argument("--generate-macos-say-corpus", action="store_true")
    parser.add_argument("--download-librispeech-smoke-corpus", action="store_true")
    parser.add_argument("--refresh-downloads", action="store_true")
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument("--models", help="comma-separated model ids; default is all installed transcription models")
    parser.add_argument("--adapter", type=Path, default=DEFAULT_ADAPTER)
    parser.add_argument("--build-adapter", action="store_true")
    parser.add_argument("--features", default="macos-metal")
    parser.add_argument("--mode", default="reliable", choices=["fast", "balanced", "reliable"])
    parser.add_argument("--replay-mode", default="offline", choices=["offline", "rolling-buffer"])
    parser.add_argument("--language", default="en")
    parser.add_argument("--prompt", default="Sports officiating intercom.")
    replay.add_rolling_replay_args(parser)
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument("--sample-interval", type=float, default=0.25)
    return parser


def main(argv: list[str] | None = None) -> int:
    return run_suite(build_parser().parse_args(argv))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SuiteError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(2)
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
