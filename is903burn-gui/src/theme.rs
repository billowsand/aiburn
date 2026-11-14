//! egui 配色。Linux 优先读取当前 Omarchy 主题，其余系统使用默认浅色方案。

use eframe::egui::{self, Color32, CornerRadius, Stroke};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub background: Color32,
    pub accent: Color32,
    pub success: Color32,
    surface: Color32,
    text: Color32,
    border: Color32,
    warning: Color32,
    error: Color32,
    dark: bool,
    omarchy: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            background: Color32::from_rgb(0xed, 0xf2, 0xf8),
            surface: Color32::WHITE,
            text: Color32::from_rgb(0x18, 0x26, 0x3a),
            border: Color32::from_rgb(0xcc, 0xd7, 0xe6),
            accent: Color32::from_rgb(0x28, 0x5b, 0xc8),
            success: Color32::from_rgb(0x23, 0x74, 0x48),
            warning: Color32::from_rgb(0x89, 0x51, 0x0e),
            error: Color32::from_rgb(0xb7, 0x37, 0x32),
            dark: false,
            omarchy: false,
        }
    }
}

impl Palette {
    pub fn current() -> Self {
        #[cfg(target_os = "linux")]
        if let Some(palette) = Self::omarchy() {
            return palette;
        }
        Self::default()
    }

    #[cfg(target_os = "linux")]
    fn omarchy() -> Option<Self> {
        let home = std::env::home_dir()?;
        // Omarchy 新版把生效的主题移到 state；旧版仍使用 config。
        [
            home.join(".local/state/omarchy/current/theme"),
            home.join(".config/omarchy/current/theme"),
        ]
        .into_iter()
        .find_map(|dir| {
            let source = std::fs::read_to_string(dir.join("colors.toml")).ok()?;
            Self::from_omarchy_colors(&source, dir.join("light.mode").is_file())
        })
    }

