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

    #[allow(dead_code)]
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
        // 对用户只暴露节点和容量;模式细节由后台处理
        format!("{}{}", self.node.display(), cap)
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

/// 让设备重新枚举:对 usb-storage 接口做 unbind/bind。
///
/// 比取消 USB 授权(authorized 0/1)温和,不会让设备彻底掉线。
/// 需要 root;无权限时自动走 pkexec。
///
/// 返回重新枚举后找到的设备(可能换了节点名,例如 /dev/sda → /dev/sr0)。
pub fn reenumerate(block_name: &str, timeout: std::time::Duration) -> Result<Device, String> {
    let iface = usb_interface_of(block_name)?;

    // 先试直接写;不行再 pkexec
    let direct = std::fs::write(
        format!("/sys/bus/usb/drivers/usb-storage/unbind"),
        &iface,
    )
    .is_ok();
    if direct {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = std::fs::write(
            format!("/sys/bus/usb/drivers/usb-storage/bind"),
            &iface,
        );
    } else {
        let script = format!(
            "echo -n '{iface}' > /sys/bus/usb/drivers/usb-storage/unbind; \
             sleep 0.4; \
             echo -n '{iface}' > /sys/bus/usb/drivers/usb-storage/bind"
        );
        let st = std::process::Command::new("pkexec")
            .args(["sh", "-c", &script])
            .status()
            .map_err(|e| format!("无法调用 pkexec: {e}"))?;
        if !st.success() {
            return Err("重新枚举需要 root 权限,授权被取消".into());
        }
    }

    // 等待设备重新出现
    let deadline = std::time::Instant::now() + timeout;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(d) = scan().into_iter().next() {
            return Ok(d);
        }
        if std::time::Instant::now() >= deadline {
            return Err("重新枚举后设备未在规定时间内出现".into());
        }
    }
}

/// 找到块设备对应的 usb-storage 接口名,例如 "2-1:1.0"
fn usb_interface_of(block_name: &str) -> Result<String, String> {
    let real = std::fs::canonicalize(format!("/sys/block/{block_name}"))
        .map_err(|e| format!("无法解析 /sys/block/{block_name}: {e}"))?;
    let mut cur = real.as_path();
    while let Some(parent) = cur.parent() {
        if let Some(n) = cur.file_name().and_then(|s| s.to_str()) {
            // 接口目录形如 "2-1:1.0"
            if n.contains('-') && n.contains(':') {
                return Ok(n.to_string());
            }
        }
        cur = parent;
    }
    Err("找不到对应的 usb-storage 接口".into())
}

/// 找到块设备对应的 /dev/sgN 节点
pub fn sg_node_of(block_name: &str) -> Option<String> {
    let target = std::fs::canonicalize(format!("/sys/block/{block_name}/device")).ok()?;
    let dir = std::fs::read_dir("/sys/class/scsi_generic").ok()?;
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Ok(p) = std::fs::canonicalize(e.path().join("device")) {
            if p == target {
                return Some(format!("/dev/{name}"));
            }
        }
    }
    None
}
