# IS903 protocol notes

Reverse engineering of the Innostar IS903 "UFCD" (USB Flash CD-ROM) vendor
SCSI protocol, recovered from `AIBurn.exe` (PE32/i386 Delphi).

These notes are the reference for `is903burn.c`.

## 1. What AIBurn.exe is

* 32-bit Delphi GUI, window class `TFm_Main`, caption `AI-Burn`.
* Version string: `EAI-Burn 1.0.0.8 (C)Innostor Technology Corporation`.
* The Windows tool uses SCSI pass-through IOCTLs and SetupAPI
  (`SetupDiGetClassDevsW` / `CM_Get_Device_IDW`). The Linux tool uses
  block I/O for its supported burn path and SG_IO for experimental vendor
  commands.

## 2. Device

| property | value |
|---|---|
| USB VID:PID | `1f75:0903` |
| Product | `USB3.0`, Manufacturer `Innostor` |
| SCSI vendor | `Innostor` |
| SCSI product | `CD-ROM on Flash` |
| SCSI revision | `1.00` |
| SCSI type | 5 (CD-ROM) |
| Linux nodes | `/dev/sr0` (ISO 9660, read only), `/dev/sg0` for SG_IO |

The device exposes a virtual CD-ROM whose content lives in the chip's
flash. Linux only exposes the *read* side of that disc. Changing the disc
content requires the vendor protocol below.

## 3. The vendor SCSI CDB

All vendor commands use a 16-byte CDB with opcode `0xFC`:

```
 byte  0 : 0xFC             vendor opcode
 byte  1 : command group    index into the group table below
 byte  2 : sub opcode      operation within the group
 byte  3 : parameter
 bytes 4-7: offset         big endian 32 bit, in 512 byte blocks
 bytes 8-9: count          big endian 16 bit
 byte 10 : flag
 byte 11 : reserved (0)
 bytes 12-15: ASCII tag    repeats the command group name
```

The tag bytes are the most reliable part: the device echoes them back in
its own reply, and the group index in byte 1 must agree with the tag. The
device rejects a command whose tag does not match the group at that index.

Command groups, in the order the device advertises them:

| index | tag | purpose |
|---|---|---|
| 0 | `INQU` | identification / capability list |
| 1 | `PINF` | product / partition information |
| 2 | `EXEC` | execute mode |
| 3 | `NAND` | raw NAND access |
| 4 | `TEPF` | template / partition table |
| 5 | `NFTL` | NAND flash translation layer |
| 6 | `P.M.` | physical media (the stored ISO image) |

Sub opcodes of the `P.M.` group that AIBurn.exe uses:

| opcode | direction | meaning |
|---|---|---|
| `0x00` | Windows SPTD `DataIn=0` | used by the **Burn Image** loop at `0x410dd9` |
| `0x03` | Windows SPTD `DataIn=1` | used by the **Save ISO Image** loop and the preparation call below |
| `0x07` | from device | 16-byte response (called at `0x404dd3` / `0x404e25`) |
| `0x08` | from device | 2 byte challenge for the key exchange |
| `0xff` | to device | 12 byte key response |

### `P.M.` read / write details

From the helper routines at `0x40202c` (op `0x03`) and `0x402144` (op `0x00`):

* byte 3 is supplied by the caller. The Burn Image data loop uses `0x01`.
  The preparation call uses `0x05` with op `0x03`.
* the offset and count use 512-byte units. The ISO loop advances the LBA by
  `0x80` after each 64 KiB transfer and rounds a partial transfer up to 512 bytes.
* the count field is the number of 512 byte blocks, `0x80` per transfer
* the transfer length is `count * 512`

The Linux experimental `read` and `write` commands currently use the opposite
directions for those opcodes. Mock tests check the current Linux mapping.
See section 11 for the recovered Windows sequence and verification status.

### `INQU`

The reply starts with the list of supported command groups as a run of
4-character ASCII tags, followed by the model string (`IS903`), the vendor
string (`Innostor`) and the version. AIBurn.exe reads 0x86 bytes.

