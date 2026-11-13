//! 图形界面和命令行共用的烧录流程。

use crate::{burn, device, iso, mount, sg};
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Msg {
    Log(String),
    /// 需要用户注意的提示(例如文件名截断)
    Notice(String),
    Progress {
        stage: String,
        frac: f32,
    },
    Done(Result<JobOk, String>),
}

#[derive(Debug)]
pub struct JobOk {
    pub mount_point: PathBuf,
    pub iso_bytes: u64,
    pub iso_sectors: u32,
    pub files: usize,
    pub dirs: usize,
    pub secs: f64,
    /// 生成镜像时发现的问题(文件名截断等)
    pub warnings: Vec<String>,
}

pub struct Job {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// 请求取消;写入已经开始时,光盘内容可能不完整。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(
    target: device::Device,
    sources: Vec<PathBuf>,
    label: String,
    ctx: eframe::egui::Context,
) -> Job {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    std::thread::spawn(move || {
        let result = run(&target, &sources, &label, &tx, Some(&ctx), &flag);
        let _ = tx.send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
        ctx.request_repaint();
    });
    Job { rx, cancel }
}

pub fn run(
    target: &device::Device,
    sources: &[PathBuf],
    label: &str,
    tx: &Sender<Msg>,
    ctx: Option<&eframe::egui::Context>,
    cancel: &AtomicBool,
) -> Result<JobOk> {
    let started = Instant::now();
    let send = |msg| {
        let _ = tx.send(msg);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    };
    let stage = |name: &str, frac| {
        send(Msg::Progress {
            stage: name.into(),
            frac,
        })
    };
    let log = |s: &str| send(Msg::Log(s.into()));
    let notice = |s: &str| send(Msg::Notice(s.into()));
    let check = || -> Result<()> {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消烧录");
        Ok(())
    };
    let progress = |name: &'static str, start: f32, end: f32| {
        let mut last = start;
        move |done: u64, total: u64| -> bool {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            if total > 0 {
                let frac = start + (end - start) * done as f32 / total as f32;
                if frac - last >= 0.005 || done == total {
                    stage(name, frac);
                    last = frac;
                }
            }
            true
        }
    };

    ensure!(!sources.is_empty(), "请添加要烧录的文件或文件夹");
    let dev = target.current()?;
    dev.check_access()?;
    // Keep the lock through re-enumeration, when the device node may change.
    let lock = lock_target(&dev)?;
    check()?;
    stage("制作 ISO 镜像", 0.02);
    let tmp = tempfile::NamedTempFile::new().context("无法创建临时 ISO")?;
    let image = tmp.path();
    let summary = iso::build_iso(
        sources,
        label,
        image,
        dev.capacity,
        &mut progress("制作 ISO 镜像", 0.02, 0.24),
    )
    .context("生成 ISO 失败")?;
    for warning in &summary.warnings {
        notice(warning);
        log(warning);
    }
    check()?;
    stage("计算源文件哈希", 0.25);
    let expected = burn::hash_tree(sources).context("读取源文件失败")?;
    ensure!(
        expected.len() == summary.files,
        "源文件在制作镜像期间发生变化"
    );
    log(&format!(
        "镜像已生成，{} 个文件，{}",
        summary.files,
        device::human_size(summary.bytes)
    ));

    let dev = dev.current()?;
    ensure!(
        summary.bytes <= dev.capacity,
        "镜像 {} 超过设备容量 {}",
        device::human_size(summary.bytes),
        device::human_size(dev.capacity)
    );
    check()?;
    stage("卸载设备", 0.27);
    mount::unmount(&dev.node)?;
    let dev = dev.current()?;

    stage("写入设备", 0.30);
    let writable = dev.is_writable();
    let mut disc = if writable {
        write_disc(&dev, image, progress("写入设备", 0.30, 0.70))?;
        dev
    } else {
        write_disc(&dev, image, progress("写入光驱", 0.30, 0.70))?;
        check()?;
        stage("刷新光驱", 0.72);
        device::reenumerate(&dev, Duration::from_secs(15)).context("镜像已发送，刷新光驱失败")?
    };
    log("设备写入完成");

    // macOS:写完之后设备可能立刻切回只读的 CD-ROM 模式,磁盘节点随之消失。
    // 这时无法继续回读与挂载,直接报告写入成功。
    #[cfg(target_os = "macos")]
    if writable {
        std::thread::sleep(Duration::from_secs(2));
        let again = device::scan();
        let still_there = again
            .iter()
            .any(|d| d.same_connection(&disc) && d.is_writable());
        if !still_there {
            log("写入完成:设备已切换为只读 CD-ROM 模式,请拔插后重新插入查看内容");
            return Ok(JobOk {
                mount_point: PathBuf::new(),
                iso_bytes: summary.bytes,
                iso_sectors: summary.sectors,
                files: summary.files,
                dirs: summary.dirs.saturating_sub(1),
                secs: started.elapsed().as_secs_f64(),
                warnings: summary.warnings,
            });
        }
    }

    check()?;
    stage("回读整盘", 0.76);
    disc = verify(&disc, image, progress("回读整盘", 0.76, 0.91))?;
    log("整盘回读与 ISO 一致");

    check()?;
    stage("挂载并核对文件", 0.93);
    let mount_point = match mount_and_check(&disc, &expected) {
        Err(error) if disc.is_writable() => {
            log(&format!(
                "首次挂载或文件核对失败，重新枚举后重试：{error:#}"
            ));
            disc.current()?;
            mount::unmount(&disc.node)?;
            stage("刷新设备 · 系统可能要求授权", 0.93);
            disc = device::reenumerate(&disc, Duration::from_secs(15))?;
            stage("重新核验镜像", 0.94);
            disc = verify(&disc, image, progress("重新核验镜像", 0.94, 0.97))?;
            stage("挂载并核对文件", 0.98);
            mount_and_check(&disc, &expected)?
        }
        result => result?,
    };
    log(&format!(
        "{} 个文件已核对，挂载于 {}",
        summary.files,
        mount_point.display()
    ));
    stage("烧录完成", 1.0);
    drop(lock);
    Ok(JobOk {
        mount_point,
        iso_bytes: summary.bytes,
        iso_sectors: summary.sectors,
        files: summary.files,
        dirs: summary.dirs.saturating_sub(1),
        secs: started.elapsed().as_secs_f64(),
        warnings: summary.warnings,
    })
}

