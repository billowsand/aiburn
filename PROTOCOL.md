# IS903 protocol notes

Reverse engineering of the Innostar IS903 "UFCD" (USB Flash CD-ROM) vendor
SCSI protocol, recovered from `AIBurn.exe` (PE32/i386 Delphi).

These notes are the reference for `is903burn.c`.

## 1. What AIBurn.exe is

* 32-bit Delphi GUI, window class `TFm_Main`, caption `AI-Burn`.
* Version string: `EAI-Burn 1.0.0.8 (C)Innostor Technology Corporation`.
* Windows-only: it reaches the device through the SCSI pass-through IOCTLs
  and SetupAPI (`SetupDiGetClassDevsW` / `CM_Get_Device_IDW`), so it cannot
  run natively on Linux as-is. The functional core is small, however, and
  is reimplemented in `is903burn.c`.

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
| `0x00` | from device | read blocks of the stored image |
| `0x03` | to device | write blocks of the stored image |
| `0x07` | to device | set title / label (16 bytes) |
| `0x08` | from device | 2 byte challenge for the key exchange |
| `0xff` | to device | 12 byte key response |

### `P.M.` read / write details

From the helper routines at `0x40202c` (write) and `0x402144` (read):

* byte 3 is always `0x01`
* the offset field counts 512 byte blocks and is advanced by `0x80` blocks
  (64 KiB) after every transfer
* the count field is the number of 512 byte blocks, `0x80` per transfer
* the transfer length is `count * 512`

AIBurn.exe burns an image by looping over 64 KiB chunks and issuing one
`P.M.` write per chunk, advancing the LBA by `0x80` each time. The Linux
implementation does exactly the same, and the chunking was verified against
the mock device.

### `INQU`

The reply starts with the list of supported command groups as a run of
4-character ASCII tags, followed by the model string (`IS903`), the vendor
string (`Innostor`) and the version. AIBurn.exe reads 0x86 bytes.

The tags are not necessarily 4-byte aligned inside the reply, so
`parse_groups()` looks for the longest run of consecutive known tags
rather than assuming an alignment.

## 4. The 12 byte key exchange

Before writing, AIBurn.exe performs a challenge/response:

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

The application does not use the stock Windows
`SCSI_PASS_THROUGH_DIRECT` layout. It lays the structure out as:

```
+0x00 USHORT Length = 0x2c
+0x02 UCHAR  ScsiStatus
+0x03 UCHAR  PathId = 0
+0x04 UCHAR  TargetId = 1
+0x05 UCHAR  LunId = 0
+0x06 UCHAR  CdbLength
+0x07 UCHAR  SenseInfoLength = 0
+0x08 UCHAR  Direction       0 = read, 1 = write
+0x0c ULONG  DataTransferLength
+0x10 ULONG  (10)
+0x14 ULONG  DataBuffer
+0x18 ULONG  (0)
+0x1c UCHAR  Cdb[16]
```

Two details worth noting:

* The direction is an explicit field in the struct, because the vendor
  opcode `0xFC` does not imply one. On Linux this maps to the
  `dxfer_direction` of `SG_IO`.
* The data buffer pointer is a plain 32-bit value, so the app can only
  address the low 2 GiB of its address space. It writes the CDB into the
  structure at `+0x1c` and then overwrites byte 0 with `0xFC`.

On Linux the equivalent transport is plain `SG_IO` on `/dev/sg0`, which is
what `is903burn.c` uses. The Windows `\\.\H:` / `\\.\G:` volume handles
have no Linux counterpart.

## 6. Notes on the port

* The CDB is identical on both platforms; only the transport differs.
* Linux needs the `sg` module for `/dev/sg0` to appear
  (`sudo modprobe sg`). `/dev/sr0` itself rejects `SG_IO`.
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

In CD-ROM mode (SCSI type 5) the block device is read only and the vendor
`P.M.` opcodes are refused, so `burn` and `dump` report that the
mass-storage mode is required.

The SCSI vendor string reported by the device is `Innostor`.
