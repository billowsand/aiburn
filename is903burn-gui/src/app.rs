use crate::device::{self, Device};
use crate::job::{self, Job, Msg};
use crate::theme;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, RichText, Stroke, StrokeKind, Vec2,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 空闲时重新扫描设备的间隔(只扫表面，不动设备)
const SCAN_INTERVAL: Duration = Duration::from_secs(5);
/// 统计来源时最多遍历的条目数，超过就显示「以上」
const SCAN_LIMIT: usize = 20_000;

/// 一项待烧录来源
struct Source {
    path: PathBuf,
    is_dir: bool,
    files: usize,
    bytes: u64,
    capped: bool,
}

impl Source {
    fn scan(path: PathBuf) -> Self {
        let meta = std::fs::symlink_metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        if !is_dir {
            return Self {
                bytes: meta.map(|m| m.len()).unwrap_or(0),
                files: 1,
                path,
                is_dir,
                capped: false,
            };
        }

        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut capped = false;
        let mut stack = vec![path.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if files >= SCAN_LIMIT {
                    capped = true;
                    break;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    if let Ok(meta) = entry.metadata() {
                        files += 1;
                        bytes = bytes.saturating_add(meta.len());
                    }
                }
            }
            if capped {
                break;
            }
        }
        Self {
            path,
            is_dir: true,
            files,
            bytes,
            capped,
        }
    }

    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn detail(&self) -> String {
        if !self.is_dir {
            return device::human_size(self.bytes);
        }
        let suffix = if self.capped { " 以上" } else { "" };
        format!(
            "{} 个文件{suffix} · {}",
            self.files,
            device::human_size(self.bytes)
        )
    }
}

