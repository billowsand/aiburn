//! egui 界面。

use crate::burn;
use crate::device::{self, Device};
use crate::job::{self, Job, Msg};
use eframe::egui;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STAGE_IDLE: &str = "就绪";

pub struct App {
    devices: Vec<Device>,
    sel: usize,
    sources: Vec<PathBuf>,
    label: String,
    label_touched: bool,
    log: Vec<String>,
    job: Option<Job>,
    stage: String,
    frac: f32,
    busy: bool,
    confirm_open: bool,
    last_scan: Instant,
    last_ok: Option<job::JobOk>,
    started: Option<Instant>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        let devices = device::scan();
        Self {
            devices,
            sel: 0,
            sources: Vec::new(),
            label: String::new(),
            label_touched: false,
            log: vec!["欢迎使用 IS903 烧录工具".to_string()],
            job: None,
            stage: STAGE_IDLE.to_string(),
            frac: 0.0,
            busy: false,
            confirm_open: false,
            last_scan: Instant::now(),
            last_ok: None,
            started: None,
        }
    }

    fn push_log(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.log.push(s);
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    fn current(&self) -> Option<&Device> {
        self.devices.get(self.sel)
    }

    fn data_bytes(&self) -> u64 {
        burn::total_size(&self.sources)
    }

    fn entry_count(&self) -> u64 {
        fn count(p: &std::path::Path, acc: &mut u64) {
            if let Ok(m) = std::fs::symlink_metadata(p) {
                *acc += 1;
                if m.is_dir() {
                    if let Ok(rd) = std::fs::read_dir(p) {
                        for e in rd.flatten() {
                            count(&e.path(), acc);
                        }
                    }
                }
            }
        }
        let mut n = 0;
        for p in &self.sources {
            count(p, &mut n);
        }
        n
    }

    fn est_iso(&self) -> u64 {
        if self.sources.is_empty() {
            return 0;
        }
        burn::estimate_iso_size(self.data_bytes(), self.entry_count())
    }

    fn default_label(&self) -> String {
        let base = self
            .sources
            .first()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "DATA".to_string());
        let up: String = base
            .chars()
            .map(|c| {
                let u = c.to_ascii_uppercase();
                if u.is_ascii_alphanumeric() || u == '_' || u == '-' {
                    u
                } else {
                    '_'
                }
            })
            .collect();
        let mut s: String = up.chars().take(32).collect();
        if s.is_empty() {
            s = "DATA".into();
        }
        s
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let mut done: Option<Result<job::JobOk, String>> = None;
        if let Some(job) = &self.job {
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    Msg::Log(s) => self.log.push(s),
                    Msg::Progress { stage, frac } => {
                        self.stage = stage;
                        self.frac = frac;
                    }
                    Msg::Done(r) => done = Some(r),
                }
            }
        }
        if self.log.len() > 500 {
            let n = self.log.len() - 500;
            self.log.drain(0..n);
        }
        if let Some(r) = done {
            self.job = None;
            self.busy = false;
            self.frac = 1.0;
            match r {
                Ok(ok) => {
                    let secs = self
                        .started
                        .take()
                        .map(|t| t.elapsed().as_secs_f64())
                        .unwrap_or(ok.secs);
                    self.stage = "烧录成功".into();
                    self.push_log(format!(
                        "✔ 完成:{} 个文件 / {} 个目录,{} ({} 扇区),耗时 {:.1} 秒",
                        ok.files,
                        ok.dirs,
                        device::human_size(ok.iso_bytes),
                        ok.iso_sectors,
                        secs
                    ));
                    if let Some(mp) = &ok.mount_point {
                        self.push_log(format!("✔ 已挂载:{}", mp.display()));
                    }
                    self.last_ok = Some(ok);
                }
                Err(e) => {
                    self.started = None;
                    self.stage = "失败".into();
                    self.push_log(format!("✘ 失败:{e}"));
                }
            }
        }
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn start_burn(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.current() else { return };
        let blk = dev.node.clone();
        let label = if self.label.trim().is_empty() {
            self.default_label()
        } else {
            self.label.trim().to_string()
        };
        let sources = self.sources.clone();

        self.busy = true;
        self.confirm_open = false;
        self.frac = 0.0;
        self.stage = "准备中".into();
        self.last_ok = None;
        self.started = Some(Instant::now());
        self.push_log(format!(
            "开始烧录 → {} 卷标 {}",
            blk.display(),
            label
        ));
        self.job = Some(job::spawn(blk, sources, label, Some(ctx.clone())));
    }

    fn ui_device(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("烧录设备:");
            let cur_text = self
                .current()
                .map(|d| d.title())
                .unwrap_or_else(|| "未检测到 IS903 设备".to_string());

            let mut new_sel = self.sel;
            egui::ComboBox::from_id_salt("devsel")
                .width(420.0)
                .selected_text(cur_text)
                .show_ui(ui, |ui| {
                    for (i, d) in self.devices.iter().enumerate() {
                        ui.selectable_value(&mut new_sel, i, d.title());
                    }
                });
            if new_sel != self.sel {
                self.sel = new_sel;
            }
            if ui.button("刷新").clicked() {
                self.devices = device::scan();
                if self.sel >= self.devices.len() {
                    self.sel = 0;
                }
                self.last_scan = Instant::now();
            }
        });

        // 设备状态提示
        match self.current() {
            None => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 80, 80),
                    "✘ 没有找到 Innostor IS903 设备。请插好设备,并确认已加载 sg 模块。",
                );
            }
            Some(d) if !d.is_writable() => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 160, 60),
                    format!(
                        "⚠ 该设备当前是{}。此模式只读,无法烧录 —— 需要它以大容量存储模式(类型 0)出现。",
                        d.status_text()
                    ),
                );
            }
            Some(d) => {
                if !can_write(&d.node) {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 160, 60),
                        "⚠ 当前用户没有写权限。请把自己加入 disk 组(重新登录生效),或给设备加 ACL。",
                    );
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(90, 180, 110),
                        "✔ 设备可写,准备就绪",
                    );
                }
            }
        }
    }

    fn ui_sources(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("烧录内容");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.busy && !self.sources.is_empty(), egui::Button::new("清空"))
                    .clicked()
                {
                    self.sources.clear();
                    if !self.label_touched {
                        self.label.clear();
                    }
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("添加文件夹"))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.sources.push(p);
                        self.after_add();
                    }
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("添加文件"))
                    .clicked()
                {
                    if let Some(ps) = rfd::FileDialog::new().pick_files() {
                        for p in ps {
                            self.sources.push(p);
                        }
                        self.after_add();
                    }
                }
            });
        });

        if self.sources.is_empty() {
            ui.add_space(6.0);
            ui.weak("还没有添加内容。点击右上角「添加文件」或「添加文件夹」。");
            return;
        }

        let mut remove: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("srcs")
            .max_height(180.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, p) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let is_dir = p.is_dir();
                        let (size, nfiles) = dir_stat(p);
                        let icon = if is_dir { "📁" } else { "📄" };
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| p.display().to_string());
                        ui.label(icon);
                        ui.label(egui::RichText::new(name).strong());
                        if is_dir {
                            ui.weak(format!("{} 个文件", nfiles));
                        }
                        ui.weak(device::human_size(size));
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("移除").small())
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                }
            });
        if let Some(i) = remove {
            self.sources.remove(i);
            if self.sources.is_empty() && !self.label_touched {
                self.label.clear();
            }
        }
    }

    fn after_add(&mut self) {
        if !self.label_touched || self.label.is_empty() {
            self.label = self.default_label();
        }
    }

    fn ui_space(&mut self, ui: &mut egui::Ui) {
        let data = self.data_bytes();
        let est = self.est_iso();
        let cap = self.current().map(|d| d.capacity).unwrap_or(0);

        ui.horizontal(|ui| {
            ui.label("卷标:");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.label)
                    .desired_width(200.0)
                    .hint_text("自动生成"),
            );
            if r.changed() {
                self.label_touched = true;
            }
            if ui.button("重置").clicked() {
                self.label = self.default_label();
                self.label_touched = false;
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("已选内容:");
            ui.monospace(device::human_size(data));
            ui.separator();
            ui.label("预计 ISO:");
            ui.monospace(device::human_size(est));
            ui.separator();
            ui.label("设备容量:");
            ui.monospace(if cap > 0 {
                device::human_size(cap)
            } else {
                "未知".into()
            });
        });

        let over = cap > 0 && est > cap;
        let frac = if cap > 0 {
            (est as f32 / cap as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bar = egui::ProgressBar::new(frac)
            .desired_height(18.0)
            .text(if cap > 0 {
                format!("{:.2}%", frac * 100.0)
            } else {
                "—".into()
            });
        ui.add(bar.fill(if over {
            egui::Color32::from_rgb(200, 70, 70)
        } else {
            egui::Color32::from_rgb(70, 140, 200)
        }));

        if over {
            ui.colored_label(
                egui::Color32::from_rgb(220, 80, 80),
                format!(
                    "✘ 超出设备容量 {} —— 请减少内容",
                    device::human_size(est - cap)
                ),
            );
        } else if cap > 0 && est > 0 {
            ui.weak(format!("剩余空间 {}", device::human_size(cap - est)));
        }
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.add_space(4.0);
        ui.add(
            egui::ProgressBar::new(self.frac)
                .desired_height(20.0)
                .text(format!("{}  {:.0}%", self.stage, self.frac * 100.0)),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let dev_ok = self
                .current()
                .map(|d| d.is_writable() && can_write(&d.node))
                .unwrap_or(false);
            let cap = self.current().map(|d| d.capacity).unwrap_or(0);
            let fits = cap == 0 || self.est_iso() <= cap;
            let ready = dev_ok && !self.sources.is_empty() && fits && !self.busy;

            let btn = egui::Button::new(egui::RichText::new("开始烧录").size(16.0).strong())
                .min_size(egui::vec2(140.0, 34.0));
            if ui.add_enabled(ready, btn).clicked() {
                self.confirm_open = true;
            }

            if self.busy {
                ui.spinner();
                ui.label("进行中…");
            } else if !dev_ok {
                ui.weak("设备不可写,无法开始");
            } else if self.sources.is_empty() {
                ui.weak("请先添加要烧录的内容");
            } else if !fits {
                ui.weak("容量不足");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(ok) = &self.last_ok {
                    if let Some(mp) = &ok.mount_point {
                        ui.weak(format!("已挂载: {}", mp.display()));
                    }
                }
            });
        });

        // 日志
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("log")
            .max_height(120.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
    }

    fn ui_confirm(&mut self, ctx: &egui::Context) {
        if !self.confirm_open {
            return;
        }
        let mut open = true;
        egui::Window::new("确认烧录")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("即将覆盖设备上的全部数据,此操作不可撤销。");
                ui.add_space(6.0);
                if let Some(d) = self.current() {
                    ui.monospace(format!("设备: {}", d.node.display()));
                    ui.monospace(format!("容量: {}", device::human_size(d.capacity)));
                }
                ui.monospace(format!("卷标: {}", self.label));
                ui.monospace(format!("大小: {}", device::human_size(self.est_iso())));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let go = egui::Button::new(
                        egui::RichText::new("确认烧录").strong(),
                    )
                    .fill(egui::Color32::from_rgb(180, 70, 70));
                    if ui.add(go).clicked() {
                        self.start_burn(ctx);
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_open = false;
                    }
                });
            });
        if !open {
            self.confirm_open = false;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 定时重扫设备(1.5 秒),处理热插拔与模式切换
        if self.last_scan.elapsed() > Duration::from_millis(1500) {
            let new = device::scan();
            let changed = new.len() != self.devices.len()
                || new
                    .iter()
                    .zip(self.devices.iter())
                    .any(|(a, b)| a.node != b.node || a.scsi_type != b.scsi_type);
            if changed {
                if let Some(cur) = self.current().map(|d| d.node.clone()) {
                    self.sel = new.iter().position(|d| d.node == cur).unwrap_or(0);
                }
                if self.sel >= new.len() {
                    self.sel = 0;
                }
                self.devices = new;
            }
            self.last_scan = Instant::now();
        }

        self.poll_job(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading("IS903 烧录工具");
            ui.add_space(6.0);
            self.ui_device(ui);
            ui.add_space(4.0);
        });

        egui::Panel::bottom("bottom").show(ui, |ui| {
            self.ui_bottom(ui, &ctx);
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_sources(ui);
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);
            self.ui_space(ui);
        });

        self.ui_confirm(&ctx);
    }
}