The tags are not necessarily 4-byte aligned inside the reply, so
`parse_groups()` looks for the longest run of consecutive known tags
rather than assuming an alignment.

## 4. The 12 byte key exchange

AIBurn.exe uses this challenge/response routine before and after ISO transfer:

1. `P.M.` op `0x08` reads 2 bytes from the device.
2. Those 2 bytes are combined big endian into a 16 bit value.
3. A fixed 11 byte seed `"Vm3RuP42j/ "` (at `0x5cd775` in the binary) is
   copied into a 12 byte buffer.
4. The buffer is scrambled with a 15 bit LFSR seeded from the challenge
   (function at `0x402684`).
5. The result is sent back with `P.M.` op `0xff`.

The LFSR step, reproduced in `scramble_key()`:

```
state = reverse of the low 15 bits of the 16 bit challenge
for each output byte:
    acc = 0
    repeat 8 times:
        b = (state ^ (state >> 1)) & 1
        acc = (acc >> 1) | (b << 7)
        state = ((state >> 1) | (b << 14)) & 0x7fff
    buffer[i] ^= acc
```

This was verified to be byte-identical to a direct transcription of the
original x86 code for all 65536 possible challenges.

## 5. Windows side (for reference)

`AIBurn.exe` uses `DeviceIoControl` with:

* `IOCTL_SCSI_PASS_THROUGH` (`0x4d004`) for the inquiry calls
* `IOCTL_SCSI_PASS_THROUGH_DIRECT` (`0x4d014`) for everything with data

The 32-bit Windows `SCSI_PASS_THROUGH_DIRECT` layout used here is:

```
+0x00 USHORT Length = 0x2c
+0x02 UCHAR  ScsiStatus
+0x03 UCHAR  PathId = 0
+0x04 UCHAR  TargetId = 1
+0x05 UCHAR  LunId = 0
+0x06 UCHAR  CdbLength
+0x07 UCHAR  SenseInfoLength = 0
+0x08 UCHAR  DataIn          0 = host to device, 1 = device to host
+0x0c ULONG  DataTransferLength
+0x10 ULONG  TimeOutValue    seconds (10 for the op 0x03 bulk helper)
+0x14 ULONG  DataBuffer
+0x18 ULONG  (0)
+0x1c UCHAR  Cdb[16]
```

The direction maps to `dxfer_direction` in Linux `SG_IO`. The buffer pointer
is 32 bits in this PE32 binary. The helpers copy the CDB to `+0x1c`.
[Microsoft SCSI_PASS_THROUGH_DIRECT reference](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntddscsi/ns-ntddscsi-_scsi_pass_through_direct)

On Linux the equivalent transport is plain `SG_IO` on `/dev/sg0`, which is
what `is903burn.c` uses. The Windows `\\.\H:` / `\\.\G:` volume handles
have no Linux counterpart.

## 6. Notes on the port

* The CDB is identical on both platforms; only the transport differs.
* Linux needs the `sg` module for `/dev/sg0` to appear
  (`sudo modprobe sg`). On the test host, read-only `/dev/sr0` accepted
  `READ CAPACITY(10)` via `SG_IO` and rejected vendor `INQU` with `EPERM`.
* The device firmware can wedge if it receives malformed vendor commands.
  A wedged device stops enumerating over USB and only recovers with a
  physical unplug/replug.

## 7. What was verified on real hardware

Tested against a real IS903 (`1f75:0903`) on Linux.

| command | result |
|---|---|
| standard `INQUIRY` / `READ CAPACITY` | OK |
| vendor `INQU` (group 0, op `0x00`) | OK |
| vendor `P.M.` op `0x07` | OK |
| vendor `P.M.` op `0x08` | OK |
| vendor `P.M.` op `0x00` (read image) | rejected |
| **block-device `dump` / `burn`** | **OK, full round trip** |

The `INQU` reply starts with a 6 byte header (`a5 01 02 15 81 <n>`), followed
by the command group tags, then the model (`IS903`), the vendor
(`Innostor`), a type byte, the version and a serial number.