pub struct App {
    devices: Vec<Device>,
    selected: usize,
    sources: Vec<Source>,
    label: String,
    label_edited: bool,
    access_error: Option<String>,
    confirmation: Option<Device>,
    confirm_since: Option<Instant>,
    job: Option<Job>,
    result: Option<Result<job::JobOk, String>>,
    stage: String,
    progress: f32,
    started: Option<Instant>,
    log: Vec<String>,
    notice: Option<String>,
    show_log: bool,
    last_scan: Instant,
    close_when_done: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        install_fonts(&cc.egui_ctx);
        let mut app = Self {
            devices: Vec::new(),
            selected: 0,
            sources: Vec::new(),
            label: String::new(),
            label_edited: false,
            access_error: None,
            confirmation: None,
            confirm_since: None,
            job: None,
            result: None,
            stage: String::new(),
            progress: 0.0,
            started: None,
            log: Vec::new(),
            notice: None,
            show_log: false,
            last_scan: Instant::now(),
            close_when_done: false,
        };
        app.refresh_devices(true);
        app.add_sources(initial);
        app
    }

    // -------------------------------------------------------------- 状态

    fn current(&self) -> Option<&Device> {
        self.devices.get(self.selected)
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn ready(&self) -> bool {
        !self.busy()
            && self.current().is_some()
            && self.access_error.is_none()
            && !self.sources.is_empty()
    }

    fn hint(&self) -> Option<&'static str> {
        if self.devices.is_empty() {
            return Some("未检测到设备，请连接 IS903 后点「刷新」");
        }
        if self.access_error.is_some() {
            // 设备卡片里已经有具体原因，这里不再重复
            return None;
        }
        if self.sources.is_empty() {
            return Some("拖入文件或文件夹，文件夹内容会放到光盘根目录");
        }
        None
    }

    fn total_bytes(&self) -> u64 {
        self.sources
            .iter()
            .fold(0u64, |sum, s| sum.saturating_add(s.bytes))
    }

    fn total_files(&self) -> usize {
        self.sources.iter().map(|s| s.files).sum()
    }

    fn refresh_devices(&mut self, force: bool) {
        let devices = device::scan();
        let unchanged = !force
            && devices.len() == self.devices.len()
            && devices.iter().zip(&self.devices).all(|(a, b)| {
                a.same_device(b)
                    && a.node == b.node
                    && a.scsi_type == b.scsi_type
                    && a.capacity == b.capacity
            });
        self.last_scan = Instant::now();
        if unchanged {
            return;
        }
        let previous = self.current().cloned();
        self.selected = previous
            .and_then(|current| devices.iter().position(|d| d.same_device(&current)))
            .unwrap_or(0);
        self.devices = devices;
        self.check_access();
    }

    fn check_access(&mut self) {
        self.access_error = self
            .current()
            .and_then(|d| d.check_access().err())
            .map(|e| format!("{e:#}"));
    }

    fn add_sources(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut added = 0usize;
        let mut missing = 0usize;
        for path in paths {
            if !path.exists() {
                missing += 1;
                continue;
            }
            let key = path_key(&path);
            if self.sources.iter().any(|s| path_key(&s.path) == key) {
                continue;
            }
            self.sources.push(Source::scan(path));
            added += 1;
        }
        if missing > 0 {
            self.notice = Some(format!("已忽略 {missing} 个无法访问的路径"));
        } else if added > 0 {
            self.notice = None;
        }
        if self.sources.is_empty() {
            self.label_edited = false;
        }
    }

    fn default_label(&self) -> String {
        let label: String = self
            .sources
            .first()
            .and_then(|s| s.path.file_stem())
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
                    .collect::<String>()
                    .trim_end_matches('_')
                    .to_owned()
            })
            .unwrap_or_default();
        // 纯中文文件名会被压成一片下划线，这种「卷标」没有意义，直接用 DATA
        let alnum = label.chars().filter(|c| c.is_ascii_alphanumeric()).count();
        if alnum < 2 || label.chars().count() > alnum * 3 {
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

    fn poll_job(&mut self, ctx: &egui::Context) {
        let done = {
            let Some(job) = &self.job else { return };
            loop {
                match job.rx.try_recv() {
                    Ok(Msg::Progress { stage, frac }) => {
                        self.stage = stage;
                        self.progress = frac;
                    }
                    Ok(Msg::Log(line)) => self.log.push(line),
                    Ok(Msg::Notice(text)) => self.notice = Some(text),
                    Ok(Msg::Done(result)) => break Some(result),
                    Err(std::sync::mpsc::TryRecvError::Empty) => return,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break Some(Err("烧录任务意外退出".into()))
                    }
                }
            }
        };
        let Some(result) = done else { return };
        if let Err(error) = &result {
            self.log.push(error.clone());
        }
        self.result = Some(result);
        self.job = None;
        self.refresh_devices(true);
        if self.close_when_done {
            self.close_when_done = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // -------------------------------------------------------------- 界面

    fn header_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(7), theme::ACCENT_SOFT);
            painter.circle_stroke(rect.center(), 7.0, Stroke::new(1.3, theme::ACCENT));
            painter.circle_stroke(rect.center(), 2.4, Stroke::new(1.3, theme::ACCENT));
            ui.add_space(2.0);
            ui.label(RichText::new("IS903 烧录工具").size(15.5).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = self.badge();
                theme::chip(ui, text, color);
            });
        });
    }

    fn badge(&self) -> (&'static str, Color32) {
        if self.busy() {
            return ("烧录中", theme::ACCENT);
        }
        match &self.result {
            Some(Ok(_)) => ("已完成", theme::ACCENT),
            Some(Err(error)) if error.starts_with("已取消") => ("已取消", theme::MUTED),
            Some(Err(_)) => ("失败", theme::ERROR),
            None if self.devices.is_empty() => ("未连接", theme::MUTED),
            None if self.access_error.is_some() => ("未就绪", theme::WARN),
            None if self.sources.is_empty() => ("就绪", theme::ACCENT),
            None => ("就绪", theme::ACCENT),
        }
    }

    fn device_ui(&mut self, ui: &mut egui::Ui) {
        let mut refresh = false;
        let enabled = !self.busy();
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section(ui, "目标设备");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        if theme::ghost_button(ui, "刷新").clicked() {
                            refresh = true;
                        }
                    });
                });
            });
            ui.add_space(2.0);
            if self.devices.is_empty() {
                ui.label(
                    RichText::new("未检测到 IS903 设备")
                        .size(12.0)
                        .color(theme::WARN),
                );
                ui.label(theme::tiny(
                    "请确认设备已插入并处于 CD-ROM 或大容量存储模式",
                    theme::MUTED,
                ));
            } else {
                let title = self
                    .current()
                    .map(Device::title)
                    .unwrap_or_else(|| "选择设备".into());
                let width = ui.available_width();
                let selected = &mut self.selected;
                let mut changed = false;
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt("device")
                        .width(width)
                        .selected_text(RichText::new(title).size(12.5))
                        .show_ui(ui, |ui| {
                            for (index, device) in self.devices.iter().enumerate() {
                                if ui
                                    .selectable_label(*selected == index, device.title())
                                    .clicked()
                                {
                                    *selected = index;
                                    changed = true;
                                }
                            }
                        });
                });
                if changed {
                    self.check_access();
                }
            }
            if let Some(error) = self.access_error.clone() {
                ui.add_space(3.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("⚠ {error}"))
                            .size(11.0)
                            .color(theme::WARN),
                    )
                    .wrap(),
                );
            }
        });
        if refresh {
            self.refresh_devices(true);
        }
    }

    fn sources_ui(&mut self, ui: &mut egui::Ui) {
        let mut pick_files = false;
        let mut pick_folder = false;
        let dragging = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section(ui, "烧录内容");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = !self.busy();
                    ui.add_enabled_ui(enabled, |ui| {
                        if theme::ghost_button(ui, "添加文件夹").clicked() {
                            pick_folder = true;
                        }
                        if theme::ghost_button(ui, "添加文件").clicked() {
                            pick_files = true;
                        }
                    });
                });
            });
            ui.add_space(2.0);
            let list_height = (ui.available_height() - 20.0).max(56.0);
            if self.sources.is_empty() {
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), list_height),
                    egui::Sense::click(),
                );
                let border = if dragging {
                    theme::ACCENT
                } else {
                    theme::BORDER
                };
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(8), theme::SURFACE_HI);
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(8),
                    Stroke::new(1.0, border),
                    StrokeKind::Inside,
                );
                let center = rect.center();
                painter.text(
                    center - Vec2::new(0.0, 9.0),
                    Align2::CENTER_CENTER,
                    "拖入文件或文件夹",
                    FontId::proportional(13.0),
                    if dragging { theme::ACCENT } else { theme::TEXT },
                );
                painter.text(
                    center + Vec2::new(0.0, 12.0),
                    Align2::CENTER_CENTER,
                    "也可以点击这里选择文件",
                    FontId::proportional(11.0),
                    theme::MUTED,
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if response.clicked() && !self.busy() {
                    pick_files = true;
                }
            } else {
                let mut remove = None;
                egui::Frame::new()
                    .fill(theme::SURFACE_HI)
                    .stroke(Stroke::new(
                        1.0,
                        if dragging {
                            theme::ACCENT
                        } else {
                            theme::BORDER
                        },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(4)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("sources")
                            .max_height(list_height - 8.0)
                            .min_scrolled_height(list_height - 8.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 2.0;
                                for (index, source) in self.sources.iter().enumerate() {
                                    if source_row(ui, source) {
                                        remove = Some(index);
                                    }
                                }
                            });
                    });
                if let Some(index) = remove {
                    self.sources.remove(index);
                    if self.sources.is_empty() {
                        self.label_edited = false;
                    }
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let total = self.total_bytes();
                let text = if self.sources.is_empty() {
                    "尚未添加内容".to_owned()
                } else {
                    format!(
                        "{} 个来源 · {} 个文件 · {}",
                        self.sources.len(),
                        self.total_files(),
                        device::human_size(total)
                    )
                };
                ui.label(theme::tiny(text, theme::MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let capacity = self.current().map(|d| d.capacity).unwrap_or(0);
                    if self.sources.is_empty() || capacity == 0 {
                        return;
                    }
                    if total > capacity {
                        ui.label(theme::tiny(
                            format!("超出光盘容量 {}", device::human_size(capacity)),
                            theme::ERROR,
                        ));
                    } else {
                        ui.label(theme::tiny(
                            format!("光盘可用 {}", device::human_size(capacity)),
                            theme::MUTED,
                        ));
                    }
                });
            });
        });
        if pick_files {
            self.add_sources(rfd::FileDialog::new().pick_files().unwrap_or_default());
        }
        if pick_folder {
            self.add_sources(rfd::FileDialog::new().pick_folder());
        }
    }

    fn footer_ui(&mut self, ui: &mut egui::Ui) {
        let mut start = false;
        let mut cancel = false;
        let mut open_mount: Option<PathBuf> = None;
        let busy = self.busy();

        // 用户没改过卷标时，跟着第一个来源自动更新
        if !self.label_edited {
            self.label = self.default_label();
        }

        theme::card(ui, |ui| {
            // 卷标
            ui.horizontal(|ui| {
                ui.label(theme::tiny("卷标", theme::MUTED));
                let width = (ui.available_width() - 46.0).max(60.0);
                let response = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.label)
                        .char_limit(16)
                        .desired_width(width)
                        .font(FontId::proportional(12.5)),
                );
                if response.changed() {
                    self.label_edited = true;
                }
                ui.label(theme::tiny(
                    format!("{}/16", self.label.chars().count()),
                    theme::FAINT,
                ));
            });
            ui.add_space(6.0);

            // 主操作
            let caption = if busy {
                "烧录进行中…"
            } else {
                "开始烧录"
            };
            if theme::primary_button(ui, caption, self.ready()).clicked() {
                start = true;
            }
            ui.add_space(6.0);

            // 状态
            if busy {
                ui.horizontal(|ui| {
                    let stage = if self.stage.is_empty() {
                        "准备中"
                    } else {
                        self.stage.as_str()
                    };
                    ui.label(RichText::new(stage).size(12.0).color(theme::TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::ghost_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                        let elapsed = self
                            .started
                            .map(|s| s.elapsed().as_secs_f32())
                            .unwrap_or(0.0);
                        ui.label(theme::tiny(
                            format!("{:.0}% · {:.0}s", self.progress * 100.0, elapsed),
                            theme::MUTED,
                        ));
                    });
                });
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .desired_height(8.0)
                        .fill(theme::ACCENT)
                        .corner_radius(CornerRadius::same(4)),
                );
            } else if let Some(result) = &self.result {
                match result {
                    Ok(ok) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("✓ 烧录完成")
                                    .size(12.0)
                                    .strong()
                                    .color(theme::ACCENT),
                            );
                            ui.label(theme::tiny(
                                format!(
                                    "{} 个文件 · {} · {:.1} 秒",
                                    ok.files,
                                    device::human_size(ok.iso_bytes),
                                    ok.secs
                                ),
                                theme::MUTED,
                            ));
                            if !ok.mount_point.as_os_str().is_empty() {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::ghost_button(ui, "打开光驱").clicked() {
                                        open_mount = Some(ok.mount_point.clone());
                                    }
                                });
                            }
                        });
                        if !ok.warnings.is_empty() {
                            ui.add(
                                egui::Label::new(theme::tiny(
                                    format!(
                                        "⚠ {} 个文件名超过 64 个字符，已在光盘上截断（详见「查看详情」）",
                                        ok.warnings.len()
                                    ),
                                    theme::WARN,
                                ))
                                .truncate(),
                            );
                        }
                    }
                    Err(error) => {
                        let cancelled = error.starts_with("已取消");
                        let color = if cancelled {
                            theme::MUTED
                        } else {
                            theme::ERROR
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(if cancelled {
                                    "已取消"
                                } else {
                                    "✕ 烧录失败"
                                })
                                .size(12.0)
                                .strong()
                                .color(color),
                            );
                            ui.add(
                                egui::Label::new(RichText::new(error).size(11.0).color(color))
                                    .truncate(),
                            );
                        });
                    }
                }
            } else if let Some(hint) = self.hint() {
                ui.label(theme::tiny(hint, theme::FAINT));
            }

            if let Some(notice) = &self.notice {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("⚠ {notice}"))
                            .size(11.0)
                            .color(theme::WARN),
                    )
                    .truncate(),
                );
            }

            // 日志(单独浮窗，避免挤压紧凑布局)
            if !self.log.is_empty() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let label = if self.show_log {
                        "收起详情".to_owned()
                    } else {
                        format!("查看详情 ({})", self.log.len())
                    };
                    if theme::ghost_button(ui, &label).clicked() {
                        self.show_log = !self.show_log;
                    }
                });
            }
        });

        if start && self.ready() {
            self.confirmation = self.current().cloned();
            self.confirm_since = Some(Instant::now());
        }
        if cancel {
            if let Some(job) = &self.job {
                job.cancel();
            }
            self.stage = "正在取消…".into();
        }
        if let Some(path) = open_mount {
            if let Err(error) = open_path(&path) {
                self.log
                    .push(format!("无法打开 {}：{error}", path.display()));
                self.notice = Some("无法自动打开目录，请手动在文件管理器里打开".into());
            }
        }
    }

    /// 运行详情放在独立浮窗里，避免展开时把主界面挤变形
    fn log_window(&mut self, ctx: &egui::Context) {
        if !self.show_log || self.log.is_empty() {
            return;
        }
        let mut open = self.show_log;
        let mut copy = false;
        egui::Window::new("运行详情")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([420.0, 190.0])
            .min_size([280.0, 120.0])
            .default_pos(ctx.content_rect().center() - Vec2::new(210.0, 95.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if theme::ghost_button(ui, "复制全部").clicked() {
                        copy = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(theme::tiny(format!("{} 行", self.log.len()), theme::MUTED));
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("log-scroll")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for line in &self.log {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(line).size(11.0).color(theme::MUTED),
                                )
                                .wrap(),
                            );
                        }
                    });
            });
        if copy {
            ctx.copy_text(self.log.join("\n"));
        }
        self.show_log = open;
    }

    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirmation.clone() else {
            return;
        };
        let label = self.volume_label();
        let mut start = false;
        let mut cancel = false;
        // 刚打开时的误触(例如双击「开始烧录」的第二次点击)不算确认
        let armed = self
            .confirm_since
            .is_none_or(|since| since.elapsed() >= Duration::from_millis(400));
        let modal = egui::Modal::new(egui::Id::unique("confirm-burn")).show(ctx, |ui| {
            ui.set_width(330.0);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(RichText::new("覆盖光盘内容？").size(15.0).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new(target.title()).size(12.0));
                    ui.add_space(2.0);
                    ui.label(theme::tiny(
                        format!(
                            "卷标 {label} · {} 个来源 · {} 个文件 · {}",
                            self.sources.len(),
                            self.total_files(),
                            device::human_size(self.total_bytes())
                        ),
                        theme::MUTED,
                    ));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("烧录会覆盖光盘上现有的全部内容，且无法撤销。")
                            .size(12.0)
                            .color(theme::WARN),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 8.0) / 2.0;
                        if ui
                            .add_sized(
                                Vec2::new(width, 30.0),
                                egui::Button::new(RichText::new("取消").size(12.5)),
                            )
                            .clicked()
                        {
                            cancel = true;
                        }
                        let confirm = egui::Button::new(
                            RichText::new("确认烧录")
                                .size(12.5)
                                .strong()
                                .color(theme::ON_ACCENT),
                        )
                        .fill(theme::ACCENT)
                        .stroke(Stroke::NONE);
                        if ui.add_sized(Vec2::new(width, 30.0), confirm).clicked() && armed {
                            start = true;
                        }
                    });
                });
        });

        if start {
            self.result = None;
            self.log.clear();
            self.notice = None;
            self.stage = "准备烧录".into();
            self.progress = 0.0;
            self.started = Some(Instant::now());
            let sources = self.sources.iter().map(|s| s.path.clone()).collect();
            self.job = Some(job::spawn(target, sources, label, ctx.clone()));
        }
        if start || cancel || modal.should_close() {
            self.confirmation = None;
            self.confirm_since = None;
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        if self.busy() || self.confirmation.is_some() {
            return;
        }
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drop-overlay"),
        ));
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(8, 12, 16, 215));
        painter.rect_stroke(
            rect.shrink(7.0),
            CornerRadius::same(10),
            Stroke::new(1.5, theme::ACCENT),
            StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "松开即可加入烧录列表",
            FontId::proportional(15.0),
            theme::TEXT,
        );
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        let busy = self.busy();

        if busy && ctx.input(|i| i.viewport().close_requested()) {
            if let Some(job) = &self.job {
                job.cancel();
            }
            self.close_when_done = true;
            self.stage = "正在取消…".into();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if busy {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if self.confirmation.is_none() {
            let focused = ctx.input(|i| i.focused);
            if focused && self.last_scan.elapsed() >= SCAN_INTERVAL {
                self.refresh_devices(false);
            }
            ctx.request_repaint_after(SCAN_INTERVAL);
        }

        // 拖拽放入
        if !busy && self.confirmation.is_none() {
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                self.add_sources(dropped.into_iter().map(|f| f.path().to_path_buf()));
            }
        }

        // Enter 直接开始，F5 重新扫描设备
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter))
                && self.ready()
                && self.confirmation.is_none()
            {
                self.confirmation = self.current().cloned();
                self.confirm_since = Some(Instant::now());
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                self.refresh_devices(true);
            }
        }

        let margin = egui::Margin::symmetric(14, 9);
        let panel = egui::Frame::new().fill(theme::BG).inner_margin(margin);
        egui::Panel::top("header")
            .resizable(false)
            .frame(panel)
            .show(ui, |ui| self.header_ui(ui));
        egui::Panel::top("device")
            .resizable(false)
            .frame(panel)
            .show(ui, |ui| self.device_ui(ui));
        egui::Panel::bottom("footer")
            .resizable(false)
            .frame(panel)
            .show(ui, |ui| self.footer_ui(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(margin))
            .show(ui, |ui| self.sources_ui(ui));

        self.confirm_ui(&ctx);
        self.log_window(&ctx);
        self.drop_overlay(&ctx);
    }
}

