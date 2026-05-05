#!/usr/bin/env sh
set -eu

python3 "$(dirname "$0")/release_version.py" --check
