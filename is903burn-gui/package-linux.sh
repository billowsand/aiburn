#!/usr/bin/env bash
# 打出 Linux 发布包：.deb 与可移植 tar.gz。产物落在 dist/。
#
# 用法：./package-linux.sh [amd64|arm64]
# 在 Ubuntu 20.04 上构建时，产物以 glibc 2.31 为基线，可在 Ubuntu 20.04 及更新系统运行。
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

ARCH=${1:-$(dpkg --print-architecture)}
case $ARCH in
    amd64) TAR_ARCH=x86_64 ;;
    arm64) TAR_ARCH=aarch64-ubuntu20.04 ;;
    *)
        printf '不支持的架构: %s（只支持 amd64 或 arm64）\n' "$ARCH" >&2
        exit 1
        ;;
esac

VERSION=${VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)}
if [[ -z $VERSION ]]; then
    printf '%s\n' '无法从 Cargo.toml 读取版本号，请设置 VERSION。' >&2
    exit 1
fi

DIST=$PWD/dist
mkdir -p "$DIST"

cargo build --release --locked

STAGE=$DIST/linux-$ARCH
rm -rf -- "$STAGE"
install -Dm755 target/release/is903burn-gui "$STAGE/usr/bin/is903burn-gui"
install -Dm644 is903burn-gui.desktop "$STAGE/usr/share/applications/is903burn-gui.desktop"
install -Dm644 assets/is903burn-gui.png \
    "$STAGE/usr/share/icons/hicolor/512x512/apps/is903burn-gui.png"

cat > "$STAGE/使用说明.txt" <<EOF
IS903 烧录工具 ${VERSION}（Linux ${TAR_ARCH}）

直接运行：
  ./is903burn-gui              启动图形界面
  ./is903burn-gui --devices    列出 IS903 设备
  ./is903burn-gui --help       查看命令行用法

安装到系统（可选）：
  sudo install -Dm755 is903burn-gui /usr/bin/is903burn-gui
  sudo install -Dm644 is903burn-gui.desktop /usr/share/applications/is903burn-gui.desktop
  sudo install -Dm644 is903burn-gui.png /usr/share/icons/hicolor/512x512/apps/is903burn-gui.png

运行时需要 udisks2（挂载/卸载）与 polkit（提权访问设备）。
烧录使用 SG_IO，需要 sg 内核模块和目标 /dev/sgN 的读写权限：
  sudo modprobe sg
  sudo setfacl -m u:\$USER:rw /dev/sg0
EOF

mkdir -p "$STAGE/DEBIAN"
installed_size=$(du -sk "$STAGE" | cut -f1)
cat > "$STAGE/DEBIAN/control" <<EOF
Package: is903burn-gui
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${installed_size}
Maintainer: sysphus <3699403+billowsand@users.noreply.github.com>
Homepage: https://github.com/billowsand/aiburn
Depends: libc6 (>= 2.31), libgcc-s1, libgl1, libegl1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libfontconfig1, udisks2, policykit-1 | polkitd, xdg-utils
Recommends: xdg-desktop-portal, xdg-desktop-portal-gtk | xdg-desktop-portal-backend
Description: IS903 (UFCD) ISO burning GUI
 Burn ISO images or folders to an Innostor IS903 USB Flash CD-ROM
 (UFCD) device. Includes a GUI and a command-line interface.
EOF

dpkg-deb --root-owner-group --build "$STAGE" \
    "$DIST/is903burn-gui_${VERSION}_${ARCH}.deb" >/dev/null

# 可移植包：解压后直接运行 is903burn-gui，不需要安装。
TAR_STAGE=$DIST/tar-$ARCH
rm -rf -- "$TAR_STAGE"
mkdir -p "$TAR_STAGE"
cp target/release/is903burn-gui is903burn-gui.desktop "$TAR_STAGE/"
cp assets/is903burn-gui.png "$TAR_STAGE/"
cp "$STAGE/使用说明.txt" "$TAR_STAGE/"
tar -C "$TAR_STAGE" -czf "$DIST/is903burn-gui-${VERSION}-linux-${TAR_ARCH}.tar.gz" .

rm -rf -- "$STAGE" "$TAR_STAGE"
ls -l "$DIST"/*.deb "$DIST"/*.tar.gz