/// 一行来源: 图标 + 名称 + 大小 + 移除
fn source_row(ui: &mut egui::Ui, source: &Source) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 16.0), egui::Sense::hover());
        let painter = ui.painter();
        if source.is_dir {
            // 文件夹: 顶部小标签 + 主体
            let body = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + 4.5),
                egui::pos2(rect.right() - 1.5, rect.bottom() - 2.5),
            );
            let tab = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + 2.0),
                egui::pos2(rect.left() + 7.5, rect.top() + 5.5),
            );
            painter.rect_filled(body, CornerRadius::same(1), theme::MUTED);
            painter.rect_filled(tab, CornerRadius::same(1), theme::MUTED);
        } else {
            // 文件: 一页纸
            let page = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 3.0, rect.top() + 1.0),
                egui::pos2(rect.right() - 3.0, rect.bottom() - 1.5),
            );
            painter.rect_filled(page, CornerRadius::same(1), theme::MUTED);
        }

        let name_width = (ui.available_width() - 108.0).max(48.0);
        ui.allocate_ui_with_layout(
            Vec2::new(name_width, 18.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add(egui::Label::new(RichText::new(source.name()).size(12.5)).truncate())
                    .on_hover_text(source.path.display().to_string());
            },
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            remove = ui
                .add(
                    egui::Button::new(RichText::new("✕").size(10.5).color(theme::FAINT))
                        .frame(false)
                        .min_size(Vec2::new(18.0, 18.0)),
                )
                .on_hover_text("从列表移除")
                .clicked();
            ui.add_space(2.0);
            ui.label(theme::tiny(source.detail(), theme::MUTED));
        });
    });
    remove
}

