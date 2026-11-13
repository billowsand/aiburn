//! IS903 烧录工具 —— 图形界面
//!
//! 发布版不弹控制台窗口；带 CLI 参数时会挂回父进程的控制台。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod burn;
mod device;
mod iso;
mod job;
mod mount;
mod sg;
#[cfg(target_os = "windows")]
mod winapi;

fn device_node(arg: &str) -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let letter = winapi::drive_letter(std::path::Path::new(arg))?;
        Ok(std::path::PathBuf::from(format!("{letter}:")))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(std::fs::canonicalize(arg)?)
    }
}

fn find_device(node: &std::path::Path) -> anyhow::Result<device::Device> {
    device::scan()
        .into_iter()
        .find(|d| d.node == node)
        .ok_or_else(|| anyhow::anyhow!("{} 不是已识别的 IS903 设备", node.display()))
}

fn main() -> eframe::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    // 只给路径(例如把文件拖到 exe 图标上)时，直接开界面并预填这些来源
    let mut initial: Vec<std::path::PathBuf> = Vec::new();
    if a.len() >= 2 {
        let only_paths = a[1..]
            .iter()
            .all(|arg| !arg.starts_with('-') && std::path::Path::new(arg).exists());
        if only_paths {
            initial = a[1..].iter().map(std::path::PathBuf::from).collect();
        } else {
            #[cfg(target_os = "windows")]
            winapi::attach_parent_console();
            let code = match cli(&a) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("错误: {e:#}");
                    1
                }
            };
            std::process::exit(code);
        }
    }

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([480.0, 560.0])
            .with_min_inner_size([440.0, 520.0])
            .with_app_id("is903burn-gui")
            .with_title("IS903 烧录工具")
            // Windows 上拖放默认开启，这里显式声明，避免后端默认值变化后静默失效
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "IS903 烧录工具",
        opts,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))),
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
                eprintln!("用法: --mkiso <来源…> -l <卷标> -o <输出>");
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
                    true
                },
            )?;
            for warning in &s.warnings {
                eprintln!("提示: {warning}");
            }
            println!(
                "sectors={} bytes={} files={} dirs={}",
                s.sectors, s.bytes, s.files, s.dirs
            );
            Ok(0)
        }
        "--write" => {
            anyhow::ensure!(a.len() == 4, "用法: --write <设备> <ISO>");
            let node = device_node(&a[2])?;
            let target = find_device(&node)?;
            job::burn_iso(&target, std::path::Path::new(&a[3]))
                .map_err(|e| anyhow::anyhow!("{e:#}"))?;
            Ok(0)
        }
        #[cfg(not(target_os = "macos"))]
        "--dump" => {
            anyhow::ensure!(a.len() == 4, "用法: --dump <设备> <输出.iso>");
            let node = device_node(&a[2])?;
            let target = find_device(&node)?;
            let drive = sg::Sg::open(&target.node, false)?;
            let bytes = drive.dump_iso(std::path::Path::new(&a[3]), |done, total| {
                eprintln!("  {done}/{total}");
                true
            })?;
            println!("已导出 {bytes} 字节到 {}", a[3]);
            Ok(0)
        }
        #[cfg(not(target_os = "macos"))]
        "--verify" => {
            anyhow::ensure!(a.len() == 4, "用法: --verify <设备> <ISO>");
            anyhow::ensure!(
                std::path::Path::new(&a[3]).is_file(),
                "找不到 ISO 文件: {}",
                a[3]
            );
            let node = device_node(&a[2])?;
            let target = find_device(&node)?;
            let drive = sg::Sg::open(&target.node, false)?;
            drive
                .verify_iso(std::path::Path::new(&a[3]), |done, total| {
                    eprintln!("  {done}/{total}");
                    true
                })
                .map_err(|e| anyhow::anyhow!("整盘回读校验失败: {e:#}"))?;
            println!("整盘回读一致: {}", a[3]);
            let _ = device::reenumerate(&target, std::time::Duration::from_secs(15));
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
            let node = device_node(&blk)?;
            let target = device::scan()
                .into_iter()
                .find(|d| d.node == node)
                .ok_or_else(|| anyhow::anyhow!("{} 不是已识别的 IS903 设备", node.display()))?;
            let (tx, rx) = std::sync::mpsc::channel();
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let r = std::thread::scope(|scope| {
                let worker =
                    scope.spawn(move || job::run(&target, &srcs, &label, &tx, None, &cancel));
                for m in rx {
                    match m {
                        job::Msg::Log(s) => println!("{s}"),
                        job::Msg::Notice(s) => println!("提示: {s}"),
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
