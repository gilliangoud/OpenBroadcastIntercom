#!/usr/bin/env python3
"""Run a RedLine benchmark corpus through a set of MLX Whisper models.

This runner is intentionally separate from the whisper.cpp suite because MLX
needs an Apple Silicon macOS session with Metal access. Run it outside the
restricted sandbox, or through an unrestricted local execution path.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import transcription_benchmark as tb


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MODELS: list[tuple[str, str]] = [
    ("mlx-whisper-large-v3-turbo", "mlx-community/whisper-large-v3-turbo"),
    ("mlx-whisper-large-v3-turbo-q4", "mlx-community/whisper-large-v3-turbo-q4"),
    ("mlx-distil-whisper-large-v3", "mlx-community/distil-whisper-large-v3"),
    ("mlx-distil-whisper-medium-en", "mlx-community/distil-whisper-medium.en"),
    ("mlx-whisper-medium-8bit", "mlx-community/whisper-medium-mlx-8bit"),
    ("mlx-whisper-large-v3", "mlx-community/whisper-large-v3-mlx"),
    ("mlx-whisper-large-v3-8bit", "mlx-community/whisper-large-v3-mlx-8bit"),
    ("mlx-whisper-small", "mlx-community/whisper-small-mlx"),
    ("mlx-whisper-small-q4", "mlx-community/whisper-small-mlx-q4"),
    ("mlx-whisper-base", "mlx-community/whisper-base-mlx"),
    ("mlx-whisper-tiny", "mlx-community/whisper-tiny-mlx"),
]


class SuiteError(Exception):
    pass


def parse_model_spec(raw: str) -> tuple[str, str]:
    if "=" not in raw:
        raise argparse.ArgumentTypeError("model specs must use model-id=hf-repo")
    model_id, repo = raw.split("=", 1)
    model_id = model_id.strip()
    repo = repo.strip()
    if not model_id or not repo:
        raise argparse.ArgumentTypeError("model id and repo cannot be empty")
    return model_id, repo


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
        "stdout": stdout,
        "stderr": stderr,
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
    model_id: str,
    repo: str,
    predictions: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    result["model"] = {"id": model_id, "runtime": "mlx-whisper", "repo": repo}
    result["adapter"] = {
        "runtime": predictions.get("runtime"),
        "backend": predictions.get("backend"),
        "model": predictions.get("model"),
        "model_load_ms": predictions.get("model_load_ms"),
        "language": predictions.get("language"),
        "condition_on_previous_text": predictions.get("condition_on_previous_text"),
    }
    result["macos_profile"] = {
        key: profile.get(key)
        for key in ("elapsed_ms", "max_rss_kb", "child_cpu_time_ms", "profile_method")
    }
    return result


def render_summary(results: list[dict[str, Any]]) -> str:
    lines = [
        "# MLX Whisper Benchmark Summary",
        "",
        "| Model | Repo | WER | CER | Avg latency | Realtime | First segment/load | Wall time | Max RSS | CPU time | Notes |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    for result in results:
        model = result.get("model", {})
        if result.get("error"):
            lines.append(
                "| "
                + " | ".join(
                    [
                        tb.markdown_escape(model.get("id")),
                        tb.markdown_escape(model.get("repo")),
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
                    tb.markdown_escape(model.get("repo")),
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
    model_id: str,
    repo: str,
) -> dict[str, Any]:
    model_dir = args.out_dir / model_id
    predictions_path = model_dir / "predictions.json"
    result_path = model_dir / "result.json"
    report_path = model_dir / "result.md"
    if args.skip_existing and predictions_path.exists() and result_path.exists():
        return json.loads(result_path.read_text(encoding="utf-8"))

    command = [
        str(args.python),
        "tools/mlx_whisper_benchmark.py",
        "--corpus",
        str(args.corpus),
        "--model",
        repo,
        "--model-id",
        model_id,
        "--out",
        str(predictions_path),
        "--language",
        args.language,
        "--prompt",
        args.prompt,
    ]
    if not args.condition_on_previous_text:
        command.append("--no-condition-on-previous-text")
    profile = run_profiled(command, env=args.env, timeout=args.timeout)
    raw_predictions = json.loads(predictions_path.read_text(encoding="utf-8"))
    _, predictions = tb.load_predictions(predictions_path)
    scored = tb.score_corpus(corpus, predictions, model_id=model_id, runtime="mlx-whisper")
    scored = enrich_score(
        scored,
        model_id=model_id,
        repo=repo,
        predictions=raw_predictions,
        profile=profile,
    )
    tb.write_outputs(scored, out_json=result_path, out_md=report_path)
    return scored


def absolute_without_symlink_resolution(path: Path) -> Path:
    expanded = path.expanduser()
    if not expanded.is_absolute():
        expanded = ROOT / expanded
    return Path(os.path.abspath(expanded))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument(
        "--python",
        type=Path,
        default=ROOT / "artifacts/transcription-benchmarks/.venv-mlx/bin/python",
        help="Python executable with mlx-whisper installed",
    )
    parser.add_argument(
        "--hf-home",
        type=Path,
        default=ROOT / "artifacts/transcription-benchmarks/hf-cache",
    )
    parser.add_argument("--model", action="append", type=parse_model_spec, default=[])
    parser.add_argument("--no-default-models", action="store_true")
    parser.add_argument("--skip-existing", action="store_true")
    parser.add_argument("--language", default="en")
    parser.add_argument("--prompt", default="Clean read English speech.")
    parser.add_argument(
        "--condition-on-previous-text",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    parser.add_argument("--timeout", type=float, default=1800.0)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    args.corpus = args.corpus.resolve()
    args.out_dir = args.out_dir.resolve()
    # Do not Path.resolve() venv Python executables: resolving the symlink points
    # at the base interpreter and bypasses the venv's site-packages.
    args.python = absolute_without_symlink_resolution(args.python)
    args.hf_home = args.hf_home.resolve()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    args.hf_home.mkdir(parents=True, exist_ok=True)
    args.env = os.environ.copy()
    args.env["HF_HOME"] = str(args.hf_home)

    if not args.python.exists():
        raise SystemExit(f"{args.python} does not exist")

    model_specs = ([] if args.no_default_models else list(DEFAULT_MODELS)) + list(args.model)
    if not model_specs:
        raise SystemExit("no models selected")

    corpus = tb.load_corpus(args.corpus)
    results: list[dict[str, Any]] = []
    for model_id, repo in model_specs:
        print(f"running {model_id} ({repo})", flush=True)
        try:
            results.append(run_model(args=args, corpus=corpus, model_id=model_id, repo=repo))
        except Exception as exc:  # noqa: BLE001 - keep the suite moving.
            error_result = {
                "model": {"id": model_id, "runtime": "mlx-whisper", "repo": repo},
                "error": str(exc),
            }
            (args.out_dir / model_id).mkdir(parents=True, exist_ok=True)
            (args.out_dir / model_id / "error.json").write_text(
                json.dumps(error_result, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            results.append(error_result)
            print(f"failed {model_id}: {exc}", file=sys.stderr, flush=True)

    summary = {
        "created_at": datetime.now(timezone.utc).isoformat(),
        "corpus": str(args.corpus),
        "runtime": "mlx-whisper",
        "models": [{"id": model_id, "repo": repo} for model_id, repo in model_specs],
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
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
