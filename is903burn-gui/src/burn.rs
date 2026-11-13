//! 烧录与校验:把 ISO 写入块设备,再回读比对。
//!
//! 为什么要绕过缓存:普通读取会命中系统缓存,读到的可能只是缓存里的
//! 数据,并不能证明「数据真的写进了闪存」。Linux 用 `O_DIRECT`、macOS 用
//! `F_NOCACHE`、Windows 用 SCSI 直通 `READ(10)`(完全不经过文件系统),
//! 读到的都是设备上的实际内容。

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "windows"))]
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
use std::path::Path;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;

pub const CHUNK: usize = 4 * 1024 * 1024;

/// 丢弃系统为该块设备缓存的页,避免挂载后看到旧内容
///
/// Linux 用 `posix_fadvise(POSIX_FADV_DONTNEED)`;macOS 用 `F_NOCACHE`,
/// Windows 回读走 SCSI 直通;这两种情况这里退化为空操作。
#[cfg(target_os = "linux")]
pub fn drop_cache(path: &Path) -> Result<()> {
    let f = File::open(path).with_context(|| format!("打开 {} 失败", path.display()))?;
    let rc = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        bail!(
            "posix_fadvise 失败: {}",
            std::io::Error::from_raw_os_error(rc)
        );
    }
    Ok(())
}

#[cfg(all(not(target_os = "linux"), not(target_os = "windows")))]
pub fn drop_cache(_path: &Path) -> Result<()> {
    Ok(())
}

/// 原始设备节点:macOS 上 /dev/diskN 的原始设备是 /dev/rdiskN,
/// 走原始设备可以绕开块层缓存,写入与回读都更可靠。
#[cfg(target_os = "macos")]
fn raw_node(blk: &Path) -> PathBuf {
    let name = blk.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some(num) = name.strip_prefix("disk") {
        let raw = PathBuf::from("/dev").join(format!("rdisk{num}"));
        if raw.exists() {
            return raw;
        }
    }
    blk.to_path_buf()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn raw_node(blk: &Path) -> PathBuf {
    blk.to_path_buf()
}

/// 把 ISO 顺序写入块设备
///
/// `progress` 返回 `false` 表示取消，此时镜像可能只写了一半。
#[cfg(not(target_os = "windows"))]
pub fn write_iso(blk: &Path, iso: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<()> {
    let mut src = File::open(iso).with_context(|| format!("打开 {} 失败", iso.display()))?;
    let total = src.metadata()?.len();

    let target = raw_node(blk);
    let mut dst = OpenOptions::new()
        .write(true)
        .custom_flags(exclusive_flag())
        .open(&target)
        .with_context(|| format!("打开 {} 失败(需要 disk 组权限)", target.display()))?;

    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // 末块补齐到 512 字节
        let pad = (512 - n % 512) % 512;
        if pad > 0 {
            buf[n..n + pad].fill(0);
        }
        dst.write_all(&buf[..n + pad])
            .with_context(|| format!("写入 {} 失败", blk.display()))?;
        done += n as u64;
        if !progress(done.min(total), total) {
            bail!("已取消烧录");
        }
    }

    // 确保数据落到设备
    dst.sync_all().context("fsync 失败")?;
    drop(dst);
    Ok(())
}

/// Windows 大容量存储模式:ISO 原样写到物理磁盘偏移 0。
///
/// 与 Linux 的块设备写入等价,但 `\\.\PHYSICALDRIVEn` 的写句柄需要管理员
/// 权限,且该盘上的卷必须已经卸载;CD-ROM 模式不走这里。
#[cfg(target_os = "windows")]
pub fn write_iso(blk: &Path, iso: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<()> {
    let mut src = File::open(iso).with_context(|| format!("打开 {} 失败", iso.display()))?;
    let total = src.metadata()?.len();
    let target =
        crate::winapi::physical_drive_path(blk).context("无法定位 IS903 对应的物理磁盘")?;
    let mut dst = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target)
        .with_context(|| {
            format!(
                "打开 {target} 失败（大容量存储模式的原始写入需要管理员权限，\
                 且需先卸载该盘的所有卷）"
            )
        })?;

    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // 末块补齐到 512 字节
        let pad = (512 - n % 512) % 512;
        if pad > 0 {
            buf[n..n + pad].fill(0);
        }
        dst.write_all(&buf[..n + pad])
            .with_context(|| format!("写入 {target} 失败"))?;
        done += n as u64;
        if !progress(done.min(total), total) {
            bail!("已取消烧录");
        }
    }

    dst.sync_all().context("FlushFileBuffers 失败")?;
    drop(dst);
    Ok(())
}

