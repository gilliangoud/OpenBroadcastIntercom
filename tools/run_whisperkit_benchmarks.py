#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through WhisperKit/Core ML models."""

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
DEFAULT_MODELS: list[dict[str, str | None]] = [
    {"id": "whisperkit-distil-large-v3", "model": "large-v3", "prefix": "distil", "path": None},
    {"id": "whisperkit-large-v3", "model": "large-v3", "prefix": None, "path": None},
    {"id": "whisperkit-small", "model": "small", "prefix": None, "path": None},
    {"id": "whisperkit-base", "model": "base", "prefix": None, "path": None},
    {"id": "whisperkit-tiny", "model": "tiny", "prefix": None, "path": None},
]


class SuiteError(Exception):
    pass


def parse_model_spec(raw: str) -> dict[str, str | None]:
    """Parse `id=model`, `id=prefix:model`, or `id=path:/local/model`."""
    if "=" not in raw:
        raise argparse.ArgumentTypeError("model specs must use id=model")
    model_id, value = raw.split("=", 1)
    model_id = model_id.strip()
    value = value.strip()
    if not model_id or not value:
        raise argparse.ArgumentTypeError("model id and value cannot be empty")
    if value.startswith("path:"):
        model_path = value.removeprefix("path:").strip()
        if not model_path:
            raise argparse.ArgumentTypeError("model path cannot be empty")
        return {"id": model_id, "model": None, "prefix": None, "path": model_path}
    if ":" in value:
        prefix, model = value.split(":", 1)
        prefix = prefix.strip()
        model = model.strip()
        if not prefix or not model:
            raise argparse.ArgumentTypeError("prefixed model specs must use prefix:model")
        return {"id": model_id, "model": model, "prefix": prefix, "path": None}
    return {"id": model_id, "model": value, "prefix": None, "path": None}


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
        "max_rss_kb": metrics.get("max_rss_kb"),
        "child_cpu_time_ms": metrics.get("child_cpu_time_ms"),
        "profile_method": "time -l" if time_l.exists() and not time_sysctl_blocked else "elapsed",
    }


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
    model: dict[str, str | None],
    predictions: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    result["model"] = {"id": model["id"], "runtime": "whisperkit-coreml", **model}
    result["adapter"] = {
        "runtime": predictions.get("runtime"),
        "backend": predictions.get("backend"),
        "model": predictions.get("model"),
        "model_prefix": predictions.get("model_prefix"),
        "model_path": predictions.get("model_path"),
        "model_load_ms": predictions.get("model_load_ms"),
        "language": predictions.get("language"),
        "compute_units": predictions.get("compute_units"),
    }
    result["macos_profile"] = {
        key: profile.get(key)
        for key in ("elapsed_ms", "max_rss_kb", "child_cpu_time_ms", "profile_method")
    }
    return result


