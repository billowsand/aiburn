//! 通过 SG_IO 直接对光驱类设备读写。
//!
//! IS903 的 `WRITE(12)` / `READ(12)` 用的是**非标准 CDB 布局**:
//! LBA 在字节 2..5,传输块数在**字节 10**(单字节,1..255),
//! 而不是标准 WRITE(12) 的字节 6..9。用标准布局会被回以
//! `INVALID FIELD IN CDB`。

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::path::Path;

pub const BLK: usize = 2048;

pub struct Sg {
    fd: libc::c_int,
}

#[repr(C)]
#[derive(Default)]
struct SgIoHdr {
    interface_id: libc::c_int,
    dxfer_direction: libc::c_int,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut libc::c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut libc::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_TO_DEV: libc::c_int = -2;
const SG_DXFER_FROM_DEV: libc::c_int = -3;

impl Sg {
    pub fn open(path: &Path) -> Result<Self> {
        let c = CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| anyhow::anyhow!("非法路径"))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("打开 {} 失败", path.display()));
        }
        Ok(Self { fd })
    }

    fn cmd(&self, cdb: &mut [u8], dir: i32, buf: &mut [u8], timeout_ms: u32) -> Result<()> {
        let mut sense = [0u8; 32];
        let mut h = SgIoHdr {
            interface_id: 0x53, // 'S'
            dxfer_direction: dir,
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            dxfer_len: buf.len() as u32,
            dxferp: if buf.is_empty() {
                std::ptr::null_mut()
            } else {
                buf.as_mut_ptr() as *mut libc::c_void
            },
            cmdp: cdb.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout_ms,
            ..Default::default()
        };
        let rc = unsafe { libc::ioctl(self.fd, SG_IO, &mut h as *mut SgIoHdr) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).context("SG_IO 失败");
        }
        if h.masked_status != 0 || h.host_status != 0 || h.driver_status != 0 {
            bail!(
                "SCSI 命令被拒:status={} host={} driver={} sense_key={:#04x} asc={:#04x}",
                h.masked_status,
                h.host_status,
                h.driver_status,
                sense[2],
                sense[12]
            );
        }
        Ok(())
    }

    /// 读容量(块大小 / 最后一块)
    pub fn capacity(&self) -> Result<(u32, u32)> {
        let mut cdb = [0x25u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut buf = [0u8; 8];
        self.cmd(&mut cdb, SG_DXFER_FROM_DEV, &mut buf, 10000)?;
        let last = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let bs = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        Ok((last, bs))
    }

    /// 标准 READ(10),读的是「光盘」可见内容
    pub fn read10(&self, lba: u32, out: &mut [u8]) -> Result<()> {
        let n = (out.len() / BLK) as u16;
        let mut cdb = [0x28u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&n.to_be_bytes());
        self.cmd(&mut cdb, SG_DXFER_FROM_DEV, out, 30000)
    }

    /// IS903 自定义 WRITE(12):块数在 CDB[10]
    pub fn write12(&self, lba: u32, data: &mut [u8]) -> Result<()> {
        let n = (data.len() / BLK) as u32;
        if n == 0 || n > 255 {
            bail!("单次写入块数必须在 1..255 之间(实际 {n})");
        }
        let mut cdb = [0u8; 12];
        cdb[0] = 0xaa;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[10] = n as u8;
        self.cmd(&mut cdb, SG_DXFER_TO_DEV, data, 60000)
    }

    /// IS903 自定义 READ(12):用于核对写入缓冲区
    #[allow(dead_code)]
    pub fn read12(&self, lba: u32, out: &mut [u8]) -> Result<()> {
        let n = (out.len() / BLK) as u32;
        let mut cdb = [0u8; 12];
        cdb[0] = 0xa8;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[10] = n as u8;
        self.cmd(&mut cdb, SG_DXFER_FROM_DEV, out, 30000)
    }

    /// 直接下发一条 6/10/16 字节的原始命令(调试用)
    #[allow(dead_code)]
    pub fn raw(&self, cdb: &mut [u8], dir: i32, buf: &mut [u8]) -> Result<()> {
        self.cmd(cdb, dir, buf, 20000)
    }
}

impl Drop for Sg {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