### The device has two modes

The same chip enumerates differently depending on how it is powered up:

| mode | SCSI type | nodes | notes |
|---|---|---|---|
| CD-ROM | 5 | `/dev/sr0`, `/dev/sg0` | read only, ISO 9660 image, 15728640 blocks |
| USB mass storage | 0 | `/dev/sda`, `/dev/sg0` | writable, ISO image at offset 0, 63213404 blocks |

In CD-ROM mode the `P.M.` group only answers op `0x07` and op `0x08`; the
read/write opcodes are refused with `INVALID FIELD IN CDB`. In mass storage
mode the same opcodes answer, but the op `0x00` read fails with a
`HARDWARE ERROR` instead.

## 8. How the image is actually burned

The ISO image is stored **verbatim at offset 0 of the block device** in
mass-storage mode. Burning therefore does not use the vendor `P.M.`
commands at all:

```
read  the PVD at sector 16 (2048 byte sectors) -> volume space size
dump  copy that many sectors from offset 0
burn  write the ISO at offset 0, fsync, done
```

This was verified on hardware: writing a patched ISO (volume label
`IS903TEST`) and remounting made the kernel report the new label, and the
contained `AIBurn.exe` still matched its SHA-256. The original image was
then restored.

In CD-ROM mode (SCSI type 5) the block device is read only. The Linux
`burn` and `dump` commands currently reject type 5. The vendor path tested
earlier used the Linux opcode/direction mapping described in section 3.

The SCSI vendor string reported by the device is `Innostor`.

## 9. 光驱模式下的写入(实测记录)

设备的 `WRITE(12)` / `READ(12)` 使用**非标准 CDB 布局**:

```
字节 0     : 0xAA (WRITE) / 0xA8 (READ)
字节 1     : 0
字节 2..5  : LBA,大端 32 位
字节 6..9  : 必须为 0        ← 标准 WRITE(12) 的长度字段在这里,本设备忽略
字节 10    : 传输块数(单字节,1..255)
字节 11    : 0
```

用标准布局(长度写进 6..9)会得到 `INVALID FIELD IN CDB`(sense 05/20)。

### 已知能工作 / 不能工作的部分

| 操作 | 结果 |
|---|---|
| `READ(10)` (0x28) 读光盘内容 | ✅ |
| `READ CAPACITY(10)` | ✅ 最后一块 LBA=15728639,块大小 2048 |
| `WRITE(12)` (0xAA,长度在字节 10) | ✅ 被接受,0 错误 |
| 写入后经 `READ(12)` 回读 | ✅ 一致(来自设备写缓存) |
| **USB 重新枚举后回读** | ❌ 数据丢失,仍是旧内容 |
| 写入后经 `READ(10)` 读光盘视图 | ❌ 仍是旧内容 |
| `WRITE(10)` / `WRITE(16)` / `WRITE AND VERIFY` | ❌ `INVALID FIELD IN CDB` |
| `SYNCHRONIZE CACHE` (0x35 / 0x91) | ❌ 不支持 |
| `CLOSE TRACK/SESSION` (0x5B) | ❌ 不支持 |
| `BLANK` (0xA1) | ❌ 不支持 |
| `MODE SENSE` / `GET CONFIGURATION` | ❌ 不支持 |
| `START STOP UNIT` (弹出/装载) | ❌ 不支持 |
| `PREVENT ALLOW MEDIUM REMOVAL` | ✅ 接受,但不影响写入 |
| 厂商 `P.M.` op 0x00/0x01/0x03 | ❌ `INVALID FIELD IN CDB` |
| 厂商 `P.M.` op 0x07 / 0x08 / 0xFF | ✅ |
| 认证(op8 取 2 字节 + opFF 回写 12 字节密钥) | ✅ 命令接受,但写仍不落盘 |

### 地址空间

`READ(12) LBA 0` 的内容等于 `READ(10) LBA 16`(ISO 的主卷描述符),
说明两者寻址基准不同。但对 `WRITE(12)` 做同样假设测试并不成立。

