//! 烧录与校验:把 ISO 写入块设备,再用 O_DIRECT 回读比对。
//!
//! 为什么要 O_DIRECT:普通读取会命中内核页缓存,读到的可能只是缓存里的
//! 数据,并不能证明「数据真的写进了闪存」。O_DIRECT 绕过全部缓存,读到
//! 的一定是设备上的实际内容。

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const CHUNK: usize = 4 * 1024 * 1024;

/// 丢弃内核为该块设备缓存的页,避免挂载后看到旧内容
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

/// 把 ISO 顺序写入块设备
pub fn write_iso(blk: &Path, iso: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let mut src = File::open(iso).with_context(|| format!("打开 {} 失败", iso.display()))?;
    let total = src.metadata()?.len();

    let mut dst = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_EXCL)
        .open(blk)
        .with_context(|| format!("打开 {} 失败(需要 disk 组权限)", blk.display()))?;

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
        progress(done.min(total), total);
    }

    // 确保数据落到设备
    dst.sync_all().context("fsync 失败")?;
    drop(dst);
    Ok(())
}

/// 用 O_DIRECT 读回设备前 `len` 字节并计算 SHA-256
fn sha256_odirect(blk: &Path, len: u64, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>> {
    let cpath = CString::new(blk.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| anyhow::anyhow!("非法路径"))?;

    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_DIRECT) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("O_DIRECT 打开 {} 失败", blk.display()));
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
        progress(done, len);
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
pub fn verify_iso(blk: &Path, iso: &Path, mut progress: impl FnMut(u64, u64)) -> Result<bool> {
    let len = std::fs::metadata(iso)?.len();
    let a = sha256_file(iso)?;
    // 读之前先丢弃缓存,确保 O_DIRECT 拿到的是真实设备内容
    let _ = drop_cache(blk);
    let b = sha256_odirect(blk, len, &mut progress)?;
    Ok(a == b)
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
