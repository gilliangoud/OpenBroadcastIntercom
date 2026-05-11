#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through Moonshine ASR candidates."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import transcription_benchmark as tb


ROOT = Path(__file__).resolve().parents[1]
ONNX_MODELS: list[tuple[str, str]] = [
    ("moonshine-tiny-onnx", "moonshine/tiny"),
    ("moonshine-base-onnx", "moonshine/base"),
]
TRANSFORMERS_MODELS: list[tuple[str, str]] = [
    ("moonshine-tiny-transformers", "UsefulSensors/moonshine-tiny"),
    ("moonshine-base-transformers", "UsefulSensors/moonshine-base"),
]


class SuiteError(Exception):
    pass


def parse_model_spec(raw: str) -> tuple[str, str]:
    if "=" not in raw:
        raise argparse.ArgumentTypeError("model specs must use model-id=model-name")
    model_id, model_name = raw.split("=", 1)
    model_id = model_id.strip()
    model_name = model_name.strip()
    if not model_id or not model_name:
        raise argparse.ArgumentTypeError("model id and model name cannot be empty")
    return model_id, model_name


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
        metrics["child_cpu_time_ms"] = metrics.get("user_time_ms", 0.0) + metrics.get(
            "system_time_ms", 0.0
        )
    return metrics