### 结论

历史测试中的 `WRITE(12)` 数据没有进入 `READ(10)` 的光盘视图，重新枚举
后也未保留。第 11 节记录了 Windows 程序实际走的厂商命令及设备重启路径。

## 10. 模式切换与恢复的实测记录

### 观察到的三种枚举状态

| 状态 | model | type | 节点 | 容量 | 可写 |
|---|---|---|---|---|---|
| CD-ROM | `CD-ROM on Flash` | 5 | `/dev/sr0` | 15728640 块 × 2048 | 否(见第 9 节) |
| 大容量存储 | (空) | 0 | `/dev/sda` | 30 GB | 是 |
| 裸片(下载模式) | `NAND Flash` | 0 | `/dev/sda` | **0** | 否 |

### 触发条件

- 「大容量存储」状态是在 ISO 区被破坏后出现的,属于**降级/回落**,
  不是正常工作模式。写入一个合法 ISO 后设备会回到 CD-ROM。
- 「裸片」状态是在介质已被弹出(`NOT READY / MEDIUM NOT PRESENT`)的
  情况下再做 USB 去授权复位触发的。该状态下 `READ CAPACITY` 失败、
  `WRITE(10)` 返回 `NOT READY`。
- **物理拔插可以恢复**:裸片状态拔插后回到正常的 CD-ROM 模式,内容完好。

### 不可用的手段

| 手段 | 结果 |
|---|---|
| `START STOP UNIT` 弹/装介质 (0x1B) | ❌ `INVALID FIELD IN CDB` |
| USB 去授权(`authorized` 0/1)复位 | ⚠️ 可让设备重新枚举,但可能落入裸片状态 |
| `usb-storage` unbind/bind | ✅ 较温和,能可靠地重新枚举 |
| 硬件写保护开关切换 | 不影响枚举模式(实测仍为 type 5) |

**结论**:模式不是由主机命令主动切换的,而是固件根据内部状态(ISO 是否
有效、是否处于异常)自行决定。主机侧能做的是温和地重新枚举设备。

## 11. 单 ISO 烧录路径复核

对照设备原盘上的 AIBurn.exe 和仓库副本，SHA-256 均为
`81755ab6a34a93bbf07dbb460348eb4f05fbb3dbfdb2801067b5f093f9f0061f`。
下表的地址是 PE 虚拟地址，可以用 `objdump -d -Mintel AIBurn.exe` 复核。

| 调用点 | 反汇编中确认的操作 |
|---|---|
| `0x40ee84`、`0x40f2eb` | Delphi 事件表将 `BTNBurnISOClick` 绑定到前者。后者打开 `\\.\X:` 光驱卷句柄。 |
| `0x410aad` | 调用 `0x402804`。满足设备记录中的版本条件时，通过 `0x08` 取 2 字节挑战值，再以 CDB byte 3 = `0x80` 发送 12 字节 `0xff` 响应。 |
| `0x410afa` → `0x402e1c` → `0x40202c` | 分区记录辅助函数。先用 `P.M. 0x03`、byte 3 = `0x05`、LBA 0、count 8 请求 4096 字节，随后在缓冲区内查找匹配或空闲的 512 字节记录。 |
| `0x410ea7` → `0x410dd9` → `0x402144` | 从源文件读取最多 64 KiB，发送 `P.M. 0x00`、byte 3 = `0x01`。LBA 来自所选分区记录，单分区分支将它设为 0（`0x40ae14`）。 |
| `0x412667` | 再调用 `0x402804`，这次 `0xff` 响应的 CDB byte 3 = `0x81`。随后关闭 ISO 和设备句柄。 |
| `0x412734` → `0x404e54` | 成功分支调用 SetupAPI，设置 `DIF_PROPERTYCHANGE` 和 `DICS_PROPCHANGE`，再调用 `SetupDiChangeState`。Windows 对匹配的设备执行停止和重新启动。 |

