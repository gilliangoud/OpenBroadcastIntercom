#!/usr/bin/env python3
"""Validate the external model manifest without downloading model files."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "intercom-models" / "manifest.json"
EXPECTED_HOST = "huggingface.co"
EXPECTED_REPO_PATH = "/ggerganov/whisper.cpp/resolve/main/"


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    models = data.get("models")
    if not isinstance(models, list) or not models:
        fail("intercom-models/manifest.json must contain a non-empty models list")

    default_model = data.get("default_model")
    filenames: set[str] = set()
    ids: set[str] = set()
    for model in models:
        model_id = model.get("id")
        filename = model.get("filename")
        url = model.get("url")
        sha1 = model.get("sha1")

        if not isinstance(model_id, str) or not model_id:
            fail("each model must have an id")
        if model_id in ids:
            fail(f"duplicate model id: {model_id}")
        ids.add(model_id)

        if not isinstance(filename, str) or not filename.startswith("ggml-") or not filename.endswith(".bin"):
            fail(f"invalid model filename: {filename!r}")
        if "/" in filename or "\\" in filename:
            fail(f"model filename must not contain path separators: {filename}")
        if filename in filenames:
            fail(f"duplicate model filename: {filename}")
        filenames.add(filename)

        if not isinstance(url, str):
            fail(f"{filename}: url is required")
        parsed = urlparse(url)
        if parsed.scheme != "https" or parsed.netloc != EXPECTED_HOST:
            fail(f"{filename}: url must use https://{EXPECTED_HOST}")
        if not parsed.path.startswith(EXPECTED_REPO_PATH):
            fail(f"{filename}: url must resolve from ggerganov/whisper.cpp main")
        if not parsed.path.endswith(filename):
            fail(f"{filename}: url path must end with the filename")

        if not isinstance(sha1, str) or not re.fullmatch(r"[0-9a-f]{40}", sha1):
            fail(f"{filename}: sha1 must be 40 lowercase hex characters")

    if default_model not in filenames:
        fail("default_model must match one of the manifest filenames")


if __name__ == "__main__":
    main()
