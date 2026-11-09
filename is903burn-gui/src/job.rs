//! 后台烧录任务:卸载 → 生成 ISO → 写入 → 刷缓存 → 校验 → 挂载。
//!
//! 全部在独立线程里跑,通过 channel 把进度和日志送回 UI。

use crate::{burn, device, iso, mount};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

#[derive(Debug)]
pub enum Msg {
    Log(String),
    Progress { stage: String, frac: f32 },
    Done(Result<JobOk, String>),
}

#[derive(Debug, Clone)]
pub struct JobOk {
    pub mount_point: Option<PathBuf>,
    pub iso_bytes: u64,
    pub iso_sectors: u32,
    pub files: usize,
    pub dirs: usize,
    pub secs: f64,
}

pub struct Job {
    pub rx: Receiver<Msg>,
}

/// 启动烧录任务(不阻塞 UI)
pub fn spawn(
    blk: PathBuf,
    sources: Vec<PathBuf>,
    label: String,
    ctx: Option<eframe::egui::Context>,
) -> Job {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let r = run(&blk, &sources, &label, &tx, ctx.as_ref());
        let _ = match r {
            Ok(mut ok) => {
                ok.secs = t0.elapsed().as_secs_f64();
                tx.send(Msg::Done(Ok(ok)))
            }
            Err(e) => tx.send(Msg::Done(Err(format!("{e:#}")))),
        };
    });
    Job { rx }
}

pub fn run(
    blk: &std::path::Path,
    sources: &[PathBuf],
    label: &str,
    tx: &Sender<Msg>,
    ctx: Option<&eframe::egui::Context>,
) -> anyhow::Result<JobOk> {
    macro_rules! logp {
        ($($a:tt)*) => {{
            let _ = tx.send(Msg::Log(format!($($a)*)));
            if let Some(c) = ctx { c.request_repaint(); }
        }};
    }
    macro_rules! prog {
        ($stage:expr, $frac:expr) => {{
            let _ = tx.send(Msg::Progress { stage: $stage.to_string(), frac: $frac });
            if let Some(c) = ctx { c.request_repaint(); }
        }};
    }

    // ---- 1. 卸载
    prog!("卸载设备", 0.0);
    let mps = mount::mount_points(blk);
    if mps.is_empty() {
        logp!("设备未挂载,无需卸载");
    } else {
        for mp in &mps {
            logp!("卸载 {}", mp.display());
        }
        mount::unmount(blk)?;
        logp!("已卸载");
    }

    // ---- 2. 生成 ISO
    prog!("生成 ISO 镜像", 0.02);
    let tmp = std::env::temp_dir().join(format!(
        "is903burn-{}-{}.iso",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    logp!("生成 ISO → {}", tmp.display());

    let summary = {
        let tx2 = tx.clone();
        let ctx2 = ctx.cloned();
        let mut last = 0.0f32;
        iso::build_iso(
            sources,
            label,
            &tmp,
            &mut |done, total| {
                if total > 0 {
                    let f = 0.02 + 0.43 * (done as f32 / total as f32);
                    if f - last > 0.005 || done == total {
                        last = f;
                        let _ = tx2.send(Msg::Progress {
                            stage: "生成 ISO 镜像".into(),
                            frac: f,
                        });
                        if let Some(c) = &ctx2 {
                            c.request_repaint();
                        }
                    }
                }
            },
        )?
    };
    logp!(
        "ISO 完成: {} 个文件 / {} 个目录, {} 扇区, {}",
        summary.files,
        summary.dirs,
        summary.sectors,
        device::human_size(summary.bytes)
    );

    // ---- 3. 写入
    let iso_len = std::fs::metadata(&tmp)?.len();
    let dev_cap = {
        // 重新读一次容量,避免设备变化
        let name = blk.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        device::scan()
            .into_iter()
            .find(|d| d.name == name)
            .map(|d| d.capacity)
            .unwrap_or(0)
    };
    if dev_cap > 0 && iso_len > dev_cap {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!(
            "镜像 {} 超过设备容量 {}",
            device::human_size(iso_len),
            device::human_size(dev_cap)
        );
    }

    prog!("写入设备", 0.45);
    logp!("开始写入 {} …", device::human_size(iso_len));
    {
        let tx2 = tx.clone();
        let ctx2 = ctx.cloned();
        let mut last = 0.0f32;
        burn::write_iso(blk, &tmp, |done, total| {
            if total > 0 {
                let f = 0.45 + 0.30 * (done as f32 / total as f32);
                if f - last > 0.004 || done == total {
                    last = f;
                    let _ = tx2.send(Msg::Progress {
                        stage: "写入设备".into(),
                        frac: f,
                    });
                    if let Some(c) = &ctx2 {
                        c.request_repaint();
                    }
                }
            }
        })?;
    }
    logp!("写入完成并已 fsync");

    // ---- 4. 刷内核缓存
    prog!("刷新内核缓存", 0.76);
    burn::drop_cache(blk)?;
    logp!("已丢弃该设备的页缓存");

    // ---- 5. 校验
    prog!("校验一致性", 0.80);
    logp!("用 O_DIRECT 回读比对(绕过内核缓存)…");
    let ok = {
        let tx2 = tx.clone();
        let ctx2 = ctx.cloned();
        let mut last = 0.0f32;
        burn::verify_iso(blk, &tmp, |done, total| {
            if total > 0 {
                let f = 0.80 + 0.17 * (done as f32 / total as f32);
                if f - last > 0.004 || done == total {
                    last = f;
                    let _ = tx2.send(Msg::Progress {
                        stage: "校验一致性".into(),
                        frac: f,
                    });
                    if let Some(c) = &ctx2 {
                        c.request_repaint();
                    }
                }
            }
        })?
    };
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("校验失败:设备上的内容与 ISO 不一致");
    }
    logp!("校验通过:设备内容与 ISO 逐字节一致");

    // ---- 6. 挂载
    prog!("挂载设备", 0.97);
    let mp = match mount::mount(blk) {
        Ok(p) => {
            logp!("已挂载到 {}", p.display());
            Some(p)
        }
        Err(e) => {
            logp!("挂载失败(不影响烧录结果): {e}");
            None
        }
    };

    prog!("完成", 1.0);
    let _ = std::fs::remove_file(&tmp);
    logp!("临时 ISO 已清理");

    Ok(JobOk {
        mount_point: mp,
        iso_bytes: summary.bytes,
        iso_sectors: summary.sectors,
        files: summary.files,
        dirs: summary.dirs,
        secs: 0.0,
    })
}
