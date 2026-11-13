//! 配色、间距与小部件的统一样式。
//!
//! 目标是「小工具」观感: 深色低饱和背景、单一强调色、紧凑的间距、
//! 少量 1px 描边区分层级 —— 不做多余的阴影和大圆角。

use eframe::egui::{self, Color32, CornerRadius, FontId, Frame, Margin, RichText, Stroke, Vec2};

// ------------------------------------------------------------------ 调色板

pub const BG: Color32 = Color32::from_rgb(14, 18, 24);
pub const SURFACE: Color32 = Color32::from_rgb(21, 27, 35);
pub const SURFACE_HI: Color32 = Color32::from_rgb(29, 37, 47);
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(35, 45, 57);
pub const BORDER: Color32 = Color32::from_rgb(37, 47, 59);
pub const TEXT: Color32 = Color32::from_rgb(231, 238, 245);
pub const MUTED: Color32 = Color32::from_rgb(140, 154, 170);
pub const FAINT: Color32 = Color32::from_rgb(100, 112, 126);
pub const ACCENT: Color32 = Color32::from_rgb(64, 208, 176);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(24, 56, 50);
pub const ON_ACCENT: Color32 = Color32::from_rgb(6, 22, 18);
pub const WARN: Color32 = Color32::from_rgb(240, 197, 111);
pub const ERROR: Color32 = Color32::from_rgb(255, 138, 128);

// ------------------------------------------------------------------ 全局样式

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(11);
    visuals.extreme_bg_color = Color32::from_rgb(11, 15, 20);
    visuals.text_edit_bg_color = Some(Color32::from_rgb(16, 21, 28));
    visuals.faint_bg_color = SURFACE;
    visuals.override_text_color = Some(TEXT);
    visuals.weak_text_color = Some(MUTED);
    visuals.selection.bg_fill = Color32::from_rgb(28, 76, 68);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARN;
    visuals.error_fg_color = ERROR;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = SURFACE;
    widgets.noninteractive.weak_bg_fill = SURFACE;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = CornerRadius::same(7);

    widgets.inactive.bg_fill = SURFACE_HI;
    widgets.inactive.weak_bg_fill = SURFACE_HI;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = CornerRadius::same(7);
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = SURFACE_ACTIVE;
    widgets.hovered.weak_bg_fill = SURFACE_ACTIVE;
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(58, 74, 92));
    widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    widgets.hovered.corner_radius = CornerRadius::same(7);
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = Color32::from_rgb(44, 57, 72);
    widgets.active.weak_bg_fill = Color32::from_rgb(44, 57, 72);
    widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 88, 108));
    widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    widgets.active.corner_radius = CornerRadius::same(7);
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = SURFACE_ACTIVE;
    widgets.open.weak_bg_fill = SURFACE_ACTIVE;
    widgets.open.bg_stroke = Stroke::new(1.0, Color32::from_rgb(58, 74, 92));
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.open.corner_radius = CornerRadius::same(7);

    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.menu_margin = Margin::symmetric(6, 4);
        style.text_styles = [
            (egui::TextStyle::Small, FontId::proportional(11.0)),
            (egui::TextStyle::Body, FontId::proportional(12.5)),
            (egui::TextStyle::Button, FontId::proportional(12.5)),
            (egui::TextStyle::Heading, FontId::proportional(15.0)),
            (egui::TextStyle::Monospace, FontId::monospace(11.0)),
        ]
        .into();
    });
}

// ------------------------------------------------------------------ 小部件

/// 卡片: 一层表面 + 1px 描边，所有面板内容都放在卡片里。
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// 卡片里的小标题
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(MUTED).strong());
}

/// 11px 的辅助文字
pub fn tiny(text: impl Into<String>, color: Color32) -> RichText {
    RichText::new(text).size(11.0).color(color)
}

/// 无边框的文字按钮(刷新、移除、查看详情等)
pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(11.5).color(MUTED))
            .frame(false)
            .min_size(Vec2::new(0.0, 20.0)),
    )
}

/// 主操作按钮: 强调色实心，禁用时退成表面色
pub fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let (fill, text, border) = if enabled {
        (ACCENT, ON_ACCENT, Stroke::NONE)
    } else {
        (
            SURFACE_HI,
            Color32::from_rgb(122, 134, 148),
            Stroke::new(1.0, BORDER),
        )
    };
    let button = egui::Button::new(RichText::new(label).size(13.0).strong().color(text))
        .fill(fill)
        .stroke(border)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 34.0));
    let response = ui.add_enabled(enabled, button);
    if enabled && response.hovered() {
        ui.painter().rect_filled(
            response.rect,
            CornerRadius::same(8),
            Color32::from_white_alpha(16),
        );
    }
    response
}

/// 状态胶囊: 小圆点 + 文案
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    let font = FontId::proportional(11.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = Vec2::new(galley.size().x + 21.0, 19.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), SURFACE_HI);
    painter.circle_filled(egui::pos2(rect.left() + 9.0, rect.center().y), 3.0, color);
    painter.galley(
        egui::pos2(rect.left() + 15.0, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
}
