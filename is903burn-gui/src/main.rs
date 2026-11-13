//! IS903 烧录工具 —— 图形界面

mod app;
mod burn;
mod device;
mod iso;
mod job;
mod mount;
mod sg;

fn main() -> eframe::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() >= 2 {
        let code = match cli(&a) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        };
        std::process::exit(code);
    }

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([460.0, 420.0])
            .with_min_inner_size([400.0, 390.0])
            .with_app_id("is903burn-gui")
            .with_title("IS903 烧录工具"),
        ..Default::default()
    };

    eframe::run_native(
        "IS903 烧录工具",
        opts,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

fn cli(a: &[String]) -> anyhow::Result<i32> {
    match a[1].as_str() {
        "--devices" => {
            for d in device::scan() {
                println!(
                    "{}  vendor={} model={} type={} cap={} 写={}",
                    d.node.display(),
                    d.vendor,
                    d.model,
                    d.scsi_type,
                    device::human_size(d.capacity),
                    d.is_writable()
                );
            }
            Ok(0)
        }
        "--mkiso" => {
            if a.len() < 5 {
                eprintln!("用法: --mkiso <来源…> -l <卷标> -o <输出>");
                return Ok(2);
            }
            let mut srcs = Vec::new();
            let mut label = String::from("DATA");
            let mut out = String::new();
            let mut i = 2;
            while i < a.len() {
                match a[i].as_str() {
                    "-l" => {
                        label = a.get(i + 1).cloned().unwrap_or(label);
                        i += 2;
                    }
                    "-o" => {
                        out = a.get(i + 1).cloned().unwrap_or_default();
                        i += 2;
                    }
                    s => {
                        srcs.push(std::path::PathBuf::from(s));
                        i += 1;
                    }
                }
            }
            if srcs.is_empty() || out.is_empty() {
                eprintln!("缺少来源或输出");
                return Ok(2);
            }
            let s = iso::build_iso(
                &srcs,
                &label,
                std::path::Path::new(&out),
                u64::MAX,
                &mut |done, total| {
                    if done == total {
                        eprintln!("  {done}/{total}");
                    }
                },
            )?;
            println!(
                "sectors={} bytes={} files={} dirs={}",
                s.sectors, s.bytes, s.files, s.dirs
            );
            Ok(0)
        }
        #[cfg(target_os = "macos")]
        "--verify-cd" => {
            anyhow::ensure!(a.len() == 4, "用法: --verify-cd <整盘节点> <ISO>");
            let node = std::fs::canonicalize(&a[2])?;
            let target = device::scan()
                .into_iter()
                .find(|d| d.node == node)
                .ok_or_else(|| anyhow::anyhow!("{} 不是已识别的 IS903 设备", node.display()))?;
            job::verify_cd(&target, std::path::Path::new(&a[3]))?;
            Ok(0)
        }
        "--burn" => {
            // --burn <块设备> -l <卷标> <来源…>
            let mut blk = String::new();
            let mut label = String::from("DATA");
            let mut srcs = Vec::new();
            let mut i = 2;
            while i < a.len() {
                match a[i].as_str() {
                    "-l" => {
                        label = a.get(i + 1).cloned().unwrap_or(label);
                        i += 2;
                    }
                    s if blk.is_empty() => {
                        blk = s.to_string();
                        i += 1;
                    }
                    s => {
                        srcs.push(std::path::PathBuf::from(s));
                        i += 1;
                    }
                }
            }
            if blk.is_empty() || srcs.is_empty() {
                eprintln!("用法: --burn <块设备> -l <卷标> <来源…>");
                return Ok(2);
            }
            let node = std::fs::canonicalize(&blk)?;
            let target = device::scan()
                .into_iter()
                .find(|d| d.node == node)
                .ok_or_else(|| anyhow::anyhow!("{} 不是已识别的 IS903 设备", node.display()))?;
            let (tx, rx) = std::sync::mpsc::channel();
            let r = std::thread::scope(|scope| {
                let worker = scope.spawn(move || job::run(&target, &srcs, &label, &tx, None));
                for m in rx {
                    match m {
                        job::Msg::Log(s) => println!("{s}"),
                        job::Msg::Progress { stage, frac } => {
                            println!("[{:>5.1}%] {stage}", frac * 100.0)
                        }
                        job::Msg::Done(_) => {}
                    }
                }
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("烧录任务意外退出"))?
            });
            match r {
                Ok(ok) => {
                    println!(
                        "✔ 成功: {} 个文件 / {} 个目录, {} ({} 扇区)",
                        ok.files,
                        ok.dirs,
                        device::human_size(ok.iso_bytes),
                        ok.iso_sectors
                    );
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("✘ 失败: {e:#}");
                    Ok(1)
                }
            }
        }
        other => {
            eprintln!("未知参数: {other}");
            Ok(2)
        }
    }
}