def run_profiled(command: list[str], *, env: dict[str, str], timeout: float) -> dict[str, Any]:
    started = time.perf_counter()
    time_l = Path("/usr/bin/time")
    wrapped = [str(time_l), "-l", *command] if time_l.exists() else command
    completed = subprocess.run(
        wrapped,
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    stderr = completed.stderr or ""
    stdout = completed.stdout or ""
    time_sysctl_blocked = "time: sysctl kern.clockrate: Operation not permitted" in stderr
    if completed.returncode != 0 and not time_sysctl_blocked:
        raise SuiteError(stderr.strip() or stdout.strip() or f"exit code {completed.returncode}")
    metrics = parse_macos_time_l(stderr) if time_l.exists() else {}
    return {
        "elapsed_ms": elapsed_ms,
        "returncode": completed.returncode,
        "stdout": stdout,
        "stderr": stderr,
        "max_rss_kb": metrics.get("max_rss_kb"),
        "child_cpu_time_ms": metrics.get("child_cpu_time_ms"),
        "profile_method": "time -l" if time_l.exists() and not time_sysctl_blocked else "elapsed",
    }


def absolute_without_symlink_resolution(path: Path) -> Path:
    expanded = path.expanduser()
    if not expanded.is_absolute():
        expanded = ROOT / expanded
    return Path(os.path.abspath(expanded))


def markdown_number(value: Any, suffix: str = "") -> str:
    if value is None:
        return "-"
    if isinstance(value, int):
        return f"{value}{suffix}"
    if isinstance(value, float):
        return f"{value:.1f}{suffix}"
    return str(value)


def enrich_score(
    result: dict[str, Any],
    *,
    model_id: str,
    model_name: str,
    predictions: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    result["model"] = {"id": model_id, "runtime": "moonshine", "name": model_name}
    result["adapter"] = {
        "runtime": predictions.get("runtime"),
        "backend": predictions.get("backend"),
        "model": predictions.get("model"),
        "model_load_ms": predictions.get("model_load_ms"),
        "mode": predictions.get("mode"),
        "chunk_ms": predictions.get("chunk_ms"),
        "overlap_ms": predictions.get("overlap_ms"),
        "rolling_buffer": predictions.get("rolling_buffer"),
    }
    result["macos_profile"] = {
        key: profile.get(key)
        for key in ("elapsed_ms", "max_rss_kb", "child_cpu_time_ms", "profile_method")
    }
    return result


def render_summary(results: list[dict[str, Any]]) -> str:
    lines = [
        "# Moonshine Benchmark Summary",
        "",
        "| Model | Backend | Mode | WER | CER | Avg latency | Realtime | First token | Finalize | Partials | Stale/drop | Load | Wall time | Max RSS | CPU time | Notes |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    for result in results:
        model = result.get("model", {})
        if result.get("error"):
            lines.append(
                "| "
                + " | ".join(
                    [
                        tb.markdown_escape(model.get("id")),
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
        profile = result.get("macos_profile", {})
        adapter = result.get("adapter", {})
        live_summary = summary.get("live") or {}
        backend = adapter.get("backend") or {}
        backend_label = "onnx" if backend.get("onnx") else "transformers"
        rss_kb = profile.get("max_rss_kb")
        rss_mb = rss_kb / 1024.0 if isinstance(rss_kb, int) else None
        stale_drop = "-"
        if live_summary:
            stale_drop = f"{live_summary.get('stale_jobs', 0)}/{live_summary.get('dropped_jobs', 0)}"
        lines.append(
            "| "
            + " | ".join(
                [
                    tb.markdown_escape(model.get("id")),
                    backend_label,
                    tb.markdown_escape(adapter.get("mode")),
                    tb.percent(summary["wer"]),
                    tb.percent(summary["cer"]),
                    markdown_number(summary["average_latency_ms"], " ms"),
                    markdown_number(summary["average_realtime_factor"], "x"),
                    markdown_number(live_summary.get("average_first_token_latency_ms"), " ms"),
                    markdown_number(live_summary.get("average_finalization_latency_ms"), " ms"),
                    markdown_number(live_summary.get("partial_updates")),
                    stale_drop,
                    markdown_number(adapter.get("model_load_ms"), " ms"),
                    markdown_number(profile.get("elapsed_ms"), " ms"),
                    markdown_number(rss_mb, " MB"),
                    markdown_number(profile.get("child_cpu_time_ms"), " ms"),
                    "",
                ]
            )
            + " |"
        )
    lines.append("")
    return "\n".join(lines)


def run_model(
    *,
    args: argparse.Namespace,
    corpus: tb.Corpus,
    model_id: str,
    model_name: str,
    mode: str,
) -> dict[str, Any]:
    model_dir = args.out_dir / f"{model_id}-{mode}"
    predictions_path = model_dir / "predictions.json"
    result_path = model_dir / "result.json"
    report_path = model_dir / "result.md"
    if args.skip_existing and predictions_path.exists() and result_path.exists():
        return json.loads(result_path.read_text(encoding="utf-8"))

    command = [
        str(args.python),
        "tools/moonshine_benchmark.py",
        "--corpus",
        str(args.corpus),
        "--model",
        model_name,
        "--model-id",
        f"{model_id}-{mode}",
        "--out",
        str(predictions_path),
        "--backend",
        args.backend,
        "--mode",
        mode,
        "--chunk-ms",
        str(args.chunk_ms),
        "--overlap-ms",
        str(args.overlap_ms),
        "--window-ms",
        str(args.window_ms),
        "--step-ms",
        str(args.step_ms),
        "--commit-lag-ms",
        str(args.commit_lag_ms),
        "--min-stable-passes",
        str(args.min_stable_passes),
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
        "--device",
        args.device,
        "--token-limit-factor",
        str(args.token_limit_factor),
    ]
    if args.fp16:
        command.append("--fp16")
    command.append(
        "--final-pass-on-release" if args.final_pass_on_release else "--no-final-pass-on-release"
    )

    profile = run_profiled(command, env=args.env, timeout=args.timeout)
    if not predictions_path.exists():
        stderr = (profile.get("stderr") or "").strip()
        stdout = (profile.get("stdout") or "").strip()
        detail = stderr or stdout or "adapter did not write predictions"
        raise SuiteError(detail[-4000:])

    raw_predictions = json.loads(predictions_path.read_text(encoding="utf-8"))
    _, predictions = tb.load_predictions(predictions_path)
    scored = tb.score_corpus(corpus, predictions, model_id=f"{model_id}-{mode}", runtime="moonshine")
    scored = enrich_score(
        scored,
        model_id=f"{model_id}-{mode}",
        model_name=model_name,
        predictions=raw_predictions,
        profile=profile,
    )
    tb.write_outputs(scored, out_json=result_path, out_md=report_path)
    return scored


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument(
        "--python",
        type=Path,
        default=ROOT / "artifacts/transcription-benchmarks/.venv-moonshine/bin/python",
    )
    parser.add_argument(
        "--hf-home",
        type=Path,
        default=ROOT / "artifacts/transcription-benchmarks/hf-cache",
    )
    parser.add_argument("--backend", choices=["onnx", "transformers"], default="onnx")
    parser.add_argument("--model", action="append", type=parse_model_spec, default=[])
    parser.add_argument("--no-default-models", action="store_true")
    parser.add_argument(
        "--mode",
        action="append",
        choices=["offline", "chunked", "rolling-buffer"],
        help="May be passed more than once. Defaults to offline and chunked.",
    )
    parser.add_argument("--skip-existing", action="store_true")
    parser.add_argument("--chunk-ms", type=int, default=2000)
    parser.add_argument("--overlap-ms", type=int, default=0)
    parser.add_argument("--window-ms", type=int, default=8000)
    parser.add_argument("--step-ms", type=int, default=1000)
    parser.add_argument("--commit-lag-ms", type=int, default=1500)
    parser.add_argument("--min-stable-passes", type=int, default=2)
    parser.add_argument(
        "--final-pass-on-release",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    parser.add_argument("--final-pass-scope", choices=["utterance", "window"], default="utterance")
    parser.add_argument("--vad-rms-threshold", type=float, default=0.01)
    parser.add_argument("--vad-hangover-ms", type=int, default=600)
    parser.add_argument("--vad-min-speech-ms", type=int, default=120)
    parser.add_argument("--stale-job-ms", type=int, default=30_000)
    parser.add_argument("--queue-limit", type=int, default=8)
    parser.add_argument("--device", choices=["auto", "cpu", "mps", "cuda"], default="auto")
    parser.add_argument("--fp16", action="store_true")
    parser.add_argument("--token-limit-factor", type=float, default=6.5)
    parser.add_argument("--timeout", type=float, default=1800.0)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    args.corpus = args.corpus.resolve()
    args.out_dir = args.out_dir.resolve()
    args.python = absolute_without_symlink_resolution(args.python)
    args.hf_home = args.hf_home.resolve()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    args.hf_home.mkdir(parents=True, exist_ok=True)
    args.env = os.environ.copy()
    args.env["HF_HOME"] = str(args.hf_home)
    if not args.python.exists():
        raise SystemExit(f"{args.python} does not exist")

    defaults = ONNX_MODELS if args.backend == "onnx" else TRANSFORMERS_MODELS
    model_specs = ([] if args.no_default_models else list(defaults)) + list(args.model)
    modes = args.mode or ["offline", "chunked"]
    if not model_specs:
        raise SystemExit("no models selected")

    corpus = tb.load_corpus(args.corpus)
    results: list[dict[str, Any]] = []
    for model_id, model_name in model_specs:
        for mode in modes:
            print(f"running {model_id} ({model_name}) in {mode} mode", flush=True)
            try:
                results.append(
                    run_model(
                        args=args,
                        corpus=corpus,
                        model_id=model_id,
                        model_name=model_name,
                        mode=mode,
                    )
                )
            except Exception as exc:  # noqa: BLE001 - keep the suite moving.
                result_id = f"{model_id}-{mode}"
                error_result = {
                    "model": {"id": result_id, "runtime": "moonshine", "name": model_name},
                    "error": str(exc),
                }
                (args.out_dir / result_id).mkdir(parents=True, exist_ok=True)
                (args.out_dir / result_id / "error.json").write_text(
                    json.dumps(error_result, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                results.append(error_result)
                print(f"failed {result_id}: {exc}", flush=True)

    summary = {
        "created_at": datetime.now(timezone.utc).isoformat(),
        "corpus": str(args.corpus),
        "runtime": "moonshine",
        "backend": args.backend,
        "models": [{"id": model_id, "name": model_name} for model_id, model_name in model_specs],
        "modes": modes,
        "chunk_ms": args.chunk_ms,
        "overlap_ms": args.overlap_ms,
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
        "results": results,
    }
    (args.out_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    (args.out_dir / "summary.md").write_text(render_summary(results), encoding="utf-8")
    print(f"wrote {args.out_dir / 'summary.md'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("interrupted", flush=True)
        raise SystemExit(130)
