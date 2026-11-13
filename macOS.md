# 在 macOS 上跑通 IS903 烧录工具 —— 调研与实现记录

## macOS 原生 CD-ROM 烧录

本机已通过管理员只读探测。设备返回 `FANG / CD-ROM on Flash`，`READ CAPACITY` 返回末尾 LBA 15597567、块大小 2048，厂商 `INQU` 返回 134 字节并确认 IS903。释放 USB 接口后，macOS 将同一设备从 `disk6` 重新枚举为 `disk4`。

Rust 工具的 macOS 后端已接入 libusb USB BOT。它按 IORegistry 的 USB sessionID 选定设备，复用现有厂商烧录协议，释放接口并等待光驱重新枚举，然后重新接管并用 `READ(10)` 逐字节校验整个 ISO，最后挂载 CD-ROM 数据轨并核对文件。macOS Release 构建及 ARM Linux 交叉检查已通过。实机已通过 CD-ROM 模式的完整烧录与校验。

本次使用 1 个 PDF 生成 6617 个 2048 字节扇区的 ISO（约 12.9 MiB）。厂商命令写入后重新枚举，`READ(10)` 回读的镜像与原 ISO 逐字节一致。macOS 将 `/dev/disk4s0` 以 ISO 9660 只读挂载于 `/Volumes/IS903TEST`。源 PDF 和挂载后的 PDF 均为 13,498,387 字节，独立 `cmp` 通过，SHA-256 均为 `a6b102418278196a149e2c3df921d2cea180ca4423f77011e948c8065eb8b30f`。本次未做断电拔插复测。

在仓库根目录运行。编译需要 libusb 1.0.30，本机已安装。

```sh
./is903burn-gui/build-macos.sh
is903burn-gui/target/release/is903burn-gui --devices
```

使用列表中的 IS903 **整盘**盘号。烧录会覆盖旧镜像，请先备份所需内容。

```sh
sudo ./is903burn-gui/target/release/is903burn-gui --burn /dev/disk4 -l IS903TEST /path/to/file
```

若已有当前光盘对应的 ISO，可以先用只读入口验证重新枚举和回读链路。

```sh
sudo ./is903burn-gui/target/release/is903burn-gui --verify-cd /dev/disk4 /path/to/current.iso
```

下面保留设备枚举及旧版块写入的调研记录。旧版的 CD-ROM 写入限制仅适用于接入 libusb 前的版本。

## 本机 IS903 实测（macOS 27.0.1 / Apple Silicon）

