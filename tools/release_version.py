#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_TAG_RE = re.compile(r"^v(?P<year>\d{4})\.(?P<month>\d{1,2})\.(?P<counter>\d+)$")
TAURI_CONFIGS = (
    ROOT / "clients/app/tauri.conf.json",
    ROOT / "clients/bridge-app/tauri.conf.json",
    ROOT / "server-app/tauri.conf.json",
)


def parse_version_tag(tag: str) -> tuple[int, int, int] | None:
    match = VERSION_TAG_RE.match(tag.strip())
    if not match:
        return None
    year = int(match.group("year"))
    month = int(match.group("month"))
    counter = int(match.group("counter"))
    if not 1 <= month <= 12 or counter < 1:
        return None
    return year, month, counter


def next_calver(tags: list[str], today: dt.date) -> str:
    counter = 0
    for tag in tags:
        parsed = parse_version_tag(tag)
        if parsed is None:
            continue
        year, month, tag_counter = parsed
        if year == today.year and month == today.month:
            counter = max(counter, tag_counter)
    return f"{today.year}.{today.month}.{counter + 1}"


def version_code(version: str) -> int:
    year, month, counter = parse_version_tag(f"v{version}") or (0, 0, 0)
    if year == 0:
        raise ValueError(f"invalid release version: {version}")
    return (year % 100) * 1_000_000 + month * 10_000 + counter


def git_tags() -> list[str]:
    result = subprocess.run(
        ["git", "tag", "--list", "v[0-9]*.[0-9]*.[0-9]*"],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def cargo_workspace_version() -> str:
    cargo = (ROOT / "Cargo.toml").read_text()
    match = re.search(r"(?ms)^\[workspace\.package\]\s*(?:^.*\n)*?^version\s*=\s*\"([^\"]+)\"", cargo)
    if not match:
        raise RuntimeError("could not find workspace.package.version in Cargo.toml")
    return match.group(1)


def update_cargo_version(version: str) -> None:
    path = ROOT / "Cargo.toml"
    text = path.read_text()
    updated = re.sub(
        r"(?ms)(^\[workspace\.package\]\s*(?:^.*\n)*?^version\s*=\s*)\"[^\"]+\"",
        rf'\1"{version}"',
        text,
        count=1,
    )
    if updated == text:
        raise RuntimeError("could not update workspace.package.version in Cargo.toml")
    path.write_text(updated)


def update_tauri_config(path: Path, version: str) -> None:
    data = json.loads(path.read_text())
    data["version"] = version
    if path == ROOT / "clients/app/tauri.conf.json":
        data.setdefault("bundle", {}).setdefault("android", {})["versionCode"] = version_code(version)
    path.write_text(json.dumps(data, indent=2) + "\n")


def assert_versions_synced() -> None:
    version = cargo_workspace_version()
    mismatches: list[str] = []
    for config in TAURI_CONFIGS:
        data = json.loads(config.read_text())
        if data.get("version") != version:
            mismatches.append(f"{config.relative_to(ROOT)} version={data.get('version')} expected={version}")
    app_config = json.loads((ROOT / "clients/app/tauri.conf.json").read_text())
    actual_code = app_config.get("bundle", {}).get("android", {}).get("versionCode")
    try:
        expected_code = version_code(version)
    except ValueError:
        expected_code = actual_code
    else:
        if actual_code != expected_code:
            mismatches.append(
                f"clients/app/tauri.conf.json android.versionCode={actual_code} expected={expected_code}"
            )
    if mismatches:
        raise RuntimeError("version sync failed:\n" + "\n".join(mismatches))


def apply_version(version: str) -> None:
    update_cargo_version(version)
    for config in TAURI_CONFIGS:
        update_tauri_config(config, version)
    assert_versions_synced()


def parse_date(value: str | None) -> dt.date:
    if value:
        return dt.date.fromisoformat(value)
    return dt.date.today()


def main() -> None:
    parser = argparse.ArgumentParser(description="Compute, apply, and verify RedLine release versions.")
    parser.add_argument("--date", help="ISO date override for deterministic CalVer computation.")
    parser.add_argument("--version", help="Explicit version to apply instead of computing the next CalVer.")
    parser.add_argument("--print-next", action="store_true", help="Print the next release version.")
    parser.add_argument("--apply", action="store_true", help="Update Cargo and Tauri version files.")
    parser.add_argument("--check", action="store_true", help="Verify Cargo and Tauri version files are synced.")
    args = parser.parse_args()

    if args.check:
        assert_versions_synced()
        return

    version = args.version or next_calver(git_tags(), parse_date(args.date))
    if args.apply:
        apply_version(version)
    if args.print_next or args.apply:
        print(version)


if __name__ == "__main__":
    main()
