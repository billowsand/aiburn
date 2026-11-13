#!/usr/bin/env bash
#
# is903-burn.sh — 把目录或文件一键烧录到 Innostar IS903 (UFCD)
#
# 流程:制作 ISO → 卸载 → 烧录 → 刷新缓存 → 校验 → 挂载 → 文件核对
#
# 用法:
#   ./is903-burn.sh /path/to/folder
#   ./is903-burn.sh /path/to/file.ttf
#   ./is903-burn.sh /path/to/folder --label MYDISC
#
set -uo pipefail

# ---------------------------------------------------------------- 配置

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BURNER="$SELF_DIR/is903burn"

SRC=""
LABEL=""
ISO_PATH=""
DEV_OVERRIDE=""
KEEP_ISO=0
DO_MOUNT=1
DO_VERIFY=1
ASSUME_YES=0
DRY_RUN=0

TMP_ISO=""
MOUNT_POINT=""
STEP=0
FAILED=0

# ---------------------------------------------------------------- 输出

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
	C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'
	C_RED=$'\033[31m'; C_GREEN=$'\033[32m'
	C_YELLOW=$'\033[33m'; C_BLUE=$'\033[36m'
else
	C_RESET=""; C_BOLD=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""
fi

step() {
	STEP=$((STEP + 1))
	printf '\n%s==> [%d] %s%s\n' "$C_BOLD$C_BLUE" "$STEP" "$*" "$C_RESET"
}

info() { printf '    %s\n' "$*"; }
ok()   { printf '    %s✔ %s%s\n' "$C_GREEN" "$*" "$C_RESET"; }
warn() { printf '    %s! %s%s\n' "$C_YELLOW" "$*" "$C_RESET" >&2; }
die()  {
	printf '\n%s✘ 失败: %s%s\n' "$C_RED" "$*" "$C_RESET" >&2
	exit 1
}

# ---------------------------------------------------------------- 工具

have() { command -v "$1" >/dev/null 2>&1; }

# 把字节数格式化成可读形式
human() {
	awk -v b="${1:-0}" 'BEGIN{
		split("B KiB MiB GiB TiB",u," "); i=1
		while (b>=1024 && i<5) { b/=1024; i++ }
		if (i==1) printf "%d %s", b, u[i]
		else printf "%.2f %s", b, u[i]
	}'
}

# 获取块设备容量(字节),依次尝试 blockdev / sysfs / is903burn
dev_capacity() {
	local blk=$1 n
	n=$(blockdev --getsize64 "$blk" 2>/dev/null)
	if [ -n "$n" ] && [ "$n" -gt 0 ] 2>/dev/null; then
		printf '%s\n' "$n"; return 0
	fi
	n=$(cat "/sys/class/block/$(basename "$blk")/size" 2>/dev/null)
	if [ -n "$n" ] && [ "$n" -gt 0 ] 2>/dev/null; then
		printf '%s\n' "$((n * 512))"; return 0
	fi
	n=$("$BURNER" capacity --dev "$blk" 2>/dev/null | sed -n 's/.*capacity [0-9]* blocks, \([0-9]*\) bytes.*/\1/p')
	if [ -n "$n" ] && [ "$n" -gt 0 ] 2>/dev/null; then
		printf '%s\n' "$n"; return 0
	fi
	return 1
}

# 以 root 执行,优先无密码 sudo,其次 pkexec
run_root() {
	if [ "$(id -u)" -eq 0 ]; then
		"$@"
	elif have sudo && sudo -n true 2>/dev/null; then
		sudo "$@"
	elif have pkexec; then
		pkexec "$@"
	else
		return 1
	fi
}

cleanup() {
	# 仅在异常退出时清理临时 ISO
	if [ -n "$TMP_ISO" ] && [ -f "$TMP_ISO" ] && [ "$KEEP_ISO" -eq 0 ]; then
		rm -f "$TMP_ISO"
	fi
}
trap cleanup EXIT

usage() {
	cat <<EOF
用法: $(basename "$0") <目录或文件> [选项]

把指定的目录或文件打包成 ISO 并烧录到 IS903 设备。

选项:
  -l, --label <名称>   卷标(默认由路径推导,大写字母/数字/下划线)
  -o, --iso <路径>     生成的 ISO 保存位置(默认临时文件)
      --keep-iso       保留生成的 ISO
      --dev <块设备>   指定设备,如 /dev/sda(默认自动检测)
      --no-mount       烧录后不挂载
      --no-verify      跳过一致性校验(不推荐)
  -y, --yes            跳过确认提示
  -n, --dry-run        只显示将执行的操作
  -h, --help           显示本帮助

示例:
  $(basename "$0") ~/Downloads/Aahj.ttf
  $(basename "$0") ~/Documents --label MYDATA
  $(basename "$0") ./资料 -l DOCS -y --keep-iso -o /tmp/docs.iso
EOF
}