def render_summary(results: list[dict[str, Any]]) -> str:
    lines = [
        "# WhisperKit/Core ML Benchmark Summary",
        "",
        "| Model | Variant | WER | CER | Avg latency | Realtime | First segment/load | Wall time | Max RSS | CPU time | Notes |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    for result in results:
        model = result.get("model", {})
        variant = model.get("path") or ":".join(
            part for part in (model.get("prefix"), model.get("model")) if part
        )
        if result.get("error"):
            lines.append(
                "| "
                + " | ".join(
                    [
                        tb.markdown_escape(model.get("id")),
                        tb.markdown_escape(variant),
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
        rss_kb = profile.get("max_rss_kb")
        rss_mb = rss_kb / 1024.0 if isinstance(rss_kb, int) else None
        lines.append(
            "| "
            + " | ".join(
                [
                    tb.markdown_escape(model.get("id")),
                    tb.markdown_escape(variant),
                    tb.percent(summary["wer"]),
                    tb.percent(summary["cer"]),
                    markdown_number(summary["average_latency_ms"], " ms"),
                    markdown_number(summary["average_realtime_factor"], "x"),
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
    model: dict[str, str | None],
) -> dict[str, Any]:
    model_id = str(model["id"])
    model_dir = args.out_dir / model_id
    predictions_path = model_dir / "predictions.json"
    result_path = model_dir / "result.json"
    report_path = model_dir / "result.md"
    if args.skip_existing and predictions_path.exists() and result_path.exists():
        return json.loads(result_path.read_text(encoding="utf-8"))

    command = [
        "python3",
        "tools/whisperkit_benchmark.py",
        "--corpus",
        str(args.corpus),
        "--model-id",
        model_id,
        "--out",
        str(predictions_path),
        "--cli",
        args.cli,
        "--language",
        args.language,
    ]
    if model.get("model"):
        command.extend(["--model", str(model["model"])])
    if model.get("prefix"):
        command.extend(["--model-prefix", str(model["prefix"])])
    if model.get("path"):
        command.extend(["--model-path", str(model["path"])])
    if args.prompt:
        command.extend(["--prompt", args.prompt])
    if args.audio_encoder_compute_units:
        command.extend(["--audio-encoder-compute-units", args.audio_encoder_compute_units])
    if args.text_decoder_compute_units:
        command.extend(["--text-decoder-compute-units", args.text_decoder_compute_units])
    if args.command_template:
        command.extend(["--command-template", args.command_template])
    if args.text_regex:
        command.extend(["--text-regex", args.text_regex])
    if args.verbose:
        command.append("--verbose")
    command.extend(["--segment-timeout", str(args.segment_timeout)])

    profile = run_profiled(command, env=args.env, timeout=args.timeout)
    raw_predictions = json.loads(predictions_path.read_text(encoding="utf-8"))
    _, predictions = tb.load_predictions(predictions_path)
    scored = tb.score_corpus(corpus, predictions, model_id=model_id, runtime="whisperkit-coreml")
    scored = enrich_score(scored, model=model, predictions=raw_predictions, profile=profile)
    tb.write_outputs(scored, out_json=result_path, out_md=report_path)
    return scored


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument("--cli", default="whisperkit-cli")
    parser.add_argument("--model", action="append", type=parse_model_spec, default=[])
    parser.add_argument("--no-default-models", action="store_true")
    parser.add_argument("--skip-existing", action="store_true")
    parser.add_argument("--language", default="en")
    parser.add_argument("--prompt", default="Clean read English speech.")
    parser.add_argument(
        "--audio-encoder-compute-units",
        choices=["all", "cpuAndGPU", "cpuOnly", "cpuAndNeuralEngine", "random"],
    )
    parser.add_argument(
        "--text-decoder-compute-units",
        choices=["all", "cpuAndGPU", "cpuOnly", "cpuAndNeuralEngine", "random"],
    )
    parser.add_argument("--command-template")
    parser.add_argument("--text-regex")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--segment-timeout", type=float, default=900.0)
    parser.add_argument("--timeout", type=float, default=2400.0)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    args.corpus = args.corpus.resolve()
    args.out_dir = args.out_dir.resolve()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    args.env = os.environ.copy()
    model_specs = ([] if args.no_default_models else list(DEFAULT_MODELS)) + list(args.model)
    if not model_specs:
        raise SystemExit("no models selected")

    corpus = tb.load_corpus(args.corpus)
    results: list[dict[str, Any]] = []
    for model in model_specs:
        print(f"running {model['id']}", flush=True)
        try:
            results.append(run_model(args=args, corpus=corpus, model=model))
        except Exception as exc:  # noqa: BLE001 - keep the suite moving.
            error_result = {
                "model": {"runtime": "whisperkit-coreml", **model},
                "error": str(exc),
            }
            (args.out_dir / str(model["id"])).mkdir(parents=True, exist_ok=True)
            (args.out_dir / str(model["id"]) / "error.json").write_text(
                json.dumps(error_result, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            results.append(error_result)
            print(f"failed {model['id']}: {exc}", flush=True)

    summary = {
        "created_at": datetime.now(timezone.utc).isoformat(),
        "corpus": str(args.corpus),
        "runtime": "whisperkit-coreml",
        "models": model_specs,
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
