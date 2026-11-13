# is903burn — native Linux replacement for AIBurn.exe

`AIBurn.exe` is a 32-bit Windows Delphi GUI that burns ISO images onto an
Innostar IS903 "USB Flash CD-ROM" (UFCD) chip using vendor SCSI commands.
`is903burn` is a native Linux reimplementation of that protocol, speaking
`SG_IO` directly to `/dev/sg0`.

**图形界面(推荐)**

`is903burn-gui/` 是支持 IS903 光驱和大容量存储模式的 Rust + egui 桌面程序。

```sh
cd is903burn-gui
cargo build --release
./target/release/is903burn-gui
```

选择设备，添加或拖入文件和文件夹，设置卷标后即可烧录。程序生成 ISO 并检查
容量，卸载设备后在后台写入。光驱模式使用已在真机验证的 `P.M.` 命令，
随后重新绑定同一 USB 存储接口，再用 `READ(10)` 回读整盘。大容量存储
模式使用块写入与 `O_DIRECT` 回读。整盘验证、自动挂载和文件哈希核对
都通过后，界面才显示烧录完成。

光驱模式需要加载 `sg`，并有目标 `/dev/sgN` 的读写权限。挂载和卸载
使用 `udisksctl`，重新绑定 USB 存储接口时系统可能通过 `pkexec` 请求授权。

ISO 由程序内部纯 Rust 生成(ISO 9660 + Joliet,支持中文文件名),不依赖
`xorrisofs` 等外部工具。

命令行自检:

```sh
./target/release/is903burn-gui --devices                    # 列出设备
./target/release/is903burn-gui --mkiso <来源…> -l L -o F    # 只生成 ISO
./target/release/is903burn-gui --burn <块设备> -l L <来源…> # 无界面烧录
```

**Documentation**

* [使用流程.md](使用流程.md) — 完整操作手册(中文):环境准备、设备确认、
  提取镜像、制作 ISO、烧录、校验、故障排查、一键脚本。
* `is903-burn.sh` — 一键烧录脚本(推荐日常使用)。
* [PROTOCOL.md](PROTOCOL.md) — the reverse engineered vendor SCSI protocol.
* [README.md](README.md) — this file: build and command reference.

## Build

```sh
make
```

Requirements: a C compiler and the `scsi/sg.h` header (glibc).

## Setup

The Linux kernel needs the `sg` module for `/dev/sg0` to appear:

```sh
sudo modprobe sg
```

On Arch/Omarchy the module is not loaded by default. If `/dev/sg*` is
missing, load it first.

You also need read/write access to `/dev/sg0`. Depending on the mode the
device comes up in, it may be owned by `root:optical` or `root:disk`:

```sh
sudo setfacl -m u:$USER:rw /dev/sg0
```

## Quick start: the wrapper script

For everyday use there is `is903-burn.sh`, which wraps the whole flow
(build ISO → unmount → capacity check → burn → cache flush → verify →
mount → per-file hash check):

```sh
./is903-burn.sh ~/Downloads/Aahj.ttf        # burn a file
./is903-burn.sh ~/Documents -l MYDATA       # burn a folder
./is903-burn.sh ./data --dry-run            # show what would happen
```

See [使用流程.md](使用流程.md#附-a一键脚本-is903-burnsh) for all options.

## Usage

```
is903burn list                        # find the IS903
is903burn info                        # identification / capability list
is903burn capacity                    # standard SCSI READ CAPACITY of the disc
is903burn status                      # P.M. status/info sub commands
is903burn dump  <out.iso>             # extract the ISO image   (VERIFIED)
is903burn burn  <in.iso>               # burn an ISO image       (VERIFIED, destructive)
is903burn read  <lba> <blocks> <out>  # raw P.M. read (vendor protocol)
is903burn write <lba> <in.iso>         # raw P.M. write (vendor protocol)
is903burn key   [--pcs n]             # 12 byte challenge/response exchange
is903burn raw   <hex-cdb> [dir] [len] # send an arbitrary vendor CDB
```

`dump` and `burn` are the ones to use: they operate on the block device and
have been verified end to end on real hardware. `read`/`write` use the
vendor `P.M.` commands decoded from AIBurn.exe, which the device refuses in
both of its modes.

The device can be selected with `--dev /dev/sg0` or `--dev <index>`; by
default the first IS903 found is used.

### Examples

```sh
$ is903burn list
NODE       VENDOR       PRODUCT    TYPE NOTE
/dev/sg0   Innostar    CD-ROM on Flash   5   <- IS903
           /dev/sr0

$ is903burn info
INQU reply (134 bytes):
  ...
supported command groups:
  0  INQU
  1  PINF
  2  EXEC
  3  NAND
  4  TEPF
  5  NFTL
  6  P.M.
```

Extract the image currently on the device:

```sh
$ is903burn dump current.iso
ISO volume: 1376 sectors of 2048 bytes (2818048 bytes)
wrote current.iso
```

Burn a new image (destructive — this overwrites the disc content):

```sh
$ is903burn burn new.iso --force
wrote 2818048 bytes (1376 ISO sectors) to /dev/sda
```

Unmount the device first, and remount it afterwards for the new content to
show up:

```sh
udisksctl unmount -b /dev/sda
is903burn burn new.iso --force
udisksctl mount   -b /dev/sda
```

`burn` refuses to run without `--force`.

### Burning a folder or file

`is903burn` burns ISO images. To put arbitrary files on the device, build an
ISO first with `xorrisofs` (package `xorriso`):

```sh
xorrisofs -o data.iso -V MYDISC -J -R /path/to/folder

udisksctl unmount -b /dev/sda
is903burn burn data.iso --force
udisksctl mount -b /dev/sda      # now mounted as /run/media/$USER/MYDISC
```

`-J` adds a Joliet tree (long file names, UTF-8) and `-R` adds Rock Ridge
(POSIX permissions, symlinks). Use `-J` alone if the disc should be readable
on Windows as well.

A smaller ISO may be burned over a larger one; the extra old data is simply
ignored because the ISO 9660 volume descriptor declares the volume size.

## Tests

```sh
./tests/run-tests.sh
```

The test suite runs against a mock IS903 device (`tests/mock_is903.c`, a
`LD_PRELOAD` shim that emulates `SG_IO`), so it needs no hardware.

`./test-is903.sh` is the equivalent smoke test against real hardware.

## Notes and caveats

* `dump` and `burn` have been verified end to end on real hardware. They
  work on the block device, which requires the chip to come up in
  mass-storage mode.
* The `read`/`write` subcommands use the vendor `P.M.` commands decoded
  from AIBurn.exe. The device refuses those opcodes in both of its modes,
  so they are kept only for protocol exploration.
* The chip enumerates in one of two modes depending on how it is powered
  up: as a CD-ROM (SCSI type 5, `/dev/sr0` plus `/dev/sg0`, read only) or
  as USB mass storage (SCSI type 0, `/dev/sda` plus `/dev/sg0`,
  writable). Only the mass-storage mode can be burned.
* `burn` is **destructive** and overwrites the image from offset 0. The
  device is left with whatever the ISO contains.
* The device firmware can wedge if it receives malformed vendor commands.
  A wedged device does not re-enumerate over USB; it needs a physical
  unplug/replug.
* `raw` and `probe` are provided for protocol exploration. Be careful with
  them — sending a bogus command can hang the device firmware. `probe`
  therefore requires `--force` as well.
* The command group index in CDB byte 1 must match the ASCII tag in bytes
  12-15. The tool discovers the real group list from the device with
  `INQU` at startup, so it works even if the device implements a different
  set of groups than the default table.
