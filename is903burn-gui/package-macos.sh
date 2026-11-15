#!/usr/bin/env bash
# 打出 macOS Universal（arm64 + x86_64）发布包：
#   dist/is903burn-gui-<version>-macos-universal.dmg
#
# 与 build-macos.sh 的区别：这里编译两种架构、合并成 Universal 可执行文件，
# 并把 libusb 动态库一起放进 App 包，用户不需要安装 Homebrew 依赖。
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
    printf '%s\n' 'Run this script on macOS to package the macOS build.' >&2
    exit 1
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

VERSION=${VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)}
LIBUSB_VERSION=${LIBUSB_VERSION:-1.0.30}
DIST=$PWD/dist
WORK=$DIST/macos
LIBUSB_PREFIX=$WORK/libusb
APP="$WORK/IS903 Burner.app"
DMG_STAGE=$WORK/dmg

mkdir -p "$DIST" "$WORK"

export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-11.0}
rustup target add aarch64-apple-darwin x86_64-apple-darwin

# libusb 的 Homebrew 包只包含当前机器架构，这里从源码编译出 Universal 版本。
if [[ ! -f $LIBUSB_PREFIX/lib/libusb-1.0.0.dylib ]]; then
    if [[ ! -d $WORK/libusb-$LIBUSB_VERSION ]]; then
        curl -fL --retry 3 \
            -o "$WORK/libusb-$LIBUSB_VERSION.tar.bz2" \
            "https://github.com/libusb/libusb/releases/download/v$LIBUSB_VERSION/libusb-$LIBUSB_VERSION.tar.bz2"
        tar -C "$WORK" -xjf "$WORK/libusb-$LIBUSB_VERSION.tar.bz2"
    fi
    for arch in arm64 x86_64; do
        build=$WORK/libusb-build-$arch
        prefix=$WORK/libusb-$arch
        rm -rf -- "$build" "$prefix"
        mkdir -p "$build"
        (
            cd "$build"
            host_arg=
            if [[ $arch == x86_64 ]]; then
                host_arg=--host=x86_64-apple-darwin
            fi
            # macOS SDK 里声明了 pipe2，但它直到 macOS 27 才可用；按部署目标 11.0
            # 编译时必须退回 pipe()，否则生成的 dylib 在旧系统上会加载失败。
            CFLAGS="-arch $arch -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET -O2" \
            LDFLAGS="-arch $arch -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET" \
            ac_cv_func_pipe2=no \
                "$WORK/libusb-$LIBUSB_VERSION/configure" \
                ${host_arg:+"$host_arg"} \
                --prefix="$prefix" --disable-static >/dev/null
            make -j"$(sysctl -n hw.ncpu)" >/dev/null
            make install >/dev/null
        )
    done
    rm -rf -- "$LIBUSB_PREFIX"
    mkdir -p "$LIBUSB_PREFIX/lib" "$LIBUSB_PREFIX/include"
    cp -R "$WORK/libusb-arm64/include/"* "$LIBUSB_PREFIX/include/"
    lipo -create \
        "$WORK/libusb-arm64/lib/libusb-1.0.0.dylib" \
        "$WORK/libusb-x86_64/lib/libusb-1.0.0.dylib" \
        -output "$LIBUSB_PREFIX/lib/libusb-1.0.0.dylib"
    # lipo 会保留两个架构各自的 LC_ID_DYLIB（分别指向构建目录），必须统一，
    # 否则链接出的 App 在 arm64 上仍会去找构建目录里的 libusb。
    install_name_tool -id '@executable_path/../Frameworks/libusb-1.0.0.dylib' \
        "$LIBUSB_PREFIX/lib/libusb-1.0.0.dylib"
    ln -sf libusb-1.0.0.dylib "$LIBUSB_PREFIX/lib/libusb-1.0.dylib"
fi
file "$LIBUSB_PREFIX/lib/libusb-1.0.0.dylib"
otool -D "$LIBUSB_PREFIX/lib/libusb-1.0.0.dylib"

