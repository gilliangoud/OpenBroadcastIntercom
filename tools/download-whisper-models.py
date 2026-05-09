#!/usr/bin/env python3
"""Download external Whisper/whisper.cpp models declared in the manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "intercom-models" / "manifest.json"
DEFAULT_TIMEOUT_SECONDS = 60.0


def load_manifest() -> dict:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


def sha1_file(path: Path) -> str:
    digest = hashlib.sha1()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def model_destination(model_dir: Path, model: dict) -> Path:
    return model_dir / model["filename"]


def download_model(model_dir: Path, model: dict, force: bool, timeout: float) -> None:
    destination = model_destination(model_dir, model)
    expected_sha1 = model["sha1"]

    if destination.exists() and not force:
        actual_sha1 = sha1_file(destination)
        if actual_sha1 == expected_sha1:
            print(f"ok: {destination} already exists and matches sha1")
            return
        raise SystemExit(
            f"{destination} already exists but sha1 is {actual_sha1}, expected {expected_sha1}. "
            "Delete it or rerun with --force."
        )

    model_dir.mkdir(parents=True, exist_ok=True)
    print(f"downloading {model['filename']} ({model.get('size', 'unknown size')})")
    request = urllib.request.Request(
        model["url"],
        headers={"User-Agent": "OpenBroadcastIntercom-model-downloader/1.0"},
    )

    with tempfile.NamedTemporaryFile(prefix=f".{model['filename']}.", dir=model_dir, delete=False) as temp:
        temp_path = Path(temp.name)
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                shutil.copyfileobj(response, temp, length=1024 * 1024)
        except Exception:
            temp_path.unlink(missing_ok=True)
            raise

    actual_sha1 = sha1_file(temp_path)
    if actual_sha1 != expected_sha1:
        temp_path.unlink(missing_ok=True)
        raise SystemExit(
            f"{model['filename']} sha1 mismatch: got {actual_sha1}, expected {expected_sha1}"
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
    parser.add_argument("--all", action="store_true", help="download every model in the manifest")
    parser.add_argument("--list", action="store_true", help="list available models and exit")
    parser.add_argument(
        "--model-dir",
        default=str(ROOT / manifest.get("model_dir", "intercom-models")),
        help="destination directory for downloaded models",
    )
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
            default = " (default)" if model["filename"] == manifest.get("default_model") else ""
            print(f"{model['id']}: {model['filename']} {model.get('size', '')}{default}")
        return

    if args.all:
        selected = models
    elif args.models:
        missing = [name for name in args.models if name not in by_id_or_filename]
        if missing:
            raise SystemExit(f"unknown model(s): {', '.join(missing)}")
        selected = [by_id_or_filename[name] for name in args.models]
    else:
        selected = [by_id_or_filename[manifest["default_model"]]]

    model_dir = Path(args.model_dir).expanduser()
    for model in selected:
        download_model(model_dir, model, args.force, args.timeout)


if __name__ == "__main__":
    main()
