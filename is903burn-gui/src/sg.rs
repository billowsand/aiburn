//! IS903 厂商写入与光盘视图回读，协议见 PROTOCOL.md。

use anyhow::{bail, ensure, Context, Result};
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::io::Read;
#[cfg(not(target_os = "macos"))]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::Path;

const BLK: usize = crate::iso::SECTOR as usize;
const VENDOR_BLOCK: usize = 512;
const CHUNK: usize = 64 * 1024;

pub struct Sg {
    #[cfg(target_os = "linux")]
    file: File,
    #[cfg(target_os = "macos")]
    usb: Option<std::ptr::NonNull<libc::c_void>>,
    #[cfg(target_os = "windows")]
    raw: crate::winapi::Raw,
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_TO_DEV: i32 = -2;
const SG_DXFER_FROM_DEV: i32 = -3;

impl Sg {
    #[cfg(target_os = "linux")]
    pub fn open(path: &Path, write: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .open(path)
            .with_context(|| format!("无法访问 {}", path.display()))?;
        Ok(Self { file })
    }

    #[cfg(target_os = "linux")]
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
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO, &mut h as *mut SgIoHdr) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).context("SG_IO 失败");
        }
        if h.status != 0 || h.host_status != 0 || h.driver_status != 0 || h.resid != 0 {
            bail!(
                "SCSI 命令失败 (opcode={:#04x}, status={}, host={}, driver={}, resid={}, sense={:02x?})",
                cdb[0],
                h.status,
                h.host_status,
                h.driver_status,
                h.resid,
                &sense[..usize::from(h.sb_len_wr).min(sense.len())]
            );
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn capture(device: &crate::device::Device) -> Result<Self> {
        let session = device.usb_session()?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe { is903_usb_open(session, &mut handle) };
        ensure!(status == 0, "无法接管 IS903 USB 接口（libusb {status}）");
        let drive = Self {
            usb: std::ptr::NonNull::new(handle),
        };
        ensure!(drive.usb.is_some(), "USB 接口为空");
        let mut inquiry = [0; 36];
        drive.cmd(
            &mut [0x12, 0, 0, 0, 36, 0],
            SG_DXFER_FROM_DEV,
            &mut inquiry,
            5000,
        )?;
        ensure!(inquiry[0] & 0x1f == 5, "目标 USB 设备不在 CD-ROM 模式");
        Ok(drive)
    }

    #[cfg(target_os = "macos")]
    pub fn release(mut self) -> Result<()> {
        let status = self.close();
        ensure!(status == 0, "释放 IS903 USB 接口失败（libusb {status}）");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn close(&mut self) -> i32 {
        self.usb
            .take()
            .map_or(0, |handle| unsafe { is903_usb_close(handle.as_ptr()) })
    }

    #[cfg(target_os = "macos")]
    fn cmd(&self, cdb: &mut [u8], dir: i32, buf: &mut [u8], timeout_ms: u32) -> Result<()> {
        let handle = self.usb.context("USB 接口已关闭")?;
        let size = i32::try_from(buf.len())?;
        let transferred = unsafe {
            is903_usb_cmd(
                handle.as_ptr(),
                cdb.as_ptr(),
                cdb.len() as u8,
                buf.as_mut_ptr(),
                size,
                i32::from(dir == SG_DXFER_FROM_DEV),
                timeout_ms,
            )
        };
        ensure!(
            transferred == size,
            "USB SCSI 命令 {:#04x} 失败（传输 {transferred} / {size} 字节）",
            cdb[0]
        );
        Ok(())
    }

    /// Windows：打开卷句柄，直通命令必须用读写方式打开（无需管理员）。
    #[cfg(target_os = "windows")]
    pub fn open(path: &Path, _write: bool) -> Result<Self> {
        let letter = crate::winapi::drive_letter(path)?;
        let raw = crate::winapi::Raw::open(letter, true)
            .with_context(|| format!("无法访问 {}", path.display()))?;
        let drive = Self { raw };
        let mut inquiry = [0u8; 36];
        drive.cmd(
            &mut [0x12, 0, 0, 0, 36, 0],
            SG_DXFER_FROM_DEV,
            &mut inquiry,
            5000,
        )?;
        ensure!(
            matches!(inquiry[0] & 0x1f, 0 | 5),
            "目标设备不是 IS903（SCSI 类型 {}）",
            inquiry[0] & 0x1f
        );
        Ok(drive)
    }

    #[cfg(target_os = "windows")]
    fn cmd(&self, cdb: &mut [u8], dir: i32, buf: &mut [u8], timeout_ms: u32) -> Result<()> {
        self.raw.cmd(cdb, dir == SG_DXFER_FROM_DEV, buf, timeout_ms)
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

    /// 标准 READ(10)。`block` 是设备逻辑块大小（光盘 2048，大容量存储 512）。
    pub fn read10(&self, lba: u32, out: &mut [u8], block: u32) -> Result<()> {
        ensure!(block > 0, "设备逻辑块大小为 0");
        ensure!(
            out.len() % block as usize == 0,
            "读取长度 {} 不是块大小 {block} 的整数倍",
            out.len()
        );
        let n = u16::try_from(out.len() as u32 / block).context("单次读取块数过多")?;
        let mut cdb = [0x28u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&n.to_be_bytes());
        self.cmd(&mut cdb, SG_DXFER_FROM_DEV, out, 30000)
    }

    fn pm(
        &self,
        group: u8,
        op: u8,
        param: u8,
        lba: u32,
        count: u16,
        dir: i32,
        data: &mut [u8],
    ) -> Result<()> {
        let mut cdb = [
            0xfc, group, op, param, 0, 0, 0, 0, 0, 0, 0, 0, b'P', b'.', b'M', b'.',
        ];
        cdb[4..8].copy_from_slice(&lba.to_be_bytes());
        cdb[8..10].copy_from_slice(&count.to_be_bytes());
        self.cmd(&mut cdb, dir, data, 30000)
            .with_context(|| format!("P.M. {op:#04x}，参数 {param:#04x}，LBA {lba}"))
    }

    fn pm_group(&self) -> Result<u8> {
        let mut reply = [0u8; 134];
        let mut cdb = [
            0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, b'I', b'N', b'Q', b'U',
        ];
        self.cmd(&mut cdb, SG_DXFER_FROM_DEV, &mut reply, 10000)
            .context("读取 IS903 命令组")?;
        ensure!(
            reply.windows(5).any(|w| w == b"IS903"),
            "INQU 未识别到 IS903"
        );
        let start = reply
            .windows(4)
            .position(|w| w == b"INQU")
            .context("INQU 响应中缺少命令组列表")?;
        let count = usize::from(reply[5]);
        let groups = reply
            .get(start..start + count * 4)
            .context("INQU 命令组列表长度无效")?;
        groups
            .chunks_exact(4)
            .position(|tag| tag == b"P.M.")
            .map(|index| index as u8)
            .context("设备未提供 P.M. 命令组")
    }

    fn exchange(&self, group: u8, param: u8) -> Result<()> {
        let mut challenge = [0u8; 2];
        self.pm(group, 8, 0, 0, 2, SG_DXFER_FROM_DEV, &mut challenge)?;
        let mut response = key_response(u16::from_be_bytes(challenge));
        self.pm(group, 0xff, param, 0, 12, SG_DXFER_TO_DEV, &mut response)
    }

    pub fn write_iso(
        &self,
        image: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<()> {
        let mut image = File::open(image)?;
        let total = image.metadata()?.len();
        let (last, block_size) = self.capacity().context("读取光驱容量")?;
        ensure!(
            block_size == BLK as u32,
            "光驱扇区大小为 {block_size}，预期为 {BLK}"
        );
        ensure!(total > 0 && total % BLK as u64 == 0, "ISO 长度无效");
        ensure!(
            total <= (u64::from(last) + 1) * u64::from(block_size),
            "ISO 超过光驱容量"
        );
        ensure!(
            total / VENDOR_BLOCK as u64 <= u64::from(u32::MAX),
            "ISO 超过厂商命令寻址范围"
        );

        let group = self.pm_group()?;
        self.exchange(group, 0x80).context("写入前握手")?;
        let mut records = [0u8; 4096];
        self.pm(group, 3, 5, 0, 8, SG_DXFER_FROM_DEV, &mut records)
            .context("读取分区记录")?;
        ensure!(
            records
                .chunks_exact(VENDOR_BLOCK)
                .any(|r| r[0] == 5 || r[..2] == [0, 0]),
            "未找到光驱分区记录或空闲记录"
        );

        let mut chunk = vec![0; CHUNK];
        let mut done = 0;
        while done < total {
            let n = (total - done).min(CHUNK as u64) as usize;
            image.read_exact(&mut chunk[..n])?;
            self.pm(
                group,
                0,
                1,
                (done / VENDOR_BLOCK as u64) as u32,
                (n / VENDOR_BLOCK) as u16,
                SG_DXFER_TO_DEV,
                &mut chunk[..n],
            )
            .context("写入光驱镜像")?;
            done += n as u64;
            if !progress(done, total) {
                bail!("已取消烧录");
            }
        }
        self.exchange(group, 0x81).context("写入后握手")
    }

    /// 按字节偏移读取，自动按设备逻辑块大小换算 LBA。
    ///
    /// macOS 需要先接管 USB 接口才能直通，暂不提供导出功能， macOS 上不编译。
    #[cfg(not(target_os = "macos"))]
    fn read_at(&self, offset: u64, out: &mut [u8], block: u32) -> Result<()> {
        ensure!(
            out.len() % block as usize == 0,
            "读取长度 {} 不是块大小 {block} 的整数倍",
            out.len()
        );
        let lba = u32::try_from(offset / u64::from(block))?;
        self.read10(lba, out, block)
    }

    /// 整盘导出（只读）：从 PVD 读卷空间大小，再逐块倒出
    #[cfg(not(target_os = "macos"))]
    pub fn dump_iso(&self, out: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<u64> {
        let (_, block) = self.capacity().context("读取设备块大小")?;
        ensure!(block > 0, "设备逻辑块大小为 0");
        let mut pvd = vec![0u8; BLK];
        self.read_at(u64::try_from(16 * BLK)?, &mut pvd, block)
            .context("读取主卷描述符")?;
        ensure!(&pvd[1..6] == b"CD001", "LBA 16 不是 ISO 9660 卷描述符");
        let sectors = u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]);
        ensure!(sectors > 0, "卷空间大小为 0");
        let total = u64::from(sectors) * BLK as u64;
        let mut file = File::create(out).with_context(|| format!("创建 {} 失败", out.display()))?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        while done < total {
            let n = (total - done).min(CHUNK as u64) as usize;
            self.read_at(done, &mut buf[..n], block)
                .with_context(|| format!("读取偏移 {done} 失败"))?;
            file.write_all(&buf[..n])?;
            done += n as u64;
            if !progress(done, total) {
                bail!("已取消导出");
            }
        }
        file.sync_all().context("写入输出文件失败")?;
        Ok(done)
    }

    pub fn verify_iso(
        &self,
        image: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<()> {
        let mut image = File::open(image)?;
        let total = image.metadata()?.len();
        let (_, block) = self.capacity().context("读取设备块大小")?;
        ensure!(block > 0, "设备逻辑块大小为 0");
        ensure!(total > 0 && total % u64::from(block) == 0, "ISO 长度无效");
        let mut expected = vec![0; CHUNK];
        let mut actual = vec![0; CHUNK];
        let mut done = 0;
        while done < total {
            let n = (total - done).min(CHUNK as u64) as usize;
            let lba = u32::try_from(done / u64::from(block))?;
            image.read_exact(&mut expected[..n])?;
            self.read10(lba, &mut actual[..n], block)
                .with_context(|| format!("READ(10) 回读 LBA {lba}"))?;
            if let Some(offset) = actual[..n]
                .iter()
                .zip(&expected[..n])
                .position(|(a, b)| a != b)
            {
                bail!("整盘回读不一致，首个差异位于字节 {}", done + offset as u64);
            }
            done += n as u64;
            if !progress(done, total) {
                bail!("已取消烧录");
            }
        }
        Ok(())
    }
}

fn key_response(challenge: u16) -> [u8; 12] {
    let mut response = *b"Vm3RuP42j/ \0";
    let mut state = challenge.reverse_bits() >> 1;
    for byte in &mut response {
        let mut mask = 0;
        for _ in 0..8 {
            let bit = (state ^ (state >> 1)) & 1;
            mask = (mask >> 1) | ((bit as u8) << 7);
            state = (state >> 1) | (bit << 14);
        }
        *byte ^= mask;
    }
    response
}

#[cfg(target_os = "macos")]
impl Drop for Sg {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn is903_usb_open(session: libc::c_ulong, output: *mut *mut libc::c_void) -> i32;
    fn is903_usb_close(handle: *mut libc::c_void) -> i32;
    fn is903_usb_cmd(
        handle: *mut libc::c_void,
        cdb: *const u8,
        cdb_size: u8,
        data: *mut u8,
        size: i32,
        data_in: i32,
        timeout: u32,
    ) -> i32;
}
