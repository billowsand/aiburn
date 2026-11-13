#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
    printf '%s\n' 'Run this script on macOS to build a native macOS executable.' >&2
    exit 1
fi

if ! command -v cargo >/dev/null; then
    printf '%s\n' 'Install Rust and Cargo (Rust 1.95 or newer).' >&2
    exit 1
fi
if ! xcrun --find clang >/dev/null 2>&1; then
    printf '%s\n' 'Install Xcode Command Line Tools with xcode-select --install.' >&2
    exit 1
fi

if [[ -z ${IS903_LIBUSB_PREFIX:-} ]] && command -v brew >/dev/null; then
    IS903_LIBUSB_PREFIX=$(brew --prefix libusb)
fi
if [[ -z ${IS903_LIBUSB_PREFIX:-} ||
      ! -f "$IS903_LIBUSB_PREFIX/include/libusb-1.0/libusb.h" ||
      ! -f "$IS903_LIBUSB_PREFIX/lib/libusb-1.0.dylib" ]]; then
    printf '%s\n' 'Install libusb 1.0.30 or newer with brew install libusb, or set IS903_LIBUSB_PREFIX.' >&2
    exit 1
fi
IS903_LIBUSB_PREFIX=$(cd -- "$IS903_LIBUSB_PREFIX" && pwd -P)
export IS903_LIBUSB_PREFIX

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
exec cargo build --release --locked "$@"