# ---------------------------------------------------------------- 参数

while [ $# -gt 0 ]; do
	case "$1" in
	-l|--label)   LABEL=${2:-}; shift 2 ;;
	-o|--iso)     ISO_PATH=${2:-}; shift 2 ;;
	--dev)        DEV_OVERRIDE=${2:-}; shift 2 ;;
	--keep-iso)   KEEP_ISO=1; shift ;;
	--no-mount)   DO_MOUNT=0; shift ;;
	--no-verify)  DO_VERIFY=0; shift ;;
	-y|--yes)     ASSUME_YES=1; shift ;;
	-n|--dry-run) DRY_RUN=1; shift ;;
	-h|--help)    usage; exit 0 ;;
	-*)           die "未知选项: $1(用 -h 查看帮助)" ;;
	*)            [ -z "$SRC" ] && SRC=$1 || die "只能指定一个路径"; shift ;;
	esac
done

[ -n "$SRC" ] || { usage; exit 1; }

# ---------------------------------------------------------------- 1. 检查

step "环境检查"

[ -e "$SRC" ] || die "路径不存在: $SRC"
SRC=$(cd "$(dirname "$SRC")" && pwd)/$(basename "$SRC")
[ -e "$SRC" ] || die "路径不存在: $SRC"

[ -x "$BURNER" ] || die "找不到 $BURNER(先在 $SELF_DIR 运行 make)"
have xorrisofs || die "缺少 xorrisofs,请安装: sudo pacman -S xorriso"

