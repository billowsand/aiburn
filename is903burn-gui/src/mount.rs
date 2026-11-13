//! 挂载/卸载。
//!
//! Linux: 走 udisks2(D-Bus),普通用户即可,不必提权。
//! macOS: 走 `diskutil`,同样不需要提权(unmountDisk / mount)。

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
fn run(cmd: &str, args: &[&str]) -> Result<(bool, String)> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("执行 {cmd} 失败: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), s))
}

// --------------------------------------------------------------- Linux ---

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    /// 当前挂载点
    pub fn mount_points(blk: &Path) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(text) = std::fs::read_to_string("/proc/mounts") {
            let b = blk.to_string_lossy();
            for line in text.lines() {
                let mut it = line.split_whitespace();
                if let (Some(src), Some(mp)) = (it.next(), it.next()) {
                    if src == b.as_ref() {
                        v.push(PathBuf::from(mp.replace("\\040", " ")));
                    }
                }
            }
        }
        v
    }

    /// 卸载设备上所有挂载点
    pub fn unmount(blk: &Path) -> Result<()> {
        if !mount_points(blk).is_empty() {
            let device = blk.to_string_lossy();
            let (_, output) = run("udisksctl", &["unmount", "-b", &device])?;
            if !mount_points(blk).is_empty() {
                bail!(
                    "无法卸载 {}。请关闭正在使用光驱或磁盘的窗口,再重试。\n{}",
                    blk.display(),
                    output.trim()
                );
            }
        }
        Ok(())
    }

    /// 挂载设备,返回挂载点
    pub fn mount(blk: &Path) -> Result<PathBuf> {
        let b = blk.to_string_lossy().to_string();
        let (_, out) = run("udisksctl", &["mount", "-b", &b])?;
        if let Some(mp) = mount_points(blk).into_iter().next() {
            return Ok(mp);
        }
        bail!("挂载失败: {}", out.trim())
    }
}

// --------------------------------------------------------------- macOS ---

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    fn diskutil(args: &[&str]) -> Result<(bool, String)> {
        let out = std::process::Command::new("/usr/sbin/diskutil")
            .args(args)
            .output()
            .map_err(|e| anyhow::anyhow!("执行 diskutil 失败: {e}"))?;
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), s))
    }

    /// `diskutil info` 里的挂载点
    pub fn mount_point(blk: &Path) -> Option<PathBuf> {
        let b = blk.to_string_lossy().to_string();
        let (ok, out) = diskutil(&["info", &b]).ok()?;
        if !ok {
            return None;
        }
        for line in out.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Mount Point:") {
                let value = rest.trim();
                if !value.is_empty() && !value.starts_with("Not applicable") {
                    return Some(PathBuf::from(value));
                }
            }
        }
        None
    }

    /// 卸载设备上的所有卷
    pub fn unmount(blk: &Path) -> Result<()> {
        let b = blk.to_string_lossy().to_string();
        let (ok, out) = diskutil(&["unmountDisk", &b])?;
        if !ok {
            bail!(
                "无法卸载 {}。请关闭正在使用该磁盘的窗口,再重试。\n{}",
                blk.display(),
                out.trim()
            );
        }
        Ok(())
    }

    /// 挂载设备,返回挂载点
    pub fn mount(blk: &Path) -> Result<PathBuf> {
        let b = blk.to_string_lossy().to_string();
        let (ok, out) = diskutil(&["mount", &b])?;
        if let Some(mp) = mount_point(blk) {
            return Ok(mp);
        }
        if !ok {
            bail!("挂载失败: {}", out.trim());
        }
        bail!("挂载失败: 没有找到挂载点")
    }
}

// ---------------------------------------------------------- 平台分发 ---

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;

pub fn unmount(blk: &Path) -> Result<()> {
    imp::unmount(blk)
}

pub fn mount(blk: &Path) -> Result<PathBuf> {
    imp::mount(blk)
}