IS903_LIBUSB_PREFIX=$LIBUSB_PREFIX ./build-macos.sh --target aarch64-apple-darwin
IS903_LIBUSB_PREFIX=$LIBUSB_PREFIX ./build-macos.sh --target x86_64-apple-darwin

ARM_EXE=target/aarch64-apple-darwin/release/is903burn-gui
X86_EXE=target/x86_64-apple-darwin/release/is903burn-gui
if [[ ! -x $ARM_EXE || ! -x $X86_EXE ]]; then
    printf '缺少构建产物: %s 或 %s\n' "$ARM_EXE" "$X86_EXE" >&2
    exit 1
fi

rm -rf -- "$APP"
cp -R "target/aarch64-apple-darwin/release/IS903 Burner.app" "$APP"

mkdir -p "$APP/Contents/Frameworks"
cp "$LIBUSB_PREFIX/lib/libusb-1.0.0.dylib" "$APP/Contents/Frameworks/"
lipo -create "$ARM_EXE" "$X86_EXE" -output "$WORK/is903burn-gui-universal"
cp "$WORK/is903burn-gui-universal" "$APP/Contents/MacOS/is903burn-gui"

# 把可执行文件里指向构建目录的 libusb 路径改到 App 包内。
# install_name_tool 不支持按架构选择，但 -change 会作用于包含该路径的所有切片，
# 因此把两个架构里的旧路径各替换一次即可。
LIBUSB_ID='@executable_path/../Frameworks/libusb-1.0.0.dylib'
for arch in arm64 x86_64; do
    otool -arch "$arch" -L "$APP/Contents/MacOS/is903burn-gui" |
        awk '/libusb/{print $1}'
done | sort -u | while IFS= read -r link; do
    [[ -z $link || $link == "$LIBUSB_ID" ]] && continue
    install_name_tool -change "$link" "$LIBUSB_ID" \
        "$APP/Contents/MacOS/is903burn-gui"
done
install_name_tool -id "$LIBUSB_ID" \
    "$APP/Contents/Frameworks/libusb-1.0.0.dylib"

for arch in arm64 x86_64; do
    link=$(otool -arch "$arch" -L "$APP/Contents/MacOS/is903burn-gui" |
        awk '/libusb/{print $1; exit}')
    if [[ $link != "$LIBUSB_ID" ]]; then
        printf '架构 %s 的 libusb 路径不正确: %s\n' "$arch" "$link" >&2
        exit 1
    fi
done

/usr/libexec/PlistBuddy \
    -c "Set :CFBundleShortVersionString $VERSION" \
    -c "Set :CFBundleVersion $VERSION" \
    "$APP/Contents/Info.plist"

codesign --force --sign - --timestamp=none \
    "$APP/Contents/Frameworks/libusb-1.0.0.dylib"
codesign --force --sign - --timestamp=none --deep "$APP"
codesign --verify --deep --strict "$APP"
lipo -info "$APP/Contents/MacOS/is903burn-gui"
otool -L "$APP/Contents/MacOS/is903burn-gui"

rm -rf -- "$DMG_STAGE"
mkdir -p "$DMG_STAGE"
cp -R "$APP" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
cat > "$DMG_STAGE/使用说明.txt" <<EOF
IS903 烧录工具 ${VERSION}（macOS Universal）

安装：
  把 IS903 Burner.app 拖到 Applications（应用程序）文件夹。

首次打开：
  本版本没有 Apple 开发者签名。首次请在 Finder 里右键点击 App，
  选择「打开」，再在提示框里确认；也可以在「系统设置 → 隐私与安全性」
  里选择「仍要打开」。

命令行：
  "/Applications/IS903 Burner.app/Contents/MacOS/is903burn-gui" --devices
  sudo "/Applications/IS903 Burner.app/Contents/MacOS/is903burn-gui" --burn <设备> -l LABEL <来源>

光驱烧录需要管理员权限；挂载与卸载使用系统自带的 diskutil。
EOF

DMG=$DIST/is903burn-gui-$VERSION-macos-universal.dmg
hdiutil create -volname 'IS903 Burner' -srcfolder "$DMG_STAGE" \
    -ov -format UDZO "$DMG" >/dev/null
shasum -a 256 "$DMG"
