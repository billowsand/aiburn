//! Windows 平台底层：SCSI 直通、卷控制、驱动器枚举。
//!
//! 与 Linux 的 `SG_IO` / macOS 的 libusb BOT 对应。要点：
//!
//! * 用 `CreateFileW(r"\\.\D:", GENERIC_READ|GENERIC_WRITE)` 打开卷，
//!   非管理员即可收发厂商 `0xFC` CDB，**包括 data-out**。
//!   只读句柄连 `INQUIRY` 都会返回 `ERROR_ACCESS_DENIED`。
//! * 单次数据缓冲上限 64 KiB，更大值会被驱动以
//!   `ERROR_INVALID_PARAMETER` 拒绝。
//! * 烧录后不需要重启设备，也不需要提权：
//!   `FSCTL_LOCK_VOLUME` + `FSCTL_DISMOUNT_VOLUME` + `FSCTL_UNLOCK_VOLUME`
//!   就会让 Windows 丢弃 CDFS 缓存并重新读取新的镜像。

use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::path::Path;

pub type Handle = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sa: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        h: Handle,
        code: u32,
        inbuf: *mut c_void,
        inlen: u32,
        outbuf: *mut c_void,
        outlen: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLastError() -> u32;
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const OPEN_EXISTING: u32 = 3;

/// `IOCTL_SCSI_PASS_THROUGH_DIRECT`
pub const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x4D014;
/// `IOCTL_STORAGE_QUERY_PROPERTY`
pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x2D1400;
/// `IOCTL_STORAGE_GET_DEVICE_NUMBER`
pub const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x2D1080;
pub const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
pub const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
pub const FSCTL_UNLOCK_VOLUME: u32 = 0x0009_001C;

const SENSE_LEN: usize = 32;
const BUS_TYPE_USB: u32 = 7;
const DRIVE_CDROM: u32 = 5;

/// `SCSI_PASS_THROUGH_DIRECT`（x64 布局）
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SptDirect {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    time_out_value: u32,
    data_buffer: *mut u8,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

#[repr(C)]
struct Packet {
    spt: SptDirect,
    sense: [u8; SENSE_LEN],
}

#[repr(C)]
struct QueryProperty {
    property_id: u32,
    query_type: u32,
    additional: [u8; 1],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

fn wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// `D:` / `D:\` / `\\.\D:` → `\\.\D:`
pub fn volume_path(blk: &Path) -> Result<String> {
    let text = blk.to_string_lossy();
    let text = text.trim();
    let text = text.strip_prefix(r"\\.\").unwrap_or(text);
    let text = text.trim_end_matches(['\\', '/']);
    let mut chars = text.chars();
    let Some(letter) = chars.next() else {
        bail!("设备路径为空");
    };
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') || chars.next().is_some() {
        bail!(
            "Windows 上请使用驱动器号（例如 D:），当前为 {}",
            blk.display()
        );
    }
    Ok(format!(r"\\.\{}:", letter.to_ascii_uppercase()))
}

/// 驱动器号（`D:` → `D`）
pub fn drive_letter(blk: &Path) -> Result<char> {
    volume_path(blk)?.chars().nth(4).context("无法解析驱动器号")
}

pub fn volume_root(blk: &Path) -> Result<String> {
    Ok(format!("{}:\\", drive_letter(blk)?))
}

pub fn logical_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u32)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i as u8) as char)
        .collect()
}

pub fn drive_type(letter: char) -> u32 {
    let root = format!("{letter}:\\");
    let w = wide(&root);
    unsafe { GetDriveTypeW(w.as_ptr()) }
}

pub fn is_cdrom(letter: char) -> bool {
    drive_type(letter) == DRIVE_CDROM
}

pub struct Raw {
    pub handle: Handle,
}

impl Raw {
    /// 打开卷。`write = true` 时会请求 `GENERIC_WRITE`，直通命令必须这样打开。
    pub fn open(letter: char, write: bool) -> Result<Self> {
        let path = format!(r"\\.\{}:", letter);
        let access = if write {
            GENERIC_READ | GENERIC_WRITE
        } else {
            GENERIC_READ
        };
        let w = wide(&path);
        let handle = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle as isize == -1 {
            let code = last_error();
            let hint = match (code, write) {
                (5, true) => {
                    "：拒绝访问。设备可能被安全软件限制，或所在目录／进程的完整性级别过低"
                }
                (5, false) => "：拒绝访问",
                (_, true) => "，请检查设备是否已插入",
                _ => "",
            };
            bail!("无法打开 {}（GetLastError={code}）{hint}", path);
        }
        Ok(Self { handle })
    }

