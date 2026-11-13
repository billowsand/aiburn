#!/bin/bash
# macOS 一键烧录 IS903(USB Flash CD-ROM / UFCD)
#
# 走「大容量存储」模式的块写入路径:ISO 原样写到块设备偏移 0,
# 与 Linux 上 is903-burn.sh 的 burn 路径一致(见 PROTOCOL.md 第 8 节)。
#
# 前置条件:
#   * 设备已经处于「大容量存储」模式(macOS 上 /dev/diskN 可写)。
#     如果设备是只读的 CD-ROM 模式,本脚本会明确报错——那种模式需要
#     厂商 SCSI 命令,请使用 is903burn-gui 的 macOS --burn。
#   * 编译好的 GUI 工具:is903burn-gui --mkiso 可以生成 ISO(纯 Rust,无需 xorriso)
#
# 用法:
#   ./is903-burn-macos.sh <文件或目录> [-l 卷标] [-d /dev/diskN] [-y]

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
GUI="${GUI:-$SCRIPT_DIR/is903burn-gui/target/release/is903burn-gui}"
SRC=""
LABEL="DATA"
DEV=""
ASSUME_YES=0

die() { echo "错误: $*" >&2; exit 1; }
info() { echo "==> $*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    -l) LABEL="$2"; shift 2 ;;
    -d) DEV="$2"; shift 2 ;;
    -y) ASSUME_YES=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) SRC="$1"; shift ;;
  esac
done

[ -n "$SRC" ] || die "缺少来源文件或目录"
[ -e "$SRC" ] || die "来源不存在: $SRC"
[ -x "$GUI" ] || die "找不到 $GUI,请先 cargo build --release"

# ---- 1. 找设备 -------------------------------------------------------------
if [ -z "$DEV" ]; then
  # GUI 的 --devices 会列出识别到的 IS903(VID:PID 1f75:0903)
  DEV=$("$GUI" --devices 2>/dev/null | awk 'NR==1{print $1}')
  [ -n "$DEV" ] || die "没有找到 IS903 设备(VID:PID 1f75:0903)"
fi
[ -b "$DEV" ] || die "$DEV 不是块设备"
RAW="$DEV"
case "$(basename "$DEV")" in
  disk*) RAW="/dev/r$(basename "$DEV")" ;;
esac
[ -c "$RAW" ] || RAW="$DEV"
info "目标设备: $DEV (原始设备 $RAW)"

# ---- 2. 只读检查 -----------------------------------------------------------
if ! diskutil info "$DEV" | grep -q "Media Read-Only: *No"; then
  die "$DEV 是只读的(CD-ROM 模式)。请使用 sudo is903burn-gui --burn 进行原生光驱烧录。"
fi

# ---- 3. 卸载 ---------------------------------------------------------------
info "卸载设备"
diskutil unmountDisk "$DEV" >/dev/null || die "无法卸载 $DEV"

# ---- 4. 生成 ISO -----------------------------------------------------------
ISO="$(mktemp -t is903).iso"
trap 'rm -f "$ISO"' EXIT
info "生成 ISO(卷标 $LABEL)"
"$GUI" --mkiso "$SRC" -l "$LABEL" -o "$ISO" >/dev/null
SIZE=$(stat -f%z "$ISO")
SECTORS=$(( (SIZE + 2047) / 2048 ))
info "ISO 大小 $SIZE 字节 ($SECTORS 个 2048 字节扇区)"

CAP=$(diskutil info "$DEV" | sed -n 's/.*Disk Size:.*(\([0-9]*\) Bytes).*/\1/p')
if [ -n "$CAP" ] && [ "$SIZE" -gt "$CAP" ]; then
  die "ISO 超过设备容量($SIZE > $CAP)"
fi

if [ "$ASSUME_YES" -eq 0 ]; then
  printf "将覆盖 %s 上的全部内容,确认继续? [y/N] " "$DEV"
  read -r answer
  case "$answer" in
    y|Y|yes|YES) ;;
    *) die "已取消" ;;
  esac
fi

# ---- 5. 写入 ---------------------------------------------------------------
info "写入(这会擦除设备上的旧内容)"
dd if="$ISO" of="$RAW" bs=1m 2>&1 | tail -2
sync

# ---- 6. 回读校验 -----------------------------------------------------------
info "回读校验"
SRC_HASH=$(shasum -a 256 "$ISO" | awk '{print $1}')
DST_HASH=$(dd if="$RAW" bs=2048 count="$SECTORS" 2>/dev/null | shasum -a 256 | awk '{print $1}')
if [ "$SRC_HASH" != "$DST_HASH" ]; then
  die "回读不一致: $SRC_HASH != $DST_HASH"
fi
info "回读一致: $DST_HASH"

# ---- 7. 挂载 ---------------------------------------------------------------
info "挂载"
diskutil mount "$DEV" || echo "提示: 设备可能已切换到只读 CD-ROM 模式,拔插后再用 diskutil list 查看"
