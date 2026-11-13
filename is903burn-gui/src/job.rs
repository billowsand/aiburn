//! 图形界面和命令行共用的烧录流程。

use crate::{burn, device, iso, mount, sg};
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Msg {
    Log(String),
    Progress { stage: String, frac: f32 },
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
}

pub struct Job {
    pub rx: Receiver<Msg>,
}

pub fn spawn(
    target: device::Device,
    sources: Vec<PathBuf>,
    label: String,
    ctx: eframe::egui::Context,
) -> Job {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = run(&target, &sources, &label, &tx, Some(&ctx));
        let _ = tx.send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
        ctx.request_repaint();
    });
    Job { rx }
}

pub fn run(
    target: &device::Device,
    sources: &[PathBuf],
    label: &str,
    tx: &Sender<Msg>,
    ctx: Option<&eframe::egui::Context>,
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
    let progress = |name: &'static str, start: f32, end: f32| {
        let mut last = start;
        move |done: u64, total: u64| {
            if total > 0 {
                let frac = start + (end - start) * done as f32 / total as f32;
                if frac - last >= 0.005 || done == total {
                    stage(name, frac);
                    last = frac;
                }
            }
        }
    };

    ensure!(!sources.is_empty(), "请添加要烧录的文件或文件夹");
    let dev = target.current()?;
    dev.check_access()?;
    // Keep the lock through re-enumeration, when the device node may change.
    let lock = lock_target(&dev)?;
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
    stage("卸载设备", 0.27);
    mount::unmount(&dev.node)?;
    let dev = dev.current()?;

    stage("写入设备", 0.30);
    let mut disc = if dev.is_writable() {
        burn::write_iso(&dev.node, image, progress("写入设备", 0.30, 0.70))
            .context("写入块设备失败")?;
        dev
    } else {
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
            drive
                .write_iso(image, progress("写入光驱", 0.30, 0.70))
                .context("写入光驱失败")?;
        }
        stage("刷新光驱 · 系统可能要求授权", 0.72);
        device::reenumerate(&dev, Duration::from_secs(15)).context("镜像已发送，刷新光驱失败")?
    };
    log("设备写入完成");

    stage("回读整盘", 0.76);
    verify(&disc, image, progress("回读整盘", 0.76, 0.91))?;
    log("整盘回读与 ISO 一致");

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
            verify(&disc, image, progress("重新核验镜像", 0.94, 0.97))?;
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
    })
}

fn lock_target(dev: &device::Device) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let lock = dev.lock_file()?;
    let status = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    ensure!(status == 0, "设备正在执行另一项烧录任务");
    Ok(lock)
}

fn verify(dev: &device::Device, image: &Path, progress: impl FnMut(u64, u64)) -> Result<()> {
    dev.current()?;
    if dev.is_writable() {
        ensure!(
            burn::verify_iso(&dev.node, image, progress)?,
            "整盘回读与 ISO 不一致"
        );
    } else {
        sg::Sg::open(&dev.node, false)?.verify_iso(image, progress)?;
    }
    Ok(())
}

fn mount_and_check(dev: &device::Device, expected: &[(u64, String)]) -> Result<PathBuf> {
    dev.current()?;
    let path = mount::mount(&dev.node).context("整盘已核验，挂载失败")?;
    let actual = burn::hash_tree(std::slice::from_ref(&path)).context("读取挂载后的文件失败")?;
    ensure!(
        expected == actual,
        "挂载后的文件与来源不一致（来源 {} 个，读到 {} 个）",
        expected.len(),
        actual.len()
    );
    Ok(path)
}