    /// 发送一条 SCSI 命令。`data_in = true` 表示设备 → 主机。
    pub fn cmd(&self, cdb: &[u8], data_in: bool, data: &mut [u8], timeout_ms: u32) -> Result<()> {
        if cdb.len() > 16 {
            bail!("CDB 长度 {} 超过 16 字节", cdb.len());
        }
        if data.len() > 64 * 1024 {
            bail!("单次传输 {} 字节超过 Windows 的 64 KiB 限制", data.len());
        }
        let mut packet = Packet {
            spt: SptDirect {
                length: std::mem::size_of::<SptDirect>() as u16,
                cdb_length: cdb.len() as u8,
                sense_info_length: SENSE_LEN as u8,
                data_in: u8::from(data_in),
                data_transfer_length: data.len() as u32,
                time_out_value: timeout_ms / 1000 + 1,
                data_buffer: if data.is_empty() {
                    std::ptr::null_mut()
                } else {
                    data.as_mut_ptr()
                },
                sense_info_offset: std::mem::size_of::<SptDirect>() as u32,
                ..Default::default()
            },
            sense: [0; SENSE_LEN],
        };
        packet.spt.cdb[..cdb.len()].copy_from_slice(cdb);

        let size = std::mem::size_of::<Packet>() as u32;
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                &mut packet as *mut Packet as *mut c_void,
                size,
                &mut packet as *mut Packet as *mut c_void,
                size,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            bail!(
                "SCSI 命令 {:#04x} 失败（DeviceIoControl，GetLastError={}）",
                cdb[0],
                last_error()
            );
        }
        if packet.spt.scsi_status != 0 {
            let mut used = 0usize;
            if packet.sense[0] == 0x70 || packet.sense[0] == 0x71 {
                used = (7 + usize::from(packet.sense[7])).min(SENSE_LEN);
            }
            bail!(
                "SCSI 命令 {:#04x} 失败（status={:#04x}，sense={:02x?}）",
                cdb[0],
                packet.spt.scsi_status,
                &packet.sense[..used]
            );
        }
        Ok(())
    }

    /// 卷控制：`FSCTL_LOCK_VOLUME` / `FSCTL_DISMOUNT_VOLUME` / `FSCTL_UNLOCK_VOLUME`
    pub fn ctl(&self, code: u32) -> Result<()> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            bail!("卷控制 {code:#010x} 失败（GetLastError={}）", last_error());
        }
        Ok(())
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// `STORAGE_DEVICE_DESCRIPTOR` 摘要
#[derive(Debug, Clone, Default)]
pub struct Properties {
    pub vendor: String,
    pub product: String,
    pub serial: String,
    pub bus_type: u32,
    /// `RemovableMedia`，U 盘模式为 true，虚拟光驱为 false
    #[allow(dead_code)]
    pub removable: bool,
}

impl Properties {
    /// IS903 可能是 Innostor / Innostar / FANG 等固件厂商名
    pub fn is_is903(&self) -> bool {
        if self.bus_type != BUS_TYPE_USB {
            return false;
        }
        let vendor = self.vendor.to_ascii_uppercase();
        let product = self.product.to_ascii_uppercase();
        vendor.contains("INNOSTOR")
            || vendor.contains("INNOSTAR")
            || vendor.contains("FANG")
            || product.contains("CD-ROM ON FLASH")
            || product.contains("UFCD")
            || product.contains("NAND FLASH")
    }
}

fn cstr_at(buf: &[u8], off: u32) -> String {
    let off = off as usize;
    if off == 0 || off >= buf.len() {
        return String::new();
    }
    let end = buf[off..]
        .iter()
        .position(|b| *b == 0)
        .map(|p| p + off)
        .unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[off..end]).trim().to_owned()
}

pub fn properties(drive: &Raw) -> Result<Properties> {
    let mut query = QueryProperty {
        property_id: 0, // StorageDeviceProperty
        query_type: 0,  // PropertyStandardQuery
        additional: [0],
    };
    let mut out = vec![0u8; 1024];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            drive.handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &mut query as *mut QueryProperty as *mut c_void,
            std::mem::size_of::<QueryProperty>() as u32,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        bail!("查询设备属性失败（GetLastError={}）", last_error());
    }
    let u32_at = |o: usize| u32::from_le_bytes([out[o], out[o + 1], out[o + 2], out[o + 3]]);
    Ok(Properties {
        vendor: cstr_at(&out, u32_at(12)),
        product: cstr_at(&out, u32_at(16)),
        serial: cstr_at(&out, u32_at(24)),
        bus_type: u32_at(28),
        removable: out[10] != 0,
    })
}

pub fn device_number(drive: &Raw) -> Result<u32> {
    let mut out = [0u8; std::mem::size_of::<DeviceNumber>()];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            drive.handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            std::ptr::null_mut(),
            0,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        bail!("查询设备号失败（GetLastError={}）", last_error());
    }
    Ok(DeviceNumber::from_bytes(&out).device_number)
}

impl DeviceNumber {
    fn from_bytes(bytes: &[u8]) -> Self {
        let u32_at =
            |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        Self {
            device_type: u32_at(0),
            device_number: u32_at(4),
            partition_number: u32_at(8),
        }
    }
}

/// 大容量存储模式的原始磁盘路径 `\\.\PHYSICALDRIVEn`
pub fn physical_drive_path(blk: &Path) -> Result<String> {
    let drive = Raw::open(drive_letter(blk)?, false)
        .with_context(|| format!("无法访问 {}", blk.display()))?;
    Ok(format!(r"\\.\PHYSICALDRIVE{}", device_number(&drive)?))
}

/// 让 Windows 丢弃该光盘卷的缓存，重新读取镜像内容。
pub fn refresh_volume(blk: &Path) -> Result<()> {
    let drive = Raw::open(drive_letter(blk)?, true)
        .with_context(|| format!("无法打开 {}", blk.display()))?;
    drive.ctl(FSCTL_LOCK_VOLUME)?;
    let dismount = drive.ctl(FSCTL_DISMOUNT_VOLUME);
    let _ = drive.ctl(FSCTL_UNLOCK_VOLUME);
    dismount
}

/// IS903 的 USB 序列号作为锁文件名，避免同一设备并发烧录。
pub fn lock_path(serial: &str, fallback: &str) -> std::path::PathBuf {
    let name: String = if serial.trim().is_empty() {
        fallback
    } else {
        serial
    }
    .chars()
    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
    .collect();
    std::env::temp_dir().join(format!("is903burn-{name}.lock"))
}