/// 把镜像写到目标设备：大容量存储走块写入，光盘模式走厂商协议。
fn write_disc(
    dev: &device::Device,
    image: &Path,
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<()> {
    if dev.is_writable() {
        return burn::write_iso(&dev.node, image, progress).context("写入块设备失败");
    }
    #[cfg(target_os = "linux")]
    {
        let sg_node = dev
            .sg_node
            .as_ref()
            .context("找不到目标光驱的 SCSI 通用节点")?;
        let drive = sg::Sg::open(sg_node, true)?;
        ensure!(
            dev.current()?.sg_node.as_ref() == Some(sg_node),
            "目标 SCSI 节点已变更"
        );
        drive.write_iso(image, progress).context("写入光驱失败")
    }
    #[cfg(target_os = "macos")]
    {
        let drive = sg::Sg::capture(dev)?;
        drive
            .write_iso(image, progress)
            .context("写入光驱失败，旧镜像可能已被部分覆盖")?;
        drive.release().context("镜像已发送，释放 USB 接口失败")
    }
    #[cfg(target_os = "windows")]
    {
        // Windows 不需要接管接口，也不需要管理员权限：
        // 直接对卷句柄 \\.\X: 发厂商 CDB。
        sg::Sg::open(&dev.node, true)?
            .write_iso(image, progress)
            .context("写入光驱失败，旧镜像可能已被部分覆盖")
    }
}

/// 把现成的 ISO 直接写到设备（`--write`），不做文件树核对。
pub fn burn_iso(target: &device::Device, image: &Path) -> Result<()> {
    let dev = target.current()?;
    dev.check_access()?;
    let _lock = lock_target(&dev)?;
    ensure!(image.is_file(), "找不到 ISO 文件: {}", image.display());
    let len = std::fs::metadata(image)?.len();
    ensure!(
        len > 0 && len % crate::iso::SECTOR as u64 == 0,
        "ISO 长度必须是 {} 字节的整数倍",
        crate::iso::SECTOR
    );
    ensure!(
        len <= dev.capacity,
        "ISO {} 超过设备容量 {}",
        device::human_size(len),
        device::human_size(dev.capacity)
    );
    mount::unmount(&dev.node)?;
    let dev = dev.current()?;
    write_disc(&dev, image, |_, _| true)?;
    let mut disc = device::reenumerate(&dev, Duration::from_secs(15))?;
    disc = verify(&disc, image, |_, _| true)?;
    let point = mount::mount(&disc.node).context("整盘已核验，挂载失败")?;
    println!("已挂载于 {}", point.display());
    Ok(())
}

fn lock_target(dev: &device::Device) -> Result<std::fs::File> {
    let lock = dev.lock_file()?;
    // Windows 上 `lock_file` 已经是独占共享模式打开，无需额外加锁
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::fd::AsRawFd;
        let status = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        ensure!(status == 0, "设备正在执行另一项烧录任务");
    }
    Ok(lock)
}

