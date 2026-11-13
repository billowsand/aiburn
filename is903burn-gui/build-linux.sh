#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
    printf '%s\n' 'Run this script on Linux to build a native Linux executable.' >&2
    exit 1
fi

if ! command -v cargo >/dev/null; then
    printf '%s\n' 'Install Rust and Cargo (Rust 1.95 or newer).' >&2
    exit 1
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
exec cargo build --release --locked "$@"