    #[cfg(target_os = "linux")]
    fn from_omarchy_colors(source: &str, light_marker: bool) -> Option<Self> {
        // 读取 Omarchy 的顶层标量，包括旧版 color0…color15 别名和行尾注释。
        let values: std::collections::HashMap<_, _> = source
            .lines()
            .map(str::trim)
            .take_while(|line| !line.starts_with('['))
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                let value = value.trim_start();
                let quote = value.chars().next()?;
                if quote != '"' && quote != '\'' {
                    return None;
                }
                Some((key.trim(), value[1..].split_once(quote)?.0))
            })
            .collect();
        let color = |keys: &[&str]| {
            keys.iter().find_map(|key| {
                Color32::from_hex(values.get(key)?)
                    .ok()
                    .filter(|color| color.is_opaque())
            })
        };
        let background = color(&["background", "color0"])?;
        let text = color(&["foreground", "color7"])?;
        let accent = color(&["accent", "blue", "color4"]).unwrap_or(text);
        let mode = values.get("mode").or_else(|| values.get("theme_type"));
        let dark = match mode.copied() {
            Some("light") => false,
            Some("dark") => true,
            _ => !light_marker && luminance(background) < 0.35,
        };
        let surface = color(&["lighter_background"])
            .filter(|shade| *shade != background)
            .unwrap_or_else(|| mix(background, text, 0.05));
        // 终端的面板色在浅色主题里有时偏深，应用面板只取一部分。
        let surface = mix(background, surface, if dark { 0.72 } else { 0.42 });
        let readable_status = |keys: &[&str]| {
            let color = color(keys).unwrap_or(text);
            // 终端 ANSI 色里有低对比度颜色，混入正文色使小字号状态仍可读。
            if contrast(color, surface) < 4.5 {
                mix(color, text, 0.65)
            } else {
                color
            }
        };

        Some(Self {
            background,
            surface,
            text,
            accent,
            success: readable_status(&["green", "color2"]),
            warning: readable_status(&["yellow", "color3"]),
            error: readable_status(&["red", "color1"]),
            border: mix(surface, text, 0.23),
            dark,
            omarchy: true,
        })
    }

    pub fn apply(self, ctx: &egui::Context) {
        let theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(theme);
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if self.dark {
            egui::SystemTheme::Dark
        } else {
            egui::SystemTheme::Light
        }));

        let radius = CornerRadius::same(if self.omarchy { 3 } else { 6 });
        let mut visuals = match theme {
            egui::Theme::Dark => egui::Visuals::dark(),
            egui::Theme::Light => egui::Visuals::light(),
        };
        visuals.panel_fill = self.background;
        visuals.window_fill = self.surface;
        visuals.window_stroke = Stroke::new(1.0, self.border);
        visuals.window_corner_radius = radius;
        visuals.menu_corner_radius = radius;
        visuals.extreme_bg_color = self.background;
        visuals.text_edit_bg_color = Some(self.background);
        visuals.faint_bg_color = mix(self.surface, self.text, 0.06);
        visuals.weak_text_color = Some(mix(self.surface, self.text, 0.75));
        visuals.hyperlink_color = self.accent;
        visuals.error_fg_color = self.error;
        visuals.warn_fg_color = self.warning;
        visuals.selection = egui::style::Selection {
            bg_fill: mix(self.surface, self.accent, 0.25),
            stroke: Stroke::new(1.0, self.accent),
        };

        let widgets = &mut visuals.widgets;
        for (widget, fill, border) in [
            (&mut widgets.noninteractive, self.surface, self.border),
            (&mut widgets.inactive, visuals.faint_bg_color, self.border),
            (
                &mut widgets.hovered,
                mix(self.surface, self.accent, 0.12),
                self.accent,
            ),
            (
                &mut widgets.active,
                mix(self.surface, self.accent, 0.23),
                self.accent,
            ),
            (
                &mut widgets.open,
                mix(self.surface, self.accent, 0.16),
                self.accent,
            ),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = Stroke::new(1.0, border);
            widget.fg_stroke = Stroke::new(1.0, self.text);
            widget.corner_radius = radius;
            widget.expansion = 0.0;
        }

        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals;
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.interact_size.y = 28.0;
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
        });
    }

    pub fn primary_button(self, text: &str) -> egui::Button<'_> {
        let text_color =
            if contrast(self.accent, Color32::WHITE) >= contrast(self.accent, Color32::BLACK) {
                Color32::WHITE
            } else {
                Color32::BLACK
            };
        egui::Button::new(egui::RichText::new(text).strong().color(text_color)).fill(self.accent)
    }
}

fn mix(base: Color32, other: Color32, weight: f32) -> Color32 {
    base.lerp_to_gamma(other, weight)
}

fn luminance(color: Color32) -> f32 {
    let [r, g, b, _] = egui::Rgba::from(color).to_array();
    r * 0.2126 + g * 0.7152 + b * 0.0722
}

fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn reads_semantic_colors_and_explicit_mode() {
        let palette = Palette::from_omarchy_colors(
            r##"
            mode = "light"
            background = "#222222"
            foreground = "#eeeeee"
            # accent = "#ffffff"
            accent = "#78824b" # inline comment
            [other]
            accent = "#000000"
            "##,
            false,
        )
        .unwrap();
        assert!(!palette.dark);
        assert_eq!(palette.background, Color32::from_rgb(0x22, 0x22, 0x22));
        assert_eq!(palette.accent, Color32::from_rgb(0x78, 0x82, 0x4b));
    }

    #[test]
    fn reads_legacy_palette_and_light_marker() {
        let source = "color0 = '#1a1b26'\ncolor7 = '#a9b1d6'\ncolor4 = '#7aa2f7'";
        let dark = Palette::from_omarchy_colors(source, false).unwrap();
        let light = Palette::from_omarchy_colors(source, true).unwrap();
        assert!(dark.dark);
        assert!(!light.dark);
        assert_eq!(light.accent, Color32::from_rgb(0x7a, 0xa2, 0xf7));
    }

    #[test]
    fn rejects_incomplete_palette() {
        assert!(Palette::from_omarchy_colors("background = '#aaa'", false).is_none());
    }
}
