//! IS903 设备检测:扫描 /sys/block,识别厂商为 Innostor 的 SCSI 设备。
//!
//! 设备有两种模式:
//!   type 5 (CD-ROM)      → /dev/srN,只读
//!   type 0 (大容量存储)  → /dev/sdN,可写(烧录用这个)

use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Device {
    /// 块设备节点,例如 /dev/sda、/dev/sr0
    pub node: PathBuf,
    pub name: String,
    pub vendor: String,
    pub model: String,
    pub scsi_type: u8,
    /// 容量(字节);未知时为 0
    pub capacity: u64,
}

impl Device {
    pub fn is_writable(&self) -> bool {
        self.scsi_type == 0
    }

    pub fn status_text(&self) -> String {
        if self.is_writable() {
            "可烧录".to_string()
        } else if self.scsi_type == 5 {
            "只读(CD-ROM 模式)".to_string()
        } else {
            format!("不支持(类型 {})", self.scsi_type)
        }
    }

    pub fn title(&self) -> String {
        let cap = if self.capacity > 0 {
            format!(", {}", human_size(self.capacity))
        } else {
            String::new()
        };
        format!("{}  ·  {}{}", self.node.display(), self.status_text(), cap)
    }
}

/// 人类可读的容量
pub fn human_size(bytes: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", U[i])
    }
}

fn read_trim(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// 扫描所有块设备,返回 IS903 设备列表
pub fn scan() -> Vec<Device> {
    let mut out = Vec::new();
    let dir = match fs::read_dir("/sys/block") {
        Ok(d) => d,
        Err(_) => return out,
    };

    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("sd") || name.starts_with("sr")) {
            continue;
        }
        let base = format!("/sys/block/{name}");
        let dev = format!("{base}/device");

        let vendor = read_trim(&format!("{dev}/vendor")).unwrap_or_default();
        // 设备实际报告的是 "Innostor";兼容几种拼写
        if !(vendor.contains("Innosto") || vendor.contains("Innostar")) {
            continue;
        }
        let model = read_trim(&format!("{dev}/model")).unwrap_or_default();
        let scsi_type: u8 = read_trim(&format!("{dev}/type"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(255);

        let sectors: u64 = read_trim(&format!("{base}/size"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let capacity = sectors.saturating_mul(512);

        out.push(Device {
            node: PathBuf::from(format!("/dev/{name}")),
            name,
            vendor,
            model,
            scsi_type,
            capacity,
        });
    }

    // sd* 排在前面(可烧录的优先)
    out.sort_by_key(|d| (d.scsi_type != 0, d.name.clone()));
    out
}

/// 软件复位 USB 设备,替代物理拔插。
#[allow(dead_code)]
///
/// 通过 /sys/bus/usb/devices/<id>/authorized 触发重新枚举。
/// 需要写权限(通常要 root);失败时返回错误说明。
pub fn usb_soft_reset(block_name: &str) -> Result<String, String> {
    // /sys/block/sda → 找到对应的 USB 设备目录
    let link = format!("/sys/block/{block_name}");
    let real = fs::canonicalize(&link).map_err(|e| format!("无法解析 {link}: {e}"))?;

    // 向上找形如 "2-1" 的 USB 设备目录
    let mut cur = real.as_path();
    let mut usb_id = None;
    while let Some(parent) = cur.parent() {
        if let Some(n) = cur.file_name().and_then(|s| s.to_str()) {
            if n.contains('-') && n.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                if cur.join("authorized").exists() {
                    usb_id = Some(n.to_string());
                    break;
                }
            }
        }
        cur = parent;
    }
    let id = usb_id.ok_or_else(|| "找不到对应的 USB 设备节点".to_string())?;

    let auth = format!("/sys/bus/usb/devices/{id}/authorized");
    fs::write(&auth, "0").map_err(|e| format!("写 {auth} 失败(需要 root): {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    fs::write(&auth, "1").map_err(|e| format!("写 {auth} 失败(需要 root): {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(1200));
    Ok(id)
}