/// 用系统的文件管理器打开目录
fn open_path(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(path).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

/// 去重用的路径键: Windows 大小写不敏感
fn path_key(path: &Path) -> String {
    let key = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    #[cfg(target_os = "windows")]
    {
        key.to_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        key
    }
}

/// 加载中文字体(egui 自带字体不含 CJK)
///
/// eframe 默认会挂一个系统字体提供者(eframe 的 `system_fonts` 特性)，
/// 所以找不到下面的字体文件也不会显示方块；这里只是优先用更合适的字体。
fn install_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[(&str, u32)] = &[
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2), // index 2 = SC
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Medium.ttc", 2),
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Light.ttc", 2),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
        ("/usr/share/fonts/wqy-microhei/wqy-microhei.ttc", 0),
        ("C:\\Windows\\Fonts\\msyh.ttc", 0),
        ("C:\\Windows\\Fonts\\msyh.ttf", 0),
        ("C:\\Windows\\Fonts\\simhei.ttf", 0),
        ("/System/Library/Fonts/PingFang.ttc", 0),
    ];
    for (path, index) in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            let data = egui::FontData::from_blob(std::sync::Arc::new(bytes), *index);
            fonts
                .font_data
                .insert("cjk".to_owned(), std::sync::Arc::new(data));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, "cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    // 退而求其次: 用 fc-match 找一个可用的中文字体
    if let Ok(out) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}:%{index}", "sans-serif:lang=zh-cn"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((file, index)) = text.rsplit_once(':') {
            if let Ok(bytes) = std::fs::read(file) {
                let mut fonts = egui::FontDefinitions::default();
                let data = egui::FontData::from_blob(
                    std::sync::Arc::new(bytes),
                    index.parse().unwrap_or(0),
                );
                fonts
                    .font_data
                    .insert("cjk".to_owned(), std::sync::Arc::new(data));
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .insert(0, "cjk".to_owned());
                }
                ctx.set_fonts(fonts);
            }
        }
    }
}