if [ -d "$SRC" ]; then
	SRC_TYPE=dir
	# 目录不能为空,否则生成不出有意义的 ISO
	if [ -z "$(find "$SRC" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]; then
		die "目录为空: $SRC"
	fi
else
	SRC_TYPE=file
fi
ok "来源: $SRC ($SRC_TYPE)"

# ---------------------------------------------------------------- 2. 找设备

step "检测 IS903 设备"

detect_device() {
	local d name vendor type parent
	for d in /sys/block/sd* /sys/block/sr*; do
		[ -d "$d" ] || continue
		vendor=$(cat "$d/device/vendor" 2>/dev/null) || continue
		case "$vendor" in
		*Innosto*|*Innostar*) ;;
		*)
			# OEM firmware can replace the SCSI vendor string.
			parent=$(readlink -f "$d/device") || continue
			while [[ "$parent" == /sys/devices/* && ! -r "$parent/idVendor" ]]; do
				parent=${parent%/*}
			done
			[ -r "$parent/idVendor" ] && [ -r "$parent/idProduct" ] || continue
			[ "$(<"$parent/idVendor"):$(<"$parent/idProduct")" = "1f75:0903" ] || continue
			;;
		esac
		name=$(basename "$d")
		type=$(cat "$d/device/type" 2>/dev/null || echo "?")
		printf '%s %s\n' "$name" "$type"
	done
}

if [ -n "$DEV_OVERRIDE" ]; then
	BLK=$(basename "$DEV_OVERRIDE")
	BLK="/dev/$BLK"
	[ -b "$BLK" ] || die "不是块设备: $BLK"
	DEV_TYPE=$(cat "/sys/class/block/$(basename "$BLK")/device/type" 2>/dev/null || echo "?")
else
	mapfile -t FOUND < <(detect_device)
	[ ${#FOUND[@]} -gt 0 ] || die "未找到 IS903 设备。请插好设备,并确认: lsusb | grep 1f75"
	if [ ${#FOUND[@]} -gt 1 ]; then
		warn "检测到多个候选设备:"
		printf '        %s\n' "${FOUND[@]}"
		die "请用 --dev 明确指定"
	fi
	read -r NAME DEV_TYPE <<<"${FOUND[0]}"
	BLK="/dev/$NAME"
fi

info "块设备: $BLK"
info "SCSI 类型: $DEV_TYPE  (0=大容量存储, 5=CD-ROM)"

if [ "$DEV_TYPE" != "0" ]; then
	cat >&2 <<EOF

  当前设备的 SCSI 类型是 $DEV_TYPE。本脚本需要以类型 0 枚举的可写块设备。
  类型 5 是只读 CD-ROM，重新插拔后仍可能保持类型 5。

EOF
	die "设备模式不支持烧录"
fi
ok "设备可写: $BLK"

# 找到配套的 /dev/sgN(可选,原生协议命令用)
SG_DEV=$(ls /dev/sg* 2>/dev/null | head -1)
[ -n "$SG_DEV" ] && info "通用 SCSI 节点: $SG_DEV"

# ---------------------------------------------------------------- 3. 容量预检

# 在花时间制作 ISO 之前,先用来源体积粗略判断是否能装下。
step "容量预检"

DEV_BYTES=$(dev_capacity "$BLK" 2>/dev/null || echo 0)
SRC_BYTES=$(du -sb "$SRC" 2>/dev/null | cut -f1)
[ -n "$SRC_BYTES" ] || SRC_BYTES=0

if [ "$DEV_BYTES" -gt 0 ] 2>/dev/null; then
	info "设备容量 : $(human "$DEV_BYTES") ($DEV_BYTES 字节)"
else
	info "设备容量 : 未知"
fi
info "来源体积 : $(human "$SRC_BYTES") ($SRC_BYTES 字节)"

if [ "$DEV_BYTES" -gt 0 ] 2>/dev/null && [ "$SRC_BYTES" -gt 0 ] 2>/dev/null; then
	if [ "$SRC_BYTES" -gt "$DEV_BYTES" ]; then
		cat >&2 <<EOF

  来源体积已超过设备容量,无需制作 ISO。

    来源 : $(human "$SRC_BYTES")
    设备 : $(human "$DEV_BYTES")
    超出 : $(human $((SRC_BYTES - DEV_BYTES)))

  请减小来源体积(删除文件或拆分目录)后重试。

EOF
		die "容量不足"
	fi
	# ISO 结构本身有少量开销,超过容量 95% 时提示
	if [ "$SRC_BYTES" -gt $((DEV_BYTES / 100 * 95)) ]; then
		warn "来源已占用设备 $(awk -v a="$SRC_BYTES" -v b="$DEV_BYTES" 'BEGIN{printf "%.1f%%", a*100/b}')，制作 ISO 后可能超出"
	else
		ok "体积预检通过(占设备 $(awk -v a="$SRC_BYTES" -v b="$DEV_BYTES" 'BEGIN{printf "%.1f%%", a*100/b}'))"
	fi
else
	info "跳过预检(容量未知)"
fi

# ---------------------------------------------------------------- 4. 卷标

step "准备卷标"

if [ -z "$LABEL" ]; then
	LABEL=$(basename "$SRC")
	# ISO9660 卷标:字母数字下划线,大写,最长 32
	LABEL=$(printf '%s' "$LABEL" | tr '[:lower:]' '[:upper:]' \
		| sed 's/[^A-Z0-9_]/_/g' | cut -c1-32)
	[ -n "$LABEL" ] || LABEL="DATA"
fi
info "卷标: $LABEL"
ok "卷标就绪"

# ---------------------------------------------------------------- 5. 制作 ISO

step "制作 ISO 镜像"

if [ -n "$ISO_PATH" ]; then
	OUT_ISO=$ISO_PATH
else
	TMP_ISO=$(mktemp /tmp/is903burn.XXXXXX.iso)
	OUT_ISO=$TMP_ISO
fi

info "命令: xorrisofs -o $OUT_ISO -V $LABEL -J -R $SRC"
if [ "$DRY_RUN" -eq 0 ]; then
	if ! xorrisofs -o "$OUT_ISO" -V "$LABEL" -J -R "$SRC" >/tmp/is903.ofs.log 2>&1; then
		cat /tmp/is903.ofs.log >&2
		die "xorrisofs 执行失败"
	fi
	ISO_SIZE=$(stat -c %s "$OUT_ISO")
	ok "ISO 已生成: $OUT_ISO ($ISO_SIZE 字节, $((ISO_SIZE / 2048)) 个 2048 字节扇区)"
else
	info "(dry-run) 跳过"
fi

# ---------------------------------------------------------------- 6. 容量检查(精确)

step "容量检查"

if [ "$DRY_RUN" -eq 0 ]; then
	DEV_BYTES=$(dev_capacity "$BLK") || die "无法获取 $BLK 的容量"
	info "设备容量 : $(human "$DEV_BYTES") ($DEV_BYTES 字节)"
	info "镜像大小 : $(human "$ISO_SIZE") ($ISO_SIZE 字节)"
	if [ "$ISO_SIZE" -gt "$DEV_BYTES" ]; then
		over=$((ISO_SIZE - DEV_BYTES))
		cat >&2 <<EOF

  镜像超出设备容量,无法烧录。

    镜像 : $(human "$ISO_SIZE")
    设备 : $(human "$DEV_BYTES")
    超出 : $(human "$over")

  请减小来源体积(删除文件或拆分目录)后重试。

EOF
		die "容量不足"
	fi
	if [ "$ISO_SIZE" -eq "$DEV_BYTES" ]; then
		ok "镜像恰好填满设备"
	else
		ok "容量充足(余量 $(human $((DEV_BYTES - ISO_SIZE))))"
	fi
else
	DEV_BYTES=$(dev_capacity "$BLK" 2>/dev/null || echo 0)
	if [ "$DEV_BYTES" -gt 0 ] 2>/dev/null; then
		info "设备容量 : $(human "$DEV_BYTES")"
		info "镜像大小 : 烧录时检查"
	else
		info "设备容量 : 未知"
	fi
	info "(dry-run) 实际烧录前会比对两者"
fi

# ---------------------------------------------------------------- 7. 卸载

step "卸载设备"

if [ "$DRY_RUN" -eq 0 ]; then
	MPS=$(findmnt -n -o TARGET --source "$BLK" 2>/dev/null || true)
	if [ -z "$MPS" ]; then
		ok "设备未挂载,无需卸载"
	else
		for mp in $MPS; do
			info "卸载 $mp"
			udisksctl unmount -b "$BLK" >/dev/null 2>&1 \
				|| run_root umount "$mp" >/dev/null 2>&1 \
				|| run_root umount -l "$mp" >/dev/null 2>&1 \
				|| true
		done
		sleep 1
		if findmnt -n --source "$BLK" >/dev/null 2>&1; then
			die "卸载失败,仍有挂载: $(findmnt -n -o TARGET --source "$BLK" | tr '\n' ' ')"
		fi
		ok "已卸载"
	fi
else
	info "(dry-run) 跳过"
fi

# ---------------------------------------------------------------- 8. 确认

step "烧录"

if [ "$ASSUME_YES" -eq 0 ] && [ "$DRY_RUN" -eq 0 ]; then
	printf '\n  %s即将覆盖 %s 上的全部数据%s\n' "$C_YELLOW" "$BLK" "$C_RESET"
	printf '  来源: %s\n' "$SRC"
	printf '  卷标: %s\n' "$LABEL"
	printf '  继续? [y/N] '
	read -r ans
	case "$ans" in
	y|Y|yes|YES) ;;
	*) die "已取消" ;;
	esac
fi

if [ "$DRY_RUN" -eq 0 ]; then
	if ! "$BURNER" burn "$OUT_ISO" --dev "$BLK" --force; then
		die "烧录失败"
	fi
	ok "烧录完成"
else
	info "(dry-run) 将执行: $BURNER burn $OUT_ISO --dev $BLK --force"
fi

# ---------------------------------------------------------------- 9. 刷缓存

step "刷新内核缓存"

# 关键:反复挂载同一块设备后,内核 ISO9660 元数据缓存可能残留旧内容。
# blockdev --flushbufs 会丢弃该设备的缓冲区,否则挂载可能显示旧文件信息。
if [ "$DRY_RUN" -eq 0 ]; then
	if run_root blockdev --flushbufs "$BLK" 2>/dev/null; then
		ok "缓冲区已刷新"
	else
		warn "blockdev --flushbufs 未成功,挂载后若显示旧内容请物理拔插设备"
	fi
else
	info "(dry-run) 跳过"
fi

# ---------------------------------------------------------------- 10. 校验

step "一致性校验"

if [ "$DRY_RUN" -eq 0 ] && [ "$DO_VERIFY" -eq 1 ]; then
	CHECK_ISO=$(mktemp /tmp/is903check.XXXXXX.iso)
	if ! "$BURNER" dump "$CHECK_ISO" --dev "$BLK" >/tmp/is903.dump.log 2>&1; then
		cat /tmp/is903.dump.log >&2
		rm -f "$CHECK_ISO"
		die "回读设备失败"
	fi
	if cmp -s "$OUT_ISO" "$CHECK_ISO"; then
		ok "设备内容与生成的 ISO 逐字节一致 ($ISO_SIZE 字节)"
		FAILED=0
	else
		rm -f "$CHECK_ISO"
		die "校验失败:设备回读内容与源 ISO 不一致"
	fi
	rm -f "$CHECK_ISO"
elif [ "$DRY_RUN" -eq 0 ]; then
	warn "已跳过校验(--no-verify)"
else
	info "(dry-run) 跳过"
fi

# ---------------------------------------------------------------- 11. 挂载

step "挂载设备"

if [ "$DRY_RUN" -eq 1 ] || [ "$DO_MOUNT" -eq 0 ]; then
	info "(跳过)"
else
	MOUNT_POINT=""
	if UOUT=$(udisksctl mount -b "$BLK" 2>&1); then
		MOUNT_POINT=$(printf '%s' "$UOUT" | sed -n 's/.* at \(.*\)$/\1/p')
	else
		warn "udisksctl 挂载失败,改用手动挂载"
	fi

	if [ -z "$MOUNT_POINT" ] || [ ! -d "$MOUNT_POINT" ]; then
		# 回退:手动挂到 /run/media/$USER/$LABEL
		target="/run/media/${USER:-user}/$LABEL"
		run_root mkdir -p "$target" 2>/dev/null || true
		if run_root mount -t iso9660 -o ro "$BLK" "$target" 2>/dev/null; then
			MOUNT_POINT=$target
		else
			# 最后回退:/mnt
			target="/mnt/is903-$LABEL"
			run_root mkdir -p "$target" 2>/dev/null || true
			if run_root mount -t iso9660 -o ro "$BLK" "$target" 2>/dev/null; then
				MOUNT_POINT=$target
			fi
		fi
	fi

	if [ -n "$MOUNT_POINT" ] && findmnt -n --source "$BLK" >/dev/null 2>&1; then
		ok "已挂载: $MOUNT_POINT"
	else
		warn "挂载失败,但烧录与校验已完成"
	fi
fi

# ---------------------------------------------------------------- 12. 核对

step "文件核对"

if [ "$DRY_RUN" -eq 1 ] || [ "$DO_MOUNT" -eq 0 ] || [ -z "$MOUNT_POINT" ]; then
	info "(跳过)"
else
	if [ "$SRC_TYPE" = "file" ]; then
		base=$(basename "$SRC")
		if [ -f "$MOUNT_POINT/$base" ]; then
			a=$(sha256sum "$SRC" | awk '{print $1}')
			b=$(sha256sum "$MOUNT_POINT/$base" | awk '{print $1}')
			if [ "$a" = "$b" ]; then
				ok "$base 哈希一致"
			else
				warn "$base 哈希不一致"
			fi
		else
			warn "挂载点里找不到 $base"
		fi
	else
		nfail=0; nok=0
		while IFS= read -r -d '' f; do
			rel=${f#"$SRC"/}
			dst="$MOUNT_POINT/$rel"
			if [ ! -f "$dst" ]; then
				warn "缺失: $rel"; nfail=$((nfail + 1)); continue
			fi
			a=$(sha256sum "$f" | awk '{print $1}')
			b=$(sha256sum "$dst" | awk '{print $1}')
			if [ "$a" = "$b" ]; then
				nok=$((nok + 1))
			else
				warn "不一致: $rel"; nfail=$((nfail + 1))
			fi
		done < <(find "$SRC" -type f -print0)
		ok "$nok 个文件哈希一致"
		[ "$nfail" -eq 0 ] || warn "$nfail 个文件有问题"
	fi
fi

# ---------------------------------------------------------------- 完成

echo
if [ "$DRY_RUN" -eq 1 ]; then
	printf '%s(dry-run 结束,未做任何写入)%s\n' "$C_YELLOW" "$C_RESET"
	exit 0
fi

printf '%s══ 烧录成功 ══%s\n' "$C_GREEN$C_BOLD" "$C_RESET"
printf '  设备 : %s\n' "$BLK"
printf '  卷标 : %s\n' "$LABEL"
[ -n "$MOUNT_POINT" ] && printf '  挂载 : %s\n' "$MOUNT_POINT"
printf '  校验 : 逐字节一致\n'

if [ "$KEEP_ISO" -eq 0 ] && [ -n "$TMP_ISO" ]; then
	info "临时 ISO 已清理"
fi
