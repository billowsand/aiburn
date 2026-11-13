use crate::device::{self, Device};
use crate::job::{self, Job, Msg};
use eframe::egui::{self, Align, Color32, RichText};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const BACKGROUND: Color32 = Color32::from_rgb(18, 23, 30);
const SURFACE: Color32 = Color32::from_rgb(27, 34, 44);
const BORDER: Color32 = Color32::from_rgb(47, 59, 72);
const MUTED: Color32 = Color32::from_rgb(163, 178, 193);
const ACCENT: Color32 = Color32::from_rgb(93, 224, 193);
const ERROR: Color32 = Color32::from_rgb(255, 152, 143);

pub struct App {
    devices: Vec<Device>,
    selected: usize,
    sources: Vec<PathBuf>,
    label: String,
    access_error: Option<String>,
    confirmation: Option<Device>,
    job: Option<Job>,
    result: Option<Result<job::JobOk, String>>,
    stage: String,
    progress: f32,
    log: Vec<String>,
    show_log: bool,
    last_scan: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        install_fonts(ctx);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BACKGROUND;
        visuals.window_fill = SURFACE;
        visuals.extreme_bg_color = BACKGROUND;
        visuals.override_text_color = Some(Color32::from_rgb(229, 237, 245));
        visuals.weak_text_color = Some(MUTED);
        visuals.selection.bg_fill = Color32::from_rgb(36, 95, 89);
        ctx.set_visuals(visuals);
        ctx.global_style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size.y = 28.0;
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                widget.corner_radius = 6.into();
            }
        });
        let mut app = Self {
            devices: Vec::new(),
            selected: 0,
            sources: Vec::new(),
            label: String::new(),
            access_error: None,
            confirmation: None,
            job: None,
            result: None,
            stage: String::new(),
            progress: 0.0,
            log: Vec::new(),
            show_log: false,
            last_scan: Instant::now(),
        };
        app.refresh_devices();
        app
    }

    fn current(&self) -> Option<&Device> {
        self.devices.get(self.selected)
    }

    fn refresh_devices(&mut self) {
        let devices = device::scan();
        self.selected = self
            .current()
            .and_then(|current| devices.iter().position(|d| d.same_device(current)))
            .unwrap_or(0);
        self.devices = devices;
        self.check_access();
        self.last_scan = Instant::now();
    }

    fn check_access(&mut self) {
        self.access_error = self
            .current()
            .and_then(|d| d.check_access().err())
            .map(|e| format!("{e:#}"));
    }

    fn poll_job(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some(job) = &self.job else { return };
        let result = loop {
            match job.rx.try_recv() {
                Ok(Msg::Progress { stage, frac }) => {
                    self.stage = stage;
                    self.progress = frac;
                }
                Ok(Msg::Log(line)) => self.log.push(line),
                Ok(Msg::Done(result)) => break result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => break Err("烧录任务意外退出".into()),
            }
        };
        if let Err(error) = &result {
            self.log.push(error.clone());
        }
        self.result = Some(result);
        self.job = None;
        self.refresh_devices();
    }

    fn add_sources(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if path.exists() && !self.sources.contains(&path) {
                self.sources.push(path);
            }
        }
    }

    fn default_label(&self) -> String {
        let label: String = self
            .sources
            .first()
            .and_then(|p| p.file_stem())
            .map(|name| {
                name.to_string_lossy()
                    .chars()
                    .map(|c| {
                        let upper = c.to_ascii_uppercase();
                        if upper.is_ascii_alphanumeric() {
                            upper
                        } else {
                            '_'
                        }
                    })
                    .take(16)
                    .collect()
            })
            .unwrap_or_default();
        if label.trim_matches('_').is_empty() {
            "DATA".into()
        } else {
            label
        }
    }

    fn volume_label(&self) -> String {
        if self.label.trim().is_empty() {
            self.default_label()
        } else {
            self.label.trim().to_owned()
        }
    }

    fn device_ui(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("目标设备").strong());
            let title = self
                .current()
                .map(Device::title)
                .unwrap_or_else(|| "未检测到 IS903".into());
            let previous = self.selected;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("device")
                    .width((ui.available_width() - 72.0).max(170.0))
                    .selected_text(title)
                    .show_ui(ui, |ui| {
                        for (index, d) in self.devices.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, index, d.title());
                        }
                    });
                if ui.button("刷新").clicked() {
                    self.refresh_devices();
                }
            });
            if self.selected != previous {
                self.check_access();
            }
            if let Some(error) = &self.access_error {
                ui.colored_label(ERROR, error);
            }
        });
    }

    fn sources_ui(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("烧录内容").strong());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("添加文件夹").clicked() {
                        self.add_sources(rfd::FileDialog::new().pick_folder());
                    }
                    if ui.button("添加文件").clicked() {
                        self.add_sources(rfd::FileDialog::new().pick_files().unwrap_or_default());
                    }
                });
            });
            egui::Frame::new()
                .fill(BACKGROUND)
                .corner_radius(8)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.sources.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(19.0);
                            ui.label(RichText::new("拖入文件或文件夹").color(MUTED));
                            ui.add_space(19.0);
                        });
                        return;
                    }

                    let mut remove = None;
                    egui::ScrollArea::vertical()
                        .id_salt("sources")
                        .max_height(106.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (index, source) in self.sources.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let name = source
                                        .file_name()
                                        .unwrap_or(source.as_os_str())
                                        .to_string_lossy();
                                    let text = egui::Label::new(name.as_ref()).truncate();
                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            if ui.small_button("移除").clicked() {
                                                remove = Some(index);
                                            }
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(ui.available_width(), 26.0),
                                                egui::Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    ui.add(text)
                                                        .on_hover_text(source.display().to_string())
                                                },
                                            );
                                        },
                                    );
                                });
                            }
                        });
                    if let Some(index) = remove {
                        self.sources.remove(index);
                    }
                });
            ui.horizontal(|ui| {
                ui.label(RichText::new("卷标").color(MUTED));
                let default = self.default_label();
                ui.add(
                    egui::TextEdit::singleline(&mut self.label)
                        .hint_text(default)
                        .desired_width(145.0)
                        .char_limit(16),
                );
                if !self.sources.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.weak(format!("{} 项来源", self.sources.len()));
                    });
                }
            });
        });
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        if self.job.is_some() {
            ui.label(RichText::new(&self.stage).strong());
            ui.add(
                egui::ProgressBar::new(self.progress)
                    .desired_height(14.0)
                    .fill(ACCENT)
                    .show_percentage(),
            );
            return;
        }
        if let Some(result) = &self.result {
            match result {
                Ok(ok) => {
                    ui.colored_label(
                        ACCENT,
                        RichText::new("烧录完成 · 整盘及文件已核验").strong(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(format!(
                            "{} 个文件 · {} · {:.1} 秒",
                            ok.files,
                            device::human_size(ok.iso_bytes),
                            ok.secs
                        ));
                        if ui.button("打开光驱").clicked() {
                            match std::process::Command::new("xdg-open")
                                .arg(&ok.mount_point)
                                .spawn()
                            {
                                Ok(mut child) => {
                                    std::thread::spawn(move || {
                                        let _ = child.wait();
                                    });
                                }
                                Err(e) => {
                                    self.log.push(format!("无法打开挂载目录：{e}"));
                                    self.show_log = true;
                                }
                            }
                        }
                    });
                }
                Err(error) => {
                    ui.colored_label(ERROR, RichText::new("烧录未完成").strong());
                    ui.label(error);
                }
            }
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirmation.clone() else {
            return;
        };
        let label = self.volume_label();
        let mut start = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("confirm-burn")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("覆盖目标光盘？");
            ui.label(target.title());
            ui.weak(format!("卷标 {label} · {} 项来源", self.sources.len()));
            ui.label("烧录会覆盖设备上的现有内容。");
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    cancel = true;
                }
                if ui.add(primary("确认烧录")).clicked() {
                    start = true;
                }
            });
        });
        if start {
            self.result = None;
            self.log.clear();
            self.stage = "准备烧录".into();
            self.progress = 0.0;
            self.job = Some(job::spawn(target, self.sources.clone(), label, ctx.clone()));
        }
        if start || cancel || modal.should_close() {
            self.confirmation = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job();
        let busy = self.job.is_some();
        if busy && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if !busy && self.confirmation.is_none() {
            if self.last_scan.elapsed() >= Duration::from_millis(1500) {
                self.refresh_devices();
            }
            ctx.request_repaint_after(Duration::from_millis(1500));
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            self.add_sources(dropped.into_iter().map(|f| f.path().to_path_buf()));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BACKGROUND).inner_margin(16))
            .show(ui, |ui| {
                let width = ui.available_width().min(468.0);
                ui.vertical_centered(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, ui.available_height()),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(width);
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(34.0, 34.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter()
                                    .rect_filled(rect, 9.0, Color32::from_rgb(31, 66, 64));
                                ui.painter().circle_stroke(
                                    rect.center(),
                                    10.0,
                                    egui::Stroke::new(1.5, ACCENT),
                                );
                                ui.painter().circle_stroke(
                                    rect.center(),
                                    3.0,
                                    egui::Stroke::new(1.5, ACCENT),
                                );
                                ui.label(RichText::new("IS903 烧录工具").size(21.0).strong());
                            });
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .id_salt("main")
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.add_enabled_ui(!busy, |ui| {
                                        self.device_ui(ui);
                                        self.sources_ui(ui);
                                    });
                                    let ready = !busy
                                        && self.current().is_some()
                                        && self.access_error.is_none()
                                        && !self.sources.is_empty();
                                    let caption = if busy {
                                        "烧录进行中"
                                    } else {
                                        "开始烧录"
                                    };
                                    ui.vertical_centered_justified(|ui| {
                                        if ui
                                            .add_enabled(
                                                ready,
                                                primary(caption).min_size(egui::vec2(0.0, 36.0)),
                                            )
                                            .clicked()
                                        {
                                            self.confirmation = self.current().cloned();
                                        }
                                    });
                                    self.status_ui(ui);
                                    if !self.log.is_empty() {
                                        ui.toggle_value(&mut self.show_log, "查看详情");
                                        if self.show_log {
                                            egui::ScrollArea::vertical()
                                                .id_salt("log")
                                                .max_height(86.0)
                                                .stick_to_bottom(true)
                                                .show(ui, |ui| {
                                                    for line in &self.log {
                                                        ui.label(
                                                            RichText::new(line)
                                                                .small()
                                                                .color(MUTED),
                                                        );
                                                    }
                                                });
                                        }
                                    }
                                });
                        },
                    );
                });
            });
        self.confirm_ui(&ctx);
    }
}

fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(11)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(BACKGROUND).strong()).fill(ACCENT)
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
                let data =
                    egui::FontData::from_blob(std::sync::Arc::new(bytes), idx.parse().unwrap_or(0));
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
