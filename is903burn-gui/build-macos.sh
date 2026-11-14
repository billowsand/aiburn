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
artifact_log=$(mktemp)
trap 'rm -f -- "$artifact_log"' EXIT
cargo build --release --locked "$@" --message-format=json-render-diagnostics > "$artifact_log"

# 使用 Cargo 报告的路径，兼容 --target、--target-dir 和自定义 profile。
/usr/bin/python3 - "$artifact_log" <<'PY'
import json
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as log:
    artifacts = (json.loads(line) for line in log)
    artifact = next((entry for entry in artifacts
                     if entry["reason"] == "compiler-artifact"
                     and entry["target"]["name"] == "is903burn-gui"
                     and "bin" in entry["target"]["kind"]
                     and entry["executable"]), None)
if artifact is None:
    sys.exit("Cargo did not produce the is903burn-gui executable.")

executable = Path(artifact["executable"])
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--format-version", "1", "--no-deps", "--locked",
]))
version = next(package["version"] for package in metadata["packages"]
               if package["id"] == artifact["package_id"])
bundle = executable.with_name("IS903 Burner.app")
contents = bundle / "Contents"
for directory in (contents / "MacOS", contents / "Resources"):
    directory.mkdir(parents=True, exist_ok=True)
shutil.copy2(executable, contents / "MacOS" / executable.name)
shutil.copyfile("assets/is903burn-gui.icns", contents / "Resources" / "is903burn-gui.icns")
with (contents / "Info.plist").open("wb") as plist:
    plistlib.dump({
        "CFBundleIdentifier": "io.github.billowsand.is903burn-gui",
        "CFBundleName": "IS903 Burner",
        "CFBundleExecutable": executable.name,
        "CFBundleIconFile": "is903burn-gui.icns",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
    }, plist)
print(f"Built {bundle}")
PY