以 LBA 0 的首个完整 64 KiB 为例，ISO 数据 CDB 为
`fc GG 00 01 00 00 00 00 00 80 00 00 50 2e 4d 2e`，其中 `GG`
是设备报告的 `P.M.` 组索引。上述数据阶段的 Windows `DataIn` 分别是
`0x08 → 1`、`0xff → 0`、准备 `0x03 → 1`、传输 `0x00 → 0`。
反向核对 `Save ISO Image`，`0x43913c` 调用 `0x40202c`，
返回后数据进入写文件流程，进一步确认这两条数据命令的方向。
[Microsoft SP_PROPCHANGE_PARAMS reference](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/ns-setupapi-sp_propchange_params)

Linux 真机测试已确认整套顺序可以更新光盘镜像，重新绑定 USB 存储接口以及
物理拔插后均可通过 `READ(10)` 完整回读。测试保留了 `0x81` 应答，
因此其独立作用尚未验证。

### 2026-11-13 Linux 复测

* 设备是 USB `1f75:0903`，SCSI 厂商字符串为 `FANG`，型号为
  `CD-ROM on Flash`，类型为 5。脚本按 VID/PID 补充识别后能找到 `/dev/sr0`。
* 宿主机上 `/dev/sr0` 只读，`O_RDWR` 返回 `EROFS`。缺少 `/dev/sgN`，
  加载 `sg` 需要本机 sudo 认证。未执行厂商写入。
* 用 `xorrisofs` 生成 15,736,832 字节的测试 ISO。用模拟块设备执行
  `burn` → `dump` → `cmp`，结果逐字节一致。原盘通过普通块读取备份了
  1376 个 2048 字节扇区。

### 2026-11-13 CD-ROM 真机烧录

* 通过系统认证加载 `sg` 后，`INQU` 确认序列号为
  `000000000000000008`，`P.M.` 组索引为 6，SCSI 类型为 5。
* 测试镜像只修改原盘的 ISO 9660 和 Joliet 卷标为 `IS903TEST`，
  大小仍为 2,818,048 字节。先卸载光驱，再通过 `/dev/sg0` 发送
  `0x80` 应答、4096 字节分区记录请求、5504 个 512 字节块的 ISO
  数据和 `0x81` 应答。所有命令均成功且无残余传输。
* 重新绑定目标 `usb-storage` 接口后，设备仍枚举为只读
  `/dev/sr0`。标准 `READ(10)` 回读 1376 个 2048 字节扇区，
  与测试 ISO 逐字节一致。重新挂载的卷标为 `IS903TEST`，
  其中 `AIBurn.exe` 与原盘文件逐字节一致。
* 物理拔插后，USB 设备地址从 11 变为 12，序列号一致，SCSI 类型仍为 5。
  再次用 `READ(10)` 回读整盘并通过 `cmp`，确认镜像在断电后保留。
* 测试 ISO 和回读的 SHA-256 均为
  `2cb30d05737b89256b366aba66a5129472f67333d1a73af14b24b0adf46991b5`。
  这一流程使用临时测试程序，现有 CLI 的 `burn` 仍走块写入路径。

### 2026-11-13 更换文件复测

* 将 `Aahj.ttf` 制作为卷标 `IS903FONT` 的 ISO，大小为 15,728,640 字节。
  按同一顺序通过 `/dev/sg0` 烧录，完整写入 30,720 个 512 字节块，
  准备和收尾应答均成功。
* 通过标准 `READ(10)` 回读全部 7680 个 2048 字节扇区，与源 ISO 逐字节一致，
  SHA-256 为 `565640b06cae9419b2e963b38271a1120659673b1a9a276dc044d3bbafddcc70`。
  重新绑定目标 USB 存储接口后再次执行整盘 `READ(10)`，结果一致。
  从回读 ISO 提取的字体及重新挂载后读取的字体均与源文件逐字节一致，SHA-256 为
  `92d6a6396da1ca6277538487b80a18b5fad2196827b05e87b3754219a9a904e5`。
