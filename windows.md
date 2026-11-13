# Windows 后端实现与真机实测

> 设备：Innostor IS903「USB Flash CD-ROM」(UFCD)，USB `1f75:0903`
> 本机：Windows 11 (26100)，`FANG / CD-ROM on Flash / REV 1.00`，序列号 `000000000000000008`，盘符 `D:`
> 工具链：rustc 1.99 `x86_64-pc-windows-msvc`，**非管理员会话**
> 结论：CD-ROM 模式的完整烧录/校验/刷新链路在 Windows 上可用，且不需要管理员权限。

## 1. 平台映射

| 能力 | Linux | macOS | Windows |
| --- | --- | --- | --- |
| SCSI 传输 | `SG_IO` on `/dev/sg0` | libusb USB BOT | `CreateFileW(r"\\.\X:")` + `IOCTL_SCSI_PASS_THROUGH_DIRECT` |
| 设备识别 | sysfs `/sys/block` | IOKit `ioreg` | `GetLogicalDrives` + `STORAGE_DEVICE_DESCRIPTOR` |
| 卸载/刷新 | `udisksctl` / usb-storage rebind | `diskutil` / 释放 USB 接口 | `FSCTL_LOCK_VOLUME` + `FSCTL_DISMOUNT_VOLUME` |
| 挂载 | `udisksctl mount` | `diskutil mount` | 系统自动挂载，等待盘符可用 |
| 大容量存储写入 | 块设备 `write` + `O_DIRECT` | `/dev/rdiskN` + `F_NOCACHE` | `\\.\PHYSICALDRIVEn`（需要管理员） |
| 并发锁 | `flock` | `flock` | 独占共享模式（`share_mode(0)`）打开锁文件 |

代码位置：

* `is903burn-gui/src/winapi.rs` — Windows 底层（直通、属性查询、卷控制、物理盘定位）。
* `is903burn-gui/src/sg.rs` — 同一份协议实现，新增 `#[cfg(windows)]` 传输层与 `dump_iso`。
* `is903burn-gui/src/device.rs` — 新增 `mod windows`（扫描、锁文件、刷新）。
* `is903burn-gui/src/mount.rs` — 新增 `mod windows`（卸载=锁卷+卸载，挂载=等待盘符）。
* `is903burn-gui/src/burn.rs` — Windows 用直通 `READ(10)` 校验；大容量存储走物理盘写入。
* `is903burn-gui/src/job.rs` — 抽出 `write_disc`，新增 `burn_iso`（`--write`）。

## 2. 实现要点

### 2.1 直通必须用读写方式打开卷

```rust
CreateFileW(r"\\.\D:", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, …)
```

只读句柄（只有 `GENERIC_READ`）会让**所有** `DeviceIoControl` 返回
`ERROR_ACCESS_DENIED(5)`，包括 `INQUIRY`。读写句柄在普通用户会话下即可
发送 `0xFC` 厂商 CDB，**包括 data-out**（`P.M. 0xFF` 密钥应答、
`P.M. 0x00` 数据写入），因此烧录不需要提权。

### 2.2 单次传输上限 64 KiB

`SCSI_PASS_THROUGH_DIRECT` 的 `DataTransferLength` 超过驱动
`MaximumTransferLength` 时返回 `ERROR_INVALID_PARAMETER(87)`：

| 请求 | 结果 |
| --- | --- |
| `READ(10)` × 64 扇区 = 128 KiB | `GetLastError=87` |
| `READ(10)` × 32 扇区 = 64 KiB | 正常 |

协议本身的 `P.M.` 分块正好是 64 KiB（0x80 × 512 字节块），实现里保持
`CHUNK = 64 * 1024`，超出时 `winapi::Raw::cmd` 会先给出明确的错误。

### 2.3 刷新光驱视图

烧录后设备侧内容立即生效（`READ(10)` 能读到新卷标），但 Windows 仍使用
缓存的 CDFS 视图。三个 IOCTL 在普通用户下全部成功：

```rust
FSCTL_LOCK_VOLUME      // 0x00090018
FSCTL_DISMOUNT_VOLUME  // 0x00090020
FSCTL_UNLOCK_VOLUME    // 0x0009001C
```

dismount 之后 Windows 丢弃缓存，下一次访问重新读取 PVD/Joliet，卷标与
文件树都会更新，不需要重启设备、不需要 `SetupDiChangeState`、不需要管理员。
`IOCTL_STORAGE_CHECK_VERIFY2` 与 `IOCTL_DISK_UPDATE_PROPERTIES` 不能达到
同样的效果（后者返回 `ERROR_NOT_SUPPORTED`）。

### 2.4 校验

Windows 没有 `O_DIRECT` / `F_NOCACHE` 的等价物可跨 CD-ROM 使用，改为
直接用 SCSI 直通 `READ(10)` 回读：完全不经过文件系统缓存，读到的就是
设备上的实际内容。块大小取自设备报告的 `READ CAPACITY`（光盘 2048）。

