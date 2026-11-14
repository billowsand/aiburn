#!/usr/bin/env bash
# 打包成 pacman 可直接安装的 .pkg.tar.zst，产物落在 dist/。
#
# makepkg 默认把工作目录放在 ./src 和 ./pkg，前者会和 crate 的源码目录冲突，
# 因此这里把 BUILDDIR 重定向到 dist/build。
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

export BUILDDIR="${BUILDDIR:-$PWD/dist/build}"
export PKGDEST="${PKGDEST:-$PWD/dist}"
mkdir -p "$BUILDDIR" "$PKGDEST"

# cargo/rust 由 rustup 提供、不经 pacman 管理，跳过 makedepends 检查。
exec makepkg --cleanbuild --force --nodeps --noconfirm "$@"