#[cfg(target_os = "macos")]
pub fn verify_cd(target: &device::Device, image: &Path) -> Result<()> {
    let dev = target.current()?;
    ensure!(!dev.is_writable(), "请选择 IS903 CD-ROM 设备");
    dev.check_access()?;
    ensure!(image.is_file(), "找不到要校验的 ISO");
    let _lock = lock_target(&dev)?;
    let disc = verify(&dev, image, |_, _| true)?;
    let track = device::cd_data_track(&disc)?;
    let path = mount::mount(&track).context("ISO 已逐字节校验，重新挂载失败")?;
    println!("ISO 回读一致，已挂载于 {}", path.display());
    Ok(())
}

fn verify(
    dev: &device::Device,
    image: &Path,
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<device::Device> {
    dev.current()?;
    if dev.is_writable() {
        ensure!(
            burn::verify_iso(&dev.node, image, progress)?,
            "整盘回读与 ISO 不一致"
        );
    } else {
        #[cfg(target_os = "windows")]
        {
            // 光盘模式同样用直通 READ(10) 回读，完全不经过文件系统缓存
            ensure!(
                burn::verify_iso(&dev.node, image, progress)?,
                "整盘回读与 ISO 不一致"
            );
        }
        #[cfg(target_os = "linux")]
        sg::Sg::open(&dev.node, false)?.verify_iso(image, progress)?;
        #[cfg(target_os = "macos")]
        {
            let mut progress = progress;
            mount::unmount(&dev.node)?;
            let current = dev.current()?;
            let drive = sg::Sg::capture(&current)?;
            drive.verify_iso(image, &mut progress)?;
            drive.release()?;
            return device::reenumerate(&current, Duration::from_secs(15))
                .context("ISO 已逐字节校验，释放 USB 后未找到光驱");
        }
    }
    Ok(dev.clone())
}

fn mount_and_check(dev: &device::Device, expected: &[(u64, String)]) -> Result<PathBuf> {
    dev.current()?;
    let node = dev.node.clone();
    #[cfg(target_os = "macos")]
    let node = if dev.is_writable() {
        node
    } else {
        device::cd_data_track(dev)?
    };
    let path = mount::mount(&node).context("整盘已核验，挂载失败")?;
    let actual = burn::hash_tree(std::slice::from_ref(&path)).context("读取挂载后的文件失败")?;
    ensure!(
        expected == actual,
        "挂载后的文件与来源不一致（来源 {} 个，读到 {} 个）",
        expected.len(),
        actual.len()
    );
    Ok(path)
}
