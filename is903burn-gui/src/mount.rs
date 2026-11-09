//! 挂载/卸载。走 udisks2(D-Bus),普通用户即可,不必提权。

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("执行 {cmd} 失败: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), s))
}

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
    for mp in mount_points(blk) {
        let mp_s = mp.to_string_lossy().to_string();
        let b = blk.to_string_lossy().to_string();
        if run("udisksctl", &["unmount", "-b", &b]).map(|r| r.0).unwrap_or(false) {
            continue;
        }
        if run("umount", &[&mp_s]).map(|r| r.0).unwrap_or(false) {
            continue;
        }
        // 懒卸载作为最后手段
        let _ = run("umount", &["-l", &mp_s]);
    }
    if !mount_points(blk).is_empty() {
        bail!(
            "卸载失败,仍挂载在: {}",
            mount_points(blk)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// 挂载设备,返回挂载点
pub fn mount(blk: &Path) -> Result<PathBuf> {
    let b = blk.to_string_lossy().to_string();
    let (ok, out) = run("udisksctl", &["mount", "-b", &b])?;
    if ok {
        if let Some(pos) = out.find(" at ") {
            let p = out[pos + 4..].trim().to_string();
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    // 回退:直接看内核挂载表(有时 udisks 报 "already mounted")
    if let Some(mp) = mount_points(blk).into_iter().next() {
        return Ok(mp);
    }
    bail!("挂载失败: {}", out.trim())
}