### 2.5 大容量存储模式

该模式（SCSI 类型 0）才能直接写偏移 0，等价于 Linux 的块设备写入：
定位 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 得到 `\\.\PHYSICALDRIVEn`，写入后
`FlushFileBuffers`。Windows 上物理盘的写句柄需要管理员权限，且需先卸载
该盘上的卷，因此这条路径需要以管理员身份运行；本机设备不在该模式，
未做真机验证。

## 3. 真机实测

### 3.1 识别

```text
> is903burn-gui --devices
D:  vendor=FANG model=CD-ROM on Flash type=5 cap=29.8 GiB 写=false
```

容量来自 `READ CAPACITY`（15,597,568 × 2048 = 31,943,819,264 字节，即整片闪存），
光盘实际卷空间由 ISO 的 PVD 决定。

### 3.2 完整烧录（`--burn`，从文件构建 ISO）

```text
> is903burn-gui --burn D: -l WINTEST .\README.md
[  2.0%] 制作 ISO 镜像 … 镜像已生成，1 个文件，58.0 KiB
[ 27.0%] 卸载设备
[ 30.0%] 写入设备
[ 70.0%] 写入光驱
[ 72.0%] 刷新光驱
[ 76.0%] 回读整盘 … 整盘回读与 ISO 一致
[ 93.0%] 挂载并核对文件 … 1 个文件已核对，挂载于 D:\
✔ 成功: 1 个文件 / 0 个目录, 58.0 KiB (29 扇区)
```

烧录后 `Get-Volume` 显示卷标 `WINTEST`，`D:\README.md` 为 7942 字节，
SHA-256 与源文件一致（`a97c3a67…5f3a9`）。

### 3.3 现成 ISO 的写入与校验（`--write` / `--verify` / `--dump`）

```text
> is903burn-gui --write D: is903-backup.iso     # 13,551,616 字节，约 8 秒
已挂载于 D:\
> is903burn-gui --verify D: is903-backup.iso
整盘回读一致: is903-backup.iso
> is903burn-gui --dump D: current.iso
已导出 13551616 字节到 current.iso
```

回读 ISO 的 SHA-256 为 `20b93d2e…2c6e61f`；重新挂载后卷标 `IS903TEST`，
卷内 PDF 的 SHA-256 为 `a6b102418278196a149e2c3df921d2cea180ca4423f77011e948c8065eb8b30f`，
与仓库 `macOS.md` 记录的原始文件一致。

### 3.4 卷标刷新实验

在 64 KiB 块内改写 PVD/Joliet 卷标为 `WINTEST` 并写回：设备侧
`READ(10)` 立刻返回 `WINTEST`，而 `Get-Volume` 仍报告旧卷标 `IS903TEST`；
执行 lock+dismount 后重新打开，Windows 读到 `WINTEST`。恢复原数据后卷标与
内容回到 `IS903TEST`，整盘逐字节校验通过。

## 4. 限制与注意事项

* 单次直通缓冲 64 KiB，代码已按此分块。
* 大容量存储模式的原始写入需要管理员权限（未在本机实测）。
* ISO 长度需为 2048 字节的整数倍（`--write`/`--verify` 会检查）。
* Windows 会给盘符，设备重插后盘符可能变化；工具按 USB 序列号识别设备，
  但命令行参数仍需给出当前盘符（`--devices` 可查）。
* 设备固件收到畸形厂商命令仍可能卡死，需要物理拔插恢复。

## 5. 构建与运行

```powershell
powershell -ExecutionPolicy Bypass -File .\is903burn-gui\build-windows.ps1
.\is903burn-gui\target\release\is903burn-gui.exe                       # GUI
.\is903burn-gui\target\release\is903burn-gui.exe --devices
.\is903burn-gui\target\release\is903burn-gui.exe --burn D: -l LABEL .\data
```

## 6. 故障排查：程序显示「容量为 0」

如果 `--devices` 或界面里容量是 `0 B`，而设备明明插着，先看是不是
**程序所在目录的完整性级别**被降低了。从带 Low 完整性标签的目录启动的
程序会以 Low IL 运行，而 `\\.\D:` 是 Medium，写句柄会被直接拒绝
（`GetLastError=5`），扫描只能退化成只读句柄，于是读不到容量。

```cmd
icacls .                     :: 看目录是否有 "Mandatory Label\Low Mandatory Level"
icacls . /setintegritylevel (OI)(CI)M
icacls . /T /C /Q /setintegritylevel (OI)(CI)M
```

修复后重新启动程序即可（也可把整个目录拷到 `%TEMP%` 下试运行以验证）。
程序现在会把这种情况提示为“拒绝访问：……完整性级别过低”。
