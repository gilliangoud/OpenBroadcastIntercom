#!/usr/bin/env python3
"""Validate the curated model manifest without downloading model files."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path, PurePosixPath
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "intercom-models" / "manifest.json"
EXPECTED_HOST = "huggingface.co"
KNOWN_CATEGORIES = {"transcription", "deepfilternet_onnx", "deepfilternet_coreml"}


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def safe_leaf(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{label} is required")
    if value.startswith(".") or "/" in value or "\\" in value or PurePosixPath(value).name != value:
        fail(f"{label} must be a safe filename: {value!r}")
    return value


def safe_destination(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{label} is required")
    if "\\" in value:
        fail(f"{label} must be a safe relative directory: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        fail(f"{label} must be a safe relative directory: {value!r}")
    return value


def validate_url(model_id: str, url: object, filename: str) -> None:
    if not isinstance(url, str) or not url:
        fail(f"{model_id}: url is required for available models")
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.netloc != EXPECTED_HOST:
        fail(f"{model_id}: url must use https://{EXPECTED_HOST}")
    if not parsed.path.endswith(filename):
        fail(f"{model_id}: url path must end with the model filename")


def main() -> None:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if data.get("version") != 2:
        fail("intercom-models/manifest.json must use version 2")
    source_policy = data.get("source_policy") or {}
    if source_policy.get("allowed_host") != EXPECTED_HOST or source_policy.get("custom_urls") is not False:
        fail("source_policy must allow only curated huggingface.co URLs")
    categories = set(data.get("categories") or [])
    if categories != KNOWN_CATEGORIES:
        fail(f"categories must be exactly {sorted(KNOWN_CATEGORIES)}")

    models = data.get("models")
    if not isinstance(models, list) or not models:
        fail("intercom-models/manifest.json must contain a non-empty models list")

    ids: set[str] = set()
    destinations: set[tuple[str, str]] = set()
    for model in models:
        model_id = model.get("id")
        if not isinstance(model_id, str) or not model_id:
            fail("each model must have an id")
        if model_id in ids:
            fail(f"duplicate model id: {model_id}")
        ids.add(model_id)

        category = model.get("category")
        if category not in categories:
            fail(f"{model_id}: unknown category {category!r}")
        filename = safe_leaf(model.get("filename"), f"{model_id}: filename")
        destination_dir = safe_destination(model.get("destination_dir"), f"{model_id}: destination_dir")
        destination = (destination_dir, filename)
        if destination in destinations:
            fail(f"duplicate destination: {destination_dir}/{filename}")
        destinations.add(destination)

        available = model.get("available", True)
        url = model.get("url")
        sha256 = model.get("sha256")
        if available:
            validate_url(model_id, url, filename)
            if not isinstance(sha256, str) or not re.fullmatch(r"[0-9a-f]{64}", sha256):
                fail(f"{model_id}: sha256 must be 64 lowercase hex characters")
        else:
            if url is not None:
                validate_url(model_id, url, filename)
            if sha256 is not None and not re.fullmatch(r"[0-9a-f]{64}", str(sha256)):
                fail(f"{model_id}: optional sha256 must be 64 lowercase hex characters")


if __name__ == "__main__":
    main()
