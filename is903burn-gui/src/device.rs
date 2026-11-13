//! 识别 IS903 设备,并在重新枚举后定位同一块设备。
//!
//! Linux: 通过 sysfs 扫描 `/sys/block`,SCSI 通用节点来自 `scsi_generic`。
//! macOS: 通过 IOKit(`ioreg`)找到 USB 设备 `1f75:0903`,再顺着设备树
//!         找到对应的 `/dev/diskN`;块设备写入与回读都在 `/dev/diskN` 上完成。
//!         CD-ROM 模式通过 libusb 接管 USB BOT 接口。

use anyhow::{ensure, Context, Result};
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
            .context("目标设备已拔出或设备节点已变更,请重新选择")
    }

    pub fn check_access(&self) -> Result<()> {
        ensure!(self.capacity > 0, "设备容量为 0,请重新插入设备");
        #[cfg(target_os = "macos")]
        if !self.is_writable() {
            self.usb_session()?;
            ensure!(
                unsafe { libc::geteuid() } == 0,
                "macOS 光驱烧录需要管理员权限，请在终端使用 sudo 运行 --burn"
            );
            return Ok(());
        }
        let path = if self.is_writable() {
            &self.node
        } else {
            self.sg_node
                .as_ref()
                .context("缺少 SCSI 通用节点,请在 Linux 上加载 sg 模块")?
        };
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("无法访问 {},请检查设备权限", path.display()))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn usb_session(&self) -> Result<libc::c_ulong> {
        let session = self
            .devnum
            .parse()
            .context("缺少目标设备的 USB sessionID")?;
        ensure!(session != 0, "USB sessionID 无效");
        Ok(session)
    }

    pub fn lock_file(&self) -> Result<std::fs::File> {
        lock_file(self)
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

// ---------------------------------------------------------------- Linux ---

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;

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

    pub fn lock_file(dev: &Device) -> Result<fs::File> {
        fs::File::open(&dev.usb).context("无法打开目标 USB 设备目录")
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
            "USB 接口重新绑定失败 ({status}),请检查系统授权和设备连接"
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
}

// --------------------------------------------------------------- macOS ---

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::process::Command;

    const VID: u64 = 0x1f75;
    const PID: u64 = 0x0903;

    /// IORegistry 里的一个节点
    struct Node {
        class: String,
        depth: usize,
        props: HashMap<String, String>,
    }

    impl Node {
        fn int(&self, key: &str) -> Option<u64> {
            self.props.get(key).and_then(|v| {
                let v = v.trim();
                if let Some(hex) = v.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16).ok()
                } else {
                    v.parse::<u64>().ok()
                }
            })
        }

        fn text(&self, key: &str) -> Option<String> {
            self.props.get(key).cloned()
        }
    }

    /// 解析 `ioreg -p IOService -l` 的树形输出
    fn ioreg_nodes() -> Vec<Node> {
        let Ok(out) = Command::new("/usr/sbin/ioreg")
            .args(["-p", "IOService", "-l"])
            .output()
        else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut nodes: Vec<Node> = Vec::new();
        let mut current: Option<usize> = None;
        for line in text.lines() {
            let trimmed = line.trim_start_matches([' ', '|']);
            let indent = line.len() - trimmed.len();
            if let Some(rest) = trimmed.strip_prefix("+-o ") {
                let class = rest
                    .split_once("<class ")
                    .map(|(_, tail)| tail.split(',').next().unwrap_or("").trim().to_owned())
                    .unwrap_or_default();
                nodes.push(Node {
                    class,
                    depth: indent / 2,
                    props: HashMap::new(),
                });
                current = Some(nodes.len() - 1);
                continue;
            }
            let Some(index) = current else { continue };
            let Some((key, value)) = parse_property(trimmed) else {
                continue;
            };
            nodes[index].props.insert(key, value);
        }
        nodes
    }

    /// 解析 `"key" = value`
    fn parse_property(line: &str) -> Option<(String, String)> {
        let rest = line.strip_prefix('"')?;
        let (key, rest) = rest.split_once('"')?;
        let (_, rest) = rest.split_once('=')?;
        let value = rest.trim();
        let value = if let Some(v) = value.strip_prefix('"') {
            v.split_once('"').map(|(v, _)| v).unwrap_or(v)
        } else if value.starts_with('<') || value.starts_with('{') {
            return None;
        } else {
            value
        };
        Some((key.to_owned(), value.to_owned()))
    }

    /// 判断 nodes[j] 是否是 nodes[i] 的后代(依赖树的先序遍历顺序)
    fn is_descendant(nodes: &[Node], ancestor: usize, node: usize) -> bool {
        if node <= ancestor {
            return false;
        }
        let depth = nodes[ancestor].depth;
        for k in (ancestor + 1)..node {
            if nodes[k].depth <= depth {
                return false;
            }
        }
        nodes[node].depth > depth
    }

    pub fn scan() -> Vec<Device> {
        let nodes = ioreg_nodes();
        let mut devices = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            if !node.class.contains("IOUSBHostDevice") {
                continue;
            }
            let vid = node.int("idVendor").unwrap_or(0);
            let pid = node.int("idProduct").unwrap_or(0);
            if vid != VID || pid != PID {
                continue;
            }
            let serial = node
                .text("Serial Number")
                .or_else(|| node.text("kUSBSerialNumberString"))
                .unwrap_or_default();
            let vendor = node
                .text("USB Vendor Name")
                .or_else(|| node.text("kUSBVendorString"))
                .unwrap_or_else(|| "Innostar".to_owned());
            let model = node
                .text("USB Product Name")
                .unwrap_or_else(|| "USB3.0".to_owned());
            let location = node
                .text("locationID")
                .or_else(|| node.text("Location ID"))
                .unwrap_or_default();

            for (j, sub) in nodes.iter().enumerate() {
                if sub.text("Whole").as_deref() != Some("Yes") || !is_descendant(&nodes, index, j) {
                    continue;
                }
                let Some(bsd) = sub.text("BSD Name") else {
                    continue;
                };
                let node_path = Path::new("/dev").join(&bsd);
                let Some(capacity) = sub.int("Size") else {
                    continue;
                };
                let scsi_type = match (sub.class.as_str(), sub.text("Writable").as_deref()) {
                    ("IOCDMedia", _) => 5,
                    ("IOMedia", Some("Yes")) => 0,
                    _ => continue,
                };
                devices.push(Device {
                    node: node_path,
                    vendor: vendor.clone(),
                    model: model.clone(),
                    scsi_type,
                    capacity,
                    sg_node: None,
                    usb: PathBuf::from(&location),
                    interface: PathBuf::from(&location),
                    serial: serial.clone(),
                    devnum: node.text("sessionID").unwrap_or_default(),
                    lun: "0".to_owned(),
                });
            }
        }
        devices.sort_by(|a, b| a.node.cmp(&b.node));
        devices
    }

    pub fn cd_data_track(dev: &Device) -> Result<PathBuf> {
        let nodes = ioreg_nodes();
        let name = dev.node.file_name().and_then(|name| name.to_str());
        let (index, _) = nodes
            .iter()
            .enumerate()
            .find(|(_, node)| {
                node.class == "IOCDMedia"
                    && name.is_some()
                    && node.text("BSD Name").as_deref() == name
            })
            .context("找不到 CD-ROM 媒体节点")?;
        let mut tracks = nodes.iter().enumerate().filter_map(|(child, node)| {
            if is_descendant(&nodes, index, child)
                && node.text("Content").as_deref() == Some("CD_ROM_Mode_1")
            {
                node.text("BSD Name")
            } else {
                None
            }
        });
        let track = tracks.next().context("找不到 CD-ROM 数据轨")?;
        ensure!(tracks.next().is_none(), "CD-ROM 有多个数据轨");
        Ok(Path::new("/dev").join(track))
    }

    pub fn lock_file(dev: &Device) -> Result<fs::File> {
        let name = if dev.serial.is_empty() {
            dev.usb.to_string_lossy().to_string()
        } else {
            dev.serial.clone()
        };
        let path = Path::new("/var/tmp").join(format!("is903burn-{}.lock", sanitize(&name)));
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .context("无法创建锁文件")
    }

    fn sanitize(name: &str) -> String {
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }

    /// 接口释放后按 USB 位置和序列号等待同一设备重新枚举。
    pub fn reenumerate(device: &Device, timeout: Duration) -> Result<Device> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(found) = scan().into_iter().find(|d| {
                d.same_device(device)
                    && d.scsi_type == device.scsi_type
                    && !d.devnum.is_empty()
                    && (!d.same_connection(device) || d.node != device.node)
            }) {
                return Ok(found);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(anyhow::anyhow!("等待 IS903 重新枚举超时，请检查设备连接"))
    }
}

// ---------------------------------------------------------- 平台分发 ---

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;

pub fn scan() -> Vec<Device> {
    imp::scan()
}

pub fn reenumerate(device: &Device, timeout: Duration) -> Result<Device> {
    imp::reenumerate(device, timeout)
}

fn lock_file(dev: &Device) -> Result<std::fs::File> {
    imp::lock_file(dev)
}

#[cfg(target_os = "macos")]
pub fn cd_data_track(dev: &Device) -> Result<PathBuf> {
    imp::cd_data_track(dev)
}