fn dir_stat(p: &std::path::Path) -> (u64, u64) {
    fn walk(p: &std::path::Path, size: &mut u64, n: &mut u64) {
        if let Ok(m) = std::fs::metadata(p) {
            if m.is_dir() {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        walk(&e.path(), size, n);
                    }
                }
            } else {
                *size += m.len();
                *n += 1;
            }
        }
    }
    let mut s = 0;
    let mut n = 0;
    walk(p, &mut s, &mut n);
    (s, n)
}

/// 试着以写方式打开设备(不会写入任何数据),判断权限是否足够
fn can_write(p: &std::path::Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(p).is_ok()
}

/// 加载中文字体(egui 默认字体不含 CJK)
fn install_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[(&str, u32)] = &[
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2), // index 2 = SC
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Medium.ttc", 2),
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Light.ttc", 2),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
        ("/usr/share/fonts/wqy-microhei/wqy-microhei.ttc", 0),
    ];
    for (path, index) in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            let data = egui::FontData::from_blob(std::sync::Arc::new(bytes), *index);
            fonts
                .font_data
                .insert("cjk".to_owned(), std::sync::Arc::new(data));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(fam)
                    .or_default()
                    .insert(0, "cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    // 退而求其次:用 fc-match 找任意中文字体
    if let Ok(out) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}:%{index}", "sans-serif:lang=zh-cn"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((file, idx)) = s.rsplit_once(':') {
            if let Ok(bytes) = std::fs::read(file) {
                let mut fonts = egui::FontDefinitions::default();
                let data = egui::FontData::from_blob(
                    std::sync::Arc::new(bytes),
                    idx.parse().unwrap_or(0),
                );
                fonts
                    .font_data
                    .insert("cjk".to_owned(), std::sync::Arc::new(data));
                for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts
                        .families
                        .entry(fam)
                        .or_default()
                        .insert(0, "cjk".to_owned());
                }
                ctx.set_fonts(fonts);
            }
        }
    }
}
