//! 从 sysfs 识别 IS903，并在重新枚举后定位同一块设备。

use anyhow::{ensure, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub node: PathBuf,
    pub vendor: String,
    pub model: String,
    pub scsi_type: u8,
    pub capacity: u64,
    pub sg_node: Option<PathBuf>,
    usb: PathBuf,
    interface: PathBuf,
    serial: String,
    devnum: String,
    lun: String,
}

impl Device {
    pub fn is_writable(&self) -> bool {
        self.scsi_type == 0
    }

    pub fn title(&self) -> String {
        let mode = if self.is_writable() {
            "大容量存储"
        } else {
            "CD-ROM"
        };
        format!(
            "{}  ·  {mode}  ·  {}",
            self.node.display(),
            human_size(self.capacity)
        )
    }

    pub fn same_device(&self, other: &Self) -> bool {
        self.interface == other.interface && self.serial == other.serial && self.lun == other.lun
    }

    pub fn same_connection(&self, other: &Self) -> bool {
        self.same_device(other) && self.devnum == other.devnum
    }

    pub fn current(&self) -> Result<Self> {
        scan()
            .into_iter()
            .find(|d| {
                d.same_connection(self) && d.node == self.node && d.scsi_type == self.scsi_type
            })
            .context("目标设备已拔出或设备节点已变更，请重新选择")
    }

    pub fn check_access(&self) -> Result<()> {
        ensure!(self.capacity > 0, "设备容量为 0，请重新插入设备");
        let path = if self.is_writable() {
            &self.node
        } else {
            self.sg_node
                .as_ref()
                .context("缺少 SCSI 通用节点，请在终端运行 sudo modprobe sg")?
        };
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("无法访问 {}，请检查设备权限", path.display()))?;
        Ok(())
    }

    pub fn lock_file(&self) -> Result<fs::File> {
        fs::File::open(&self.usb).context("无法打开目标 USB 设备目录")
    }
}

pub fn human_size(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut i = 0;
    while value >= 1024.0 && i < units.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[i])
    }
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn inspect(block: &Path) -> Option<Device> {
    let name = block.file_name()?.to_str()?;
    if !(name.starts_with("sd") || name.starts_with("sr")) {
        return None;
    }
    let target = fs::canonicalize(block.join("device")).ok()?;
    let usb = target.ancestors().find(|p| p.join("idVendor").exists())?;
    let interface = target
        .ancestors()
        .find(|p| p.join("bInterfaceNumber").exists())?;
    let vendor = read_trim(&target.join("vendor")).unwrap_or_default();
    let exact_id = read_trim(&usb.join("idVendor")).as_deref() == Some("1f75")
        && read_trim(&usb.join("idProduct")).as_deref() == Some("0903");
    if !(exact_id || vendor.contains("Innostor") || vendor.contains("Innostar")) {
        return None;
    }
    let scsi_type = read_trim(&target.join("type"))?.parse().ok()?;
    if !matches!(scsi_type, 0 | 5) {
        return None;
    }
    let sectors = read_trim(&block.join("size"))?.parse::<u64>().ok()?;
    let sg_node = fs::read_dir(target.join("scsi_generic"))
        .ok()
        .and_then(|mut entries| entries.next()?.ok())
        .map(|entry| Path::new("/dev").join(entry.file_name()));
    Some(Device {
        node: Path::new("/dev").join(name),
        vendor,
        model: read_trim(&target.join("model")).unwrap_or_default(),
        scsi_type,
        capacity: sectors.saturating_mul(512),
        sg_node,
        usb: usb.to_owned(),
        interface: interface.to_owned(),
        serial: read_trim(&usb.join("serial")).unwrap_or_default(),
        devnum: read_trim(&usb.join("devnum"))?,
        lun: target.file_name()?.to_str()?.rsplit(':').next()?.to_owned(),
    })
}

pub fn scan() -> Vec<Device> {
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return Vec::new();
    };
    let mut devices: Vec<_> = entries
        .flatten()
        .filter_map(|entry| inspect(&entry.path()))
        .collect();
    devices.sort_by(|a, b| (a.scsi_type, &a.node).cmp(&(b.scsi_type, &b.node)));
    devices
}

pub fn reenumerate(device: &Device, timeout: Duration) -> Result<Device> {
    let device = device.current()?;
    let driver = Path::new("/sys/bus/usb/drivers/usb-storage");
    let interface = device
        .interface
        .file_name()
        .context("找不到目标 USB 接口")?;
    ensure!(
        fs::canonicalize(driver.join(interface)).ok().as_ref() == Some(&device.interface),
        "目标设备未绑定 usb-storage 驱动"
    );

    // Recheck the same physical USB connection after polkit authentication.
    let script = r#"
        set -eu
        driver=/sys/bus/usb/drivers/usb-storage
        test "$(readlink -f "$driver/$1")" = "$2"
        test "$(cat "$3/devnum")" = "$4"
        printf '%s' "$1" > "$driver/unbind"
        sleep 0.4
        printf '%s' "$1" > "$driver/bind"
    "#;
    let mut command = if unsafe { libc::geteuid() } == 0 {
        std::process::Command::new("/bin/sh")
    } else {
        let mut command = std::process::Command::new("pkexec");
        command.arg("/bin/sh");
        command
    };
    let status = command
        .args(["-c", script, "is903-rebind"])
        .arg(interface)
        .arg(&device.interface)
        .arg(&device.usb)
        .arg(&device.devnum)
        .status()
        .context("无法重新绑定 USB 存储接口")?;
    ensure!(
        status.success(),
        "USB 接口重新绑定失败 ({status})，请检查系统授权和设备连接"
    );

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = scan()
            .into_iter()
            .find(|d| d.same_connection(&device) && d.node.exists())
        {
            return Ok(found);
        }
        ensure!(Instant::now() < deadline, "重新枚举后找不到目标 IS903");
        std::thread::sleep(Duration::from_millis(250));
    }
}