- USB `1f75:0903`（FANGLITECH / FLASH DISK），接口 0 的 class/subclass/protocol 为 `08/06/50`（SCSI transparent / USB Bulk-Only Transport）。
- 系统识别为 SCSI type 5，整盘为 `/dev/disk6`，数据轨 `/dev/disk6s0` 以 ISO 9660 只读挂载在 `/Volumes/THE_ECONOMIST_20`。已成功读取卷内 EPUB 并计算 SHA-256。盘号可能随重插变化。
- 已修复 Rust 工具的扫描逻辑。原来的文本解析漏掉以竖线起始的 IORegistry 子节点，并匹配了错误的节点类别。现在从同一个 USB 子树的整盘 `IOCDMedia` 或可写 `IOMedia` 读取属性，真机 `--devices` 返回 `/dev/disk6 ... type=5 ... 写=false`。列表容量来自整盘 IORegistry 几何信息。当前光驱整盘采用 2352 字节扇区，数据轨采用 2048 字节扇区。实际 ISO 写入上限应由 SCSI `READ CAPACITY` 和协议限制确定。
- 设备的 IORegistry 树未公布 `SCSITaskDeviceCategory`。在 `IOSCSILogicalUnitNub` 上申请 MMC 和 SCSITask 插件，均返回 `0xe00002c7`（`kIOReturnUnsupported`）。Apple 有用户态 SCSI API，但本次探测未找到可用的插件入口。全程未卸载卷或写入设备。
- 原生 CD-ROM 后端优先验证 Apple 的 [IOUSBHost DeviceCapture](https://developer.apple.com/documentation/iousbhost/iousbhostobjectinitoptions/devicecapture?language=objc)。它可以在有权限的情况下接管 USB 设备，释放后会触发设备重置和驱动重新匹配。实验顺序为卸载数据轨、按设备身份接管接口 0、发标准 `INQUIRY` 和 `READ CAPACITY`，再发只读厂商 `INQU` 确认 IS903 响应。通过后将 `sg.rs` 中的协议逻辑与 Linux `SG_IO` 传输拆开，在 macOS 端实现 BOT 传输，最后验证烧录后重新枚举与 ISO 回读。本机最终使用 libusb 完成了设备接管和 CD-ROM 烧录。


> 目标设备:Innostar IS903「USB Flash CD-ROM」(UFCD),USB `1f75:0903`
> 代码位置:`is903burn-gui/`(Rust + egui 图形界面)、`is903burn.c`(Linux CLI)、
> `is903-burn.sh`(Linux 一键脚本)
> 本机:macOS 27.0.1 / Apple Silicon(Mac mini),rustc 1.98

## 原生 USB 传输探测

`tools/is903-macos-probe.c` 是只读验证工具。普通模式读取 USB 描述符和驱动状态。实测 libusb 1.0.30 在本机能打开这台 IS903，识别到 bulk IN `0x81` 和 OUT `0x02`，确认接口 0 由系统驱动占用。Codex 沙箱内 libusb 枚举不到该设备。普通用户在沙箱外可枚举，接管需要管理员授权。

从仓库根目录执行：

```sh
libusb_prefix=$(brew --prefix libusb)
clang -Wall -Wextra -Werror -I"$libusb_prefix/include/libusb-1.0" \
  tools/is903-macos-probe.c -L"$libusb_prefix/lib" -lusb-1.0 \
  -o /tmp/is903-macos-probe
/tmp/is903-macos-probe
```

确认 `diskutil list external` 仍显示 IS903 为 `disk6` 后，在本机终端卸载卷并运行探测：

```sh
diskutil unmountDisk /dev/disk6 && sudo /tmp/is903-macos-probe --capture
```

`--capture` 通过 libusb 尝试解除驱动绑定并占用接口。成功后依次发送 `INQUIRY`、`READ CAPACITY(10)` 和厂商 `INQU`，校验 BOT 返回状态并释放接口。它会短暂中断系统对设备的访问，释放后设备可能重新枚举。请保留完整输出，尤其是 `USB capture/claim` 错误或 `Vendor response confirms IS903`。若 libusb 无法接管，下一步用 Apple `IOUSBHostDevice` 的 DeviceCapture 单独验证。本机只读命令已通过，原生后端已接入。

---

## 1. 项目本身:它到底做了什么

芯片是「伪装成光驱的 U 盘」,固件会在两种枚举状态之间切换:

| 模式 | Linux 节点 | 可写 | 写入方式 |
|---|---|---|---|
| CD-ROM(SCSI type 5) | `/dev/sr0` + `/dev/sg0` | ❌ | 厂商 SCSI 命令(opcode `0xFC`,组 `P.M.`),Windows 的 AIBurn.exe 就是这么烧的 |
| 大容量存储(type 0) | `/dev/sda` + `/dev/sg0` | ✅ | **普通块写入**:ISO 原样写到偏移 0 |

关键结论(`PROTOCOL.md` 第 8 节,真机验证过):

> ISO 镜像原样存放在块设备偏移 0 处,所以「烧录」在 type 0 模式下
> **根本不需要厂商私有命令**,写块设备即可。

两条烧录路径:

* **块写入路径**(type 0):`burn.rs` → `write_iso()` 顺序写 `/dev/sdX`,
  再用 `O_DIRECT` 回读整盘比 SHA-256。**这一条是跨平台的**。
* **厂商命令路径**(type 5):`sg.rs` → `SG_IO` ioctl 发 16 字节 CDB
  (`0xFC`,组 `P.M.`),含 12 字节 LFSR 密钥交换。**这一条是 Linux 专属的**。

Linux 上的完整流程(`is903-burn.sh`)是:做 ISO → 卸载 → 容量检查 → 写块设备 →
`blockdev --flushbufs` → `dump`+`cmp` 校验 → 挂载 → 逐文件哈希核对。

---

## 2. macOS 的障碍逐项拆解

| Linux 依赖 | 用在哪 | macOS 现状 | 处理方式 |
|---|---|---|---|
| `/dev/sgN` + `SG_IO` ioctl | `sg.rs` 厂商 CD-ROM 写入/回读 | 没有 `/dev/sg*` 或 `SG_IO`，本机 SCSITask 插件探测未通过 | Linux USB 直通，或验证 macOS USB 接管 |
| `/sys/block` 扫描 | `device.rs` 找设备 | 不存在 | 改用 IOKit(`ioreg` 解析 IOService 树),本次已实现 |
| `udisksctl` 挂载/卸载 | `mount.rs` | 不存在 | 改用 `diskutil mount / unmountDisk`,本次已实现 |
| `O_DIRECT` 回读 | `burn.rs` 校验 | macOS 没有 `O_DIRECT` | 用 `fcntl(F_NOCACHE)` 绕过缓存,本次已实现 |
| `posix_fadvise(DONTNEED)` | `burn.rs` 丢缓存 | macOS 没有 fadvise | 退化为空操作(回读已用 `F_NOCACHE`) |
| `usb-storage` unbind/bind | `device.rs` 重新枚举 | macOS 没有这套机制 | 只能等设备自己重新枚举,否则提示拔插 |
| `flock` USB 设备目录 | `job.rs` 互斥锁 | 目录不能 flock | 改成 `/var/tmp/is903burn-<serial>.lock` |
| `scsi/sg.h` + glibc | `is903burn.c` CLI | macOS 无此头文件 | C 版 CLI 无法在 macOS 编译,只保留 GUI/CLI(Rust) |
| `/dev/diskN` 权限 | 写入 | `root:operator` 读写 | 管理员用户在 `operator` 组,通常**不需要 sudo**(实测 `ls -l` 为 `-rw-r----- root operator`) |

---

## 3. 本次已经做的 macOS 适配(可编译、可运行)

改动只在 `is903burn-gui/`,Linux 侧逻辑保持不变(用 `cargo check --target
x86_64-unknown-linux-gnu` 验证过两个平台都能编译):

* `src/burn.rs`
  * `posix_fadvise` / `O_DIRECT` 换成平台分支;macOS 用 `F_NOCACHE`。
  * macOS 上写入与回读都走**原始设备** `/dev/rdiskN`,绕开块层缓存。
  * `O_EXCL` 只在 Linux 上使用。
* `src/device.rs`
  * 拆成 `linux` / `macos` 两个模块。
  * macOS 版:解析 `ioreg -p IOService -l` 的设备树 → 找
    `IOUSBHostDevice` 且 `idVendor=0x1f75`/`idProduct=0x0903` → 顺着
    子树找到整盘 `IOCDMedia` 或可写 `IOMedia` → `/dev/diskN`。
  * 容量和可写性来自媒体节点的 `Size` 和 `Writable` 属性。
* `src/mount.rs`:`udisksctl` → `diskutil`(`unmountDisk` / `mount`,
  挂载点从 `diskutil info` 的 `Mount Point:` 读)。
* `src/job.rs`:macOS 上写完 ISO 后设备可能立刻切回只读 CD-ROM 模式、
  磁盘节点消失,此时直接报告「写入完成,请拔插后重新插入」,而不是报错。
* `is903-burn-macos.sh`:对应 Linux `is903-burn.sh` 的一键脚本(仅 type 0 路径):
  找设备 → 卸载 → `--mkiso` 生成 ISO → `dd` 写 `/dev/rdiskN` →
  回读 SHA-256 比对 → 挂载。

验证情况:

```sh
cd is903burn-gui
cargo build --release          # ✅ macOS 通过
cargo check --target x86_64-unknown-linux-gnu   # ✅ Linux 通过
./target/release/is903burn-gui --devices        # ✅ 无设备时不崩
./target/release/is903burn-gui --mkiso README.md -l TEST -o /tmp/t.iso
#   → sectors=29 bytes=59392 files=1 dirs=1   (纯 Rust ISO 9660+Joliet,跨平台 OK)
```

> 原生 CD-ROM 烧录结果见本文开头。大容量存储模式的 macOS 块写入仍待实测。

---

## 4. 三条落地路线

### 路线 A(推荐,当天可用):Linux 虚拟机 + USB 直通

macOS 做不到的部分只有「发厂商 SCSI 命令」,而这一部分**在 Linux 里现成**。
所以最省事、最可靠的做法是:在 Mac 上跑一个 Linux 虚拟机,把 IS903 直通进去,
Linux 虚拟机里跑原封不动的工具。

* **UTM(QEMU,免费)**:用 `usb-host` 直通最稳——把整个 USB 设备交给客户机,
  客户机内核自己跑 `usb-storage` + `sg`,厂商 CDB 依旧可用:
  ```sh
  qemu-system-aarch64 -M virt -cpu host -m 8G \
    -device qemu-xhci,id=xhci \
    -device usb-host,vendorid=0x1f75,productid=0x0903,bus=xhci.0 \
    -drive if=none,id=root,file=ubuntu.img,format=qcow2 \
    -device virtio-blk-pci,drive=root -nographic
  ```
  注意:**不要**用 QEMU 的 `scsi-generic host=/dev/sg0`(那是宿主机 Linux 才有的),
  要用 USB 直通,让客户机内核去做 SCSI 层。
* **VirtualBox / Parallels / VMware Fusion**:图形界面里加 USB 过滤器
  (选 `1f75:0903`),同样直通整个设备。
* **最省事的替代**:一台便宜的 Linux 小主机 / 树莓派,把芯片插上去,
  Mac 只负责发文件。

客户机(Ubuntu/Debian)里:
```sh
sudo modprobe sg
sudo setfacl -m u:$USER:rw /dev/sg0
sudo apt install udisks2 xorriso          # xorriso 可选,GUI 自带 ISO 生成器
cd is903burn-gui && cargo build --release && ./target/release/is903burn-gui
```
只有「CD-ROM 模式写入」需要 `sg`;只用「大容量存储模式」块写入时,连 `sg` 都不需要。

### 路线 B(已实现,纯 macOS):只做「大容量存储」模式的块写入

macOS 原生已经能完成:type 0 模式下 `dd`/GUI 写 `/dev/rdiskN`,回读校验,
挂载核对文件。**前提是芯片此刻处于 type 0 模式**,而模式由固件自己决定
(`PROTOCOL.md` 第 10 节):

* 正常上电是 CD-ROM 模式(type 5,只读)→ macOS 上只能读,不能写;
* ISO 区损坏时固件会降级到 type 0 → 此时可以写;
* 写进一个合法 ISO 后设备又会回到 CD-ROM 模式。

所以路线 B 的实际用法是:**先让设备进入 type 0,再烧**。
能不能在 macOS 上稳定地把设备逼进 type 0,需要真机试(见 §6 的实验 1)。
这也是路线 B 目前最大的不确定点。

### 路线 C:在 macOS 上补齐厂商 CDB 写入(研究性)

CD-ROM 模式才是「正规」烧录路径(AIBurn.exe 走的就是它)。要在 macOS 用户态
发厂商 CDB,只有两个方向:

1. **自己实现 SCSI over BOT**:芯片是标准 Bulk-Only Transport 设备,
   可以用 `rusb`(libusb)在 USB 层自己发 CBW(`0xFC` + 组标签 + LBA + 计数),
   按 `PROTOCOL.md` 第 3/4/11 节实现:`INQU` 取 `P.M.` 组索引 →
   `op 0x08` 取 2 字节挑战 → LFSR 算出 12 字节应答 `op 0xFF` →
   `op 0x03 / byte3=0x05` 读 4096 字节分区记录 →
   `op 0x00 / byte3=0x01` 分片写 ISO →
   `op 0xFF / byte3=0x81` 收尾 → 重新枚举 → `READ(10)` 回读整盘。
   **主要风险**:macOS 的 `AppleUSBHostMassStorage` 已经占了这个接口,
   libusb 的 Darwin 后端支持在符合权限条件时接管整个设备。
   可优先用 Apple IOUSBHost API 验证本机能否独占接口。
2. **写 kext / 用第三方驱动**:能拿到 `IOUSCSIPeripheralDevice`,但需要
   第三方驱动/内核扩展,成本和签名(Apple Silicon 必须签名)都很高。

### 路线 C':Windows 虚拟机跑原版 AIBurn.exe

Parallels / VMware Fusion / UTM(Windows 11 ARM)里装 Windows,USB 直通芯片,
直接跑原厂 `AIBurn.exe`——这是「参考实现」,协议细节仓库里已经逆向完
(`PROTOCOL.md` 第 11 节),可用于交叉验证路线 B/C 的结果。

---

## 5. 路线对比

| | 路线 A:Linux VM | 路线 B:原生 macOS 块写 | 路线 C:原生 macOS 厂商 CDB |
|---|---|---|---|
| 覆盖 CD-ROM 模式写入 | ✅ | ❌ | ✅(待做) |
| 覆盖 type 0 块写入 | ✅ | ✅ | ✅ |
| 需要真机外的额外依赖 | 虚拟机 | 无 | libusb / 可能需要 kext |
| 工作量 | 小时级 | 已完成主体 | 数天~数周 |
| 出错风险 | 低 | 中(依赖固件模式) | 高(接口占用) |
| 适合 | **首选** | 快速验证 | 长期方案 |

---

## 6. 拿到硬件后的验证清单

按顺序做,每步都能独立判定成败:

1. **确认 macOS 能不能认到设备**
   ```sh
   system_profiler SPUSBDataType | grep -A5 -i "1f75\|Innostar"
   ioreg -p IOService -l | grep -B5 -A20 "idProduct\" = 2307"
   ./target/release/is903burn-gui --devices
   ```
   若 `--devices` 为空，先在 IORegistry 核对 USB VID/PID、整盘媒体节点和 BSD Name。
2. **确认当前模式**
   ```sh
   diskutil info /dev/diskN | grep -E "Media Read-Only|Disk Size|Protocol"
   ```
   `Media Read-Only: Yes` ⇒ CD-ROM 模式(type 5),macOS 只能读。
3. **type 0 模式下的读**:把设备里的 ISO 读出来比对
   ```sh
   dd if=/dev/rdiskN bs=2048 count=1376 of=/tmp/cur.iso
   file /tmp/cur.iso        # 期望 ISO 9660
   ```
4. **type 0 模式下的一次完整烧录**(备份原盘后再做):
   ```sh
   sudo ./is903-burn-macos.sh ~/somefile -l TEST
   ```
   期望:`回读一致`,挂载后能读到文件、中文名正常(Joliet)。
5. **拔插后验证落盘**:设备回到 CD-ROM 模式后,再读一次,确认内容还在
   (Linux 上的经验:写入后必须重新枚举,否则挂载视图是旧内容)。
6. **固件模式切换实验**(路线 B 的关键未知数):
   反复拔插 / 在 CD-ROM 模式下尝试触发复位,记录设备在什么条件下掉到
   type 0;Linux 上已经知道 `usb-storage` unbind/bind 是「温和的」重新枚举手段,
   macOS 上没有等价物,可能要靠物理拔插。
7. **路线 C 的可行性实验**:写一个 30 行的 `rusb` 程序,尝试
   `claim_interface` 那个 bulk-only 接口,并发一条 `INQUIRY` 看是否成功。
   成功后再验证只读厂商命令。如果 libusb 接管失败，单独验证 IOUSBHost DeviceCapture。

---

## 7. 已知风险与未解决问题

* 原生 CD-ROM 烧录已验证 USB 接管、重新枚举和 ISO 9660 读取。大容量存储模式仍待实测。
* 芯片处于 CD-ROM 模式时 macOS 可能完全不建盘节点,那时 GUI 会显示「无设备」。
* macOS 上没有厂商命令 → 「写入后设备切回只读」这一段无法程序化处理,
  只能提示用户拔插,体验不如 Linux 自动化。
* `is903burn.c` CLI 在 macOS 上无法编译(依赖 `scsi/sg.h`),只能当协议参考。
* Linux 那边唯一还需要注意的:`is903burn.c` 的 `read`/`write` 子命令按
  `PROTOCOL.md` 第 3 节是**方向反了的**,不要拿它们真烧;用 `burn`/`dump`。

---

## 8. 结论

* **要「今天就能烧」**:路线 A —— Linux 虚拟机 + USB 直通,现有工具一行不改
  就能用,包括 CD-ROM 模式的厂商写入。
* **要「在 macOS 上原生跑」**:路线 B 已经落地(本次改动),但只能在芯片处于
  「大容量存储」模式时可写,CD-ROM 模式写入仍待实现。
* **要「macOS 原生 + CD-ROM 模式」**:路线 C,核心是用 libusb/BOT 自己发
  SCSI CDB,协议已完全逆向清楚,主要障碍是 macOS 内核驱动的接口占用。