/// 独占打开标志:只有 Linux 需要 O_EXCL
#[cfg(target_os = "linux")]
#[inline]
fn exclusive_flag() -> libc::c_int {
    libc::O_EXCL
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[inline]
fn exclusive_flag() -> libc::c_int {
    0
}

/// 直读标志:Linux 用 O_DIRECT,macOS 没有 O_DIRECT,改用 F_NOCACHE
#[cfg(target_os = "linux")]
#[inline]
fn direct_flag() -> libc::c_int {
    libc::O_DIRECT
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[inline]
fn direct_flag() -> libc::c_int {
    0
}

/// 用 O_DIRECT 读回设备前 `len` 字节并计算 SHA-256
#[cfg(not(target_os = "windows"))]
fn sha256_odirect(
    blk: &Path,
    len: u64,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<Vec<u8>> {
    let blk = &raw_node(blk);
    let cpath = CString::new(blk.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| anyhow::anyhow!("非法路径"))?;

    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | direct_flag()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("直读打开 {} 失败", blk.display()));
    }
    // macOS 用 F_NOCACHE 让这个 fd 绕过统一缓冲区缓存
    #[cfg(not(target_os = "linux"))]
    unsafe {
        libc::fcntl(fd, libc::F_NOCACHE, 1);
    }

    // 对齐缓冲区(4096 对齐,O_DIRECT 要求)
    const ALIGN: usize = 4096;
    let cap = CHUNK + ALIGN;
    let mut raw: *mut libc::c_void = std::ptr::null_mut();
    let rc = unsafe { libc::posix_memalign(&mut raw, ALIGN, cap) };
    if rc != 0 {
        unsafe { libc::close(fd) };
        bail!("分配对齐缓冲区失败");
    }

    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut result = Ok(());

    // O_DIRECT 要求读写长度与缓冲区都按设备块大小对齐。
    // 每次只读「本块所需长度向上取整到 ALIGN」,并且只把真实长度计入哈希,
    // 否则多读的字节会让后续偏移错位。
    while done < len {
        let remain = (len - done) as usize;
        let want = remain.min(CHUNK); // 真正要计入哈希的字节数
        let read_len = (want + ALIGN - 1) / ALIGN * ALIGN; // 对齐后的读取长度

        let mut got = 0usize;
        while got < read_len {
            let n = unsafe {
                libc::read(
                    fd,
                    (raw as *mut u8).add(got) as *mut libc::c_void,
                    read_len - got,
                )
            };
            if n < 0 {
                result = Err(std::io::Error::last_os_error()).context("O_DIRECT 读取失败");
                break;
            }
            if n == 0 {
                result = Err(anyhow::anyhow!(
                    "设备提前结束(读到 {} / {} 字节)",
                    done + got as u64,
                    len
                ));
                break;
            }
            got += n as usize;
        }
        if result.is_err() {
            break;
        }

        let slice = unsafe { std::slice::from_raw_parts(raw as *const u8, want) };
        hasher.update(slice);
        done += want as u64;
        if !progress(done, len) {
            result = Err(anyhow::anyhow!("已取消烧录"));
            break;
        }
    }

    unsafe {
        libc::free(raw);
        libc::close(fd);
    }
    result?;
    Ok(hasher.finalize().to_vec())
}

fn sha256_file(p: &Path) -> Result<Vec<u8>> {
    let mut f = File::open(p)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// 校验:设备前 `len` 字节是否与 ISO 完全一致
#[cfg(not(target_os = "windows"))]
pub fn verify_iso(
    blk: &Path,
    iso: &Path,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<bool> {
    let len = std::fs::metadata(iso)?.len();
    let a = sha256_file(iso)?;
    // 读之前先丢弃缓存,确保 O_DIRECT 拿到的是真实设备内容
    let _ = drop_cache(blk);
    let b = sha256_odirect(blk, len, &mut progress)?;
    Ok(a == b)
}

/// Windows:用 SCSI 直通 `READ(10)` 逐字节回读。
///
/// 直通不经过文件系统缓存,因此光盘模式和大容量存储模式都能用;
/// 块大小由设备自己报告(光盘 2048,大容量存储 512)。
#[cfg(target_os = "windows")]
pub fn verify_iso(blk: &Path, iso: &Path, progress: impl FnMut(u64, u64) -> bool) -> Result<bool> {
    let drive = crate::sg::Sg::open(blk, false)
        .with_context(|| format!("无法打开 {} 进行回读校验", blk.display()))?;
    drive.verify_iso(iso, progress)?;
    Ok(true)
}

/// 递归收集一组路径下所有文件的 (大小, SHA-256)。
///
/// 用来做「内容级」核对:不比较文件名,只比较每份内容的哈希多重集合。
/// 这样即使 Joliet 截断了长文件名,也不会误判。
pub fn hash_tree(paths: &[std::path::PathBuf]) -> Result<Vec<(u64, String)>> {
    let mut out = Vec::new();
    for p in paths {
        hash_walk(p, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn hash_walk(p: &Path, out: &mut Vec<(u64, String)>) -> Result<()> {
    let meta = std::fs::symlink_metadata(p).with_context(|| format!("无法读取 {}", p.display()))?;
    if meta.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(p)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            hash_walk(&e.path(), out)?;
        }
    } else if meta.is_file() {
        let h = sha256_file(p)?;
        out.push((meta.len(), hex(&h)));
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
