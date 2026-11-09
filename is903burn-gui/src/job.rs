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
    /// 挂载后逐文件核对的结果;(None = 未能核对)
    pub file_check: Option<(usize, Vec<String>)>,
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

    // ---- 2. 判断可写路径
    let blk_name = blk
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dev = device::scan().into_iter().find(|d| d.name == blk_name);
    let use_sg = match &dev {
        Some(d) => !d.is_writable(),
        None => false,
    };
    if use_sg {
        logp!("设备为光驱模式,使用 SG_IO 写入");
    } else {
        logp!("设备为可写块设备,直接写入");
    }

    // ---- 3. 生成 ISO
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
    if use_sg {
        let logger = |line: String| logp!("{line}");
        write_via_sg(blk, &blk_name, &tmp, iso_len, &logger)?;
    } else {
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
    logp!("写入完成");

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
    prog!("挂载设备", 0.96);
    let mut mp = match mount::mount(blk) {
        Ok(p) => {
            logp!("已挂载到 {}", p.display());
            Some(p)
        }
        Err(e) => {
            logp!("挂载失败,稍后尝试重新枚举设备: {e}");
            None
        }
    };

    // ---- 7. 文件级核对(内容多重集合比对)
    let mut file_check: Option<(usize, Vec<String>)> = None;
    if let Some(mp) = &mp {
        prog!("核对文件内容", 0.985);
        logp!("核对挂载后的文件内容(按大小+哈希比对)…");
        match (burn::hash_tree(sources), burn::hash_tree(&[mp.clone()])) {
            (Ok(a), Ok(b)) => {
                let (matched, problems) = burn::compare_sets(&a, &b);
                if problems.is_empty() && a.len() == b.len() {
                    logp!("✔ 文件核对通过:{} 个文件全部一致", a.len());
                } else {
                    logp!(
                        "⚠ 文件核对:{} 个一致,源 {} 个 / 盘上 {} 个",
                        matched,
                        a.len(),
                        b.len()
                    );
                    for p in &problems {
                        logp!("   {p}");
                    }
                }
                file_check = Some((matched, problems));
            }
            (Err(e), _) | (_, Err(e)) => logp!("⚠ 无法完成文件核对: {e}"),
        }
    }

    // ---- 8. 必要时重新枚举设备再核对一次
    //
    // 设备内部可能缓存着旧镜像,导致挂载/读取到旧内容。这时对 usb-storage
    // 接口做一次 unbind/bind,让固件重新读取闪存,再挂载核对。
    let need_refresh = match &file_check {
        Some((_, probs)) => !probs.is_empty() || mp.is_none(),
        None => mp.is_none(),
    };
    if need_refresh {
        let name = blk
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        logp!("检测到内容可能未刷新,正在重新枚举设备(无需拔插)…");
        match device::reenumerate(&name, std::time::Duration::from_secs(15)) {
            Ok(nd) => {
                logp!("设备已重新枚举为 {}", nd.node.display());
                // 新节点可能变成 CD-ROM(/dev/srN)
                if let Ok(p) = mount::mount(&nd.node) {
                    logp!("已挂载到 {}", p.display());
                    mp = Some(p.clone());
                    // 重新核对
                    match (burn::hash_tree(sources), burn::hash_tree(&[p])) {
                        (Ok(a), Ok(b)) => {
                            let (matched, problems) = burn::compare_sets(&a, &b);
                            if problems.is_empty() && a.len() == b.len() {
                                logp!("✔ 重新枚举后文件核对通过:{} 个文件全部一致", a.len());
                            } else {
                                logp!("⚠ 重新枚举后仍有 {} 个文件不一致", problems.len());
                                for x in &problems {
                                    logp!("   {x}");
                                }
                            }
                            file_check = Some((matched, problems));
                        }
                        _ => logp!("⚠ 重新枚举后无法完成文件核对"),
                    }
                } else {
                    logp!("⚠ 重新枚举后挂载失败(烧录本身已完成)");
                }
            }
            Err(e) => logp!("⚠ 重新枚举失败:{e}(可手动拔插后查看)"),
        }
    }

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
        file_check,
    })
}

/// 通过 SG_IO 写入光驱类设备(IS903 自定义 WRITE(12) 布局)
fn write_via_sg(
    blk: &std::path::Path,
    blk_name: &str,
    iso: &std::path::Path,
    iso_len: u64,
    logp: &dyn Fn(String),
) -> anyhow::Result<()> {
    use std::io::Read;
    let sg_path = device::sg_node_of(blk_name)
        .ok_or_else(|| anyhow::anyhow!("找不到 {} 的 /dev/sgN 节点", blk.display()))?;
    logp(format!("打开 {sg_path}"));
    let sg = crate::sg::Sg::open(std::path::Path::new(&sg_path))?;
    let (last, bs) = sg.capacity()?;
    logp(format!(
        "设备容量: 最后一块 LBA={last}, 块大小={bs} 字节"
    ));

    let total_blocks = (iso_len as usize).div_ceil(crate::sg::BLK);
    logp(format!("需要写入 {total_blocks} 块(每块 {} 字节)", crate::sg::BLK));

    let mut f = std::fs::File::open(iso)?;
    let mut buf = vec![0u8; 255 * crate::sg::BLK];
    let mut lba = 0u32;
    while (lba as usize) < total_blocks {
        let n = ((total_blocks - lba as usize).min(255)) as usize;
        let want = n * crate::sg::BLK;
        buf[..want].fill(0);
        f.read_exact(&mut buf[0..want])
            .or_else(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof {
                // 末块不足则读剩余,其余补零
                Ok::<_, std::io::Error>(()) 
            } else { Err(e) })?;
        sg.write12(lba, &mut buf[..want])
            .map_err(|e| anyhow::anyhow!("LBA {lba} 写入失败: {e}"))?;
        lba += n as u32;
    }
    logp(format!("已下发 {lba} 块"));

    // 用 READ(10) 回读「光盘」可见内容做核对
    let mut back = vec![0u8; crate::sg::BLK];
    let mut bad = 0;
    for probe in [0u32, 16, 32] {
        let mut orig = vec![0u8; crate::sg::BLK];
        use std::io::{Seek, SeekFrom};
        let mut f2 = std::fs::File::open(iso)?;
        let off = probe as u64 * crate::sg::BLK as u64;
        if off + crate::sg::BLK as u64 > iso_len {
            continue;
        }
        f2.seek(SeekFrom::Start(off))?;
        f2.read_exact(&mut orig)?;
        match sg.read10(probe, &mut back) {
            Ok(()) => {
                if back != orig {
                    bad += 1;
                    logp(format!("⚠ 回读 LBA {probe} 与源不一致"));
                } else {
                    logp(format!("回读 LBA {probe} 一致"));
                }
            }
            Err(e) => {
                bad += 1;
                logp(format!("⚠ 回读 LBA {probe} 失败: {e}"));
            }
        }
    }
    if bad > 0 {
        anyhow::bail!(
            "写入未被设备接受({bad} 个抽样块回读不一致)。\n\
             该固件在光驱模式下拒绝持久化写入,需要先用量产工具把设备\
             重新开卡为可写配置。"
        );
    }
    Ok(())
}
