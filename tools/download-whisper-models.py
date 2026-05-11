#!/usr/bin/env python3
"""Download curated model assets declared in intercom-models/manifest.json.

The filename is kept for compatibility with existing docs/scripts. By default it
downloads the default transcription model; pass --category deepfilternet_onnx or
--all to fetch other curated assets.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "intercom-models" / "manifest.json"
DEFAULT_TIMEOUT_SECONDS = 60.0
EXPECTED_HOST = "huggingface.co"


def load_manifest() -> dict:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def model_destination(root: Path, model: dict, override_dir: Path | None) -> Path:
    model_dir = override_dir if override_dir is not None else root / model["destination_dir"]
    return model_dir / model["filename"]


def validate_downloadable(model: dict) -> None:
    if not model.get("available", True):
        raise SystemExit(f"{model['id']} is listed but not available for download yet")
    url = model.get("url")
    sha256 = model.get("sha256")
    if not url or not sha256:
        raise SystemExit(f"{model['id']} is missing a curated URL or sha256")
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.netloc != EXPECTED_HOST:
        raise SystemExit(f"{model['id']} URL is not on the curated host allowlist")
    if not parsed.path.endswith(model["filename"]):
        raise SystemExit(f"{model['id']} URL path does not end with {model['filename']}")


def download_model(destination: Path, model: dict, force: bool, timeout: float) -> None:
    validate_downloadable(model)
    expected_sha256 = model["sha256"]

    if destination.exists() and not force:
        actual_sha256 = sha256_file(destination)
        if actual_sha256 == expected_sha256:
            print(f"ok: {destination} already exists and matches sha256")
            return
        raise SystemExit(
            f"{destination} already exists but sha256 is {actual_sha256}, expected {expected_sha256}. "
            "Delete it or rerun with --force."
        )

    destination.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {model['filename']} ({model.get('size', 'unknown size')})")
    request = urllib.request.Request(
        model["url"],
        headers={"User-Agent": "RedLine-model-downloader/1.0"},
    )

    with tempfile.NamedTemporaryFile(
        prefix=f".{model['filename']}.",
        dir=destination.parent,
        delete=False,
    ) as temp:
        temp_path = Path(temp.name)
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                shutil.copyfileobj(response, temp, length=1024 * 1024)
        except Exception:
            temp_path.unlink(missing_ok=True)
            raise

    actual_sha256 = sha256_file(temp_path)
    if actual_sha256 != expected_sha256:
        temp_path.unlink(missing_ok=True)
        raise SystemExit(
            f"{model['filename']} sha256 mismatch: got {actual_sha256}, expected {expected_sha256}"
        )

    temp_path.replace(destination)
    print(f"saved: {destination}")


def main() -> None:
    manifest = load_manifest()
    models = manifest["models"]
    by_id_or_filename = {
        key: model
        for model in models
        for key in (model["id"], model["filename"])
    }

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("models", nargs="*", help="model ids or filenames to download")
    parser.add_argument("--all", action="store_true", help="download every available model in the manifest")
    parser.add_argument("--category", choices=manifest.get("categories", []), help="download available models in one category")
    parser.add_argument("--list", action="store_true", help="list catalog models and exit")
    parser.add_argument("--model-dir", help="override destination directory for selected models")
    parser.add_argument("--force", action="store_true", help="replace an existing local file")
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"network timeout in seconds per request (default: {DEFAULT_TIMEOUT_SECONDS:g})",
    )
    args = parser.parse_args()

    if args.list:
        for model in models:
            flags = []
            if model.get("default"):
                flags.append("default")
            if model.get("recommended"):
                flags.append("recommended")
            if not model.get("available", True):
                flags.append("planned")
            suffix = f" ({', '.join(flags)})" if flags else ""
            print(f"{model['id']}: {model['filename']} [{model['category']}/{model['runtime']}] {model.get('size', '')}{suffix}")
        return

    if args.all:
        selected = [model for model in models if model.get("available", True)]
    elif args.category:
        selected = [
            model for model in models
            if model["category"] == args.category and model.get("available", True)
        ]
    elif args.models:
        missing = [name for name in args.models if name not in by_id_or_filename]
        if missing:
            raise SystemExit(f"unknown model(s): {', '.join(missing)}")
        selected = [by_id_or_filename[name] for name in args.models]
    else:
        selected = [
            model for model in models
            if model["category"] == "transcription" and model.get("default")
        ]

    if not selected:
        raise SystemExit("no downloadable models matched")

    override_dir = Path(args.model_dir).expanduser() if args.model_dir else None
    for model in selected:
        download_model(model_destination(ROOT, model, override_dir), model, args.force, args.timeout)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
