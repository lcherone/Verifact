//! Precision: Verifact's semantic native theme. Views consume roles rather than
//! hex values; GPUI Component retains ownership of control interactions.
use gpui::{
    App, Hsla, ParentElement, SharedString, Styled, Window, WindowAppearance, div, px, rgb,
};
use gpui_component::{Theme, ThemeMode};

pub const SIDEBAR_WIDTH: f32 = 232.;
pub const HEADER_HEIGHT: f32 = 96.;
pub const FONT_UI: &str = ".SystemUIFont";
pub const FONT_CODE: &str = "Menlo";
pub const BODY_SIZE: f32 = 13.;
pub const META_SIZE: f32 = 12.;
pub const SECTION_SIZE: f32 = 14.;
pub const TITLE_SIZE: f32 = 22.;
pub const CONTROL_HEIGHT: f32 = 32.;
pub const ICON_SIZE: f32 = 16.;
pub const RADIUS: f32 = 6.;
pub const GUTTER: f32 = 24.;
pub const SPACE_1: f32 = 4.;
pub const SPACE_2: f32 = 8.;
pub const SPACE_3: f32 = 12.;
pub const SPACE_4: f32 = 16.;
pub const SPACE_6: f32 = 24.;
pub const SPACE_8: f32 = 32.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    Dark,
    Light,
    System,
}

impl ThemePreference {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    /// Unknown persisted values safely return the product's dark-first default.
    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "system" => Self::System,
            _ => Self::Dark,
        }
    }

    pub fn resolved_dark(self, appearance: WindowAppearance) -> bool {
        match self {
            Self::Dark => true,
            Self::Light => false,
            Self::System => matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub canvas: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub secondary: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub selected: Hsla,
    pub hover: Hsla,
    pub focus: Hsla,
    pub success: Hsla,
    pub failure: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    /// Disabled foreground only; never use for ordinary metadata.
    pub disabled: Hsla,
    pub disabled_surface: Hsla,
}

impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        let color = |value| Hsla::from(rgb(value));
        if dark {
            Self {
                canvas: color(0x121a27),
                surface: color(0x1d2836),
                raised: color(0x263445),
                border: color(0x334155),
                border_strong: color(0x647b96),
                text: color(0xf1f5f9),
                secondary: color(0xb4c0d0),
                muted: color(0x94a3b8),
                accent: color(0x43d9b2),
                accent_text: color(0x10251f),
                selected: color(0x1c3d35),
                hover: color(0x2b3a4d),
                focus: color(0x43d9b2),
                success: color(0x62dbae),
                failure: color(0xff8b99),
                warning: color(0xe6be70),
                info: color(0x82b7ff),
                disabled: color(0x71839b),
                disabled_surface: color(0x1a2533),
            }
        } else {
            Self {
                canvas: color(0xf2f5f8),
                surface: color(0xffffff),
                raised: color(0xf7f9fc),
                border: color(0xcdd6e0),
                border_strong: color(0x71839a),
                text: color(0x172536),
                secondary: color(0x465970),
                muted: color(0x5a6d82),
                accent: color(0x087f67),
                accent_text: color(0xffffff),
                selected: color(0xe0f3ec),
                hover: color(0xeaf0f5),
                focus: color(0x087f67),
                success: color(0x127547),
                failure: color(0xb4233b),
                warning: color(0x8f5a0c),
                info: color(0x245dbb),
                disabled: color(0x8896a7),
                disabled_surface: color(0xedf1f5),
            }
        }
    }
}

/// Keep button labels on Precision colors: upstream Button 0.5.1 applies a
/// hardcoded red hover foreground to the parent. An explicitly colored child
/// prevents inheritance without replacing its focus/click/disabled behavior.
pub fn action_label(text: impl Into<SharedString>, palette: &Palette, primary: bool) -> gpui::Div {
    div()
        .text_color(if primary {
            palette.accent_text
        } else {
            palette.text
        })
        .child(text.into())
}

fn emphasize(mut color: Hsla, dark: bool, amount: f32) -> Hsla {
    color.l = (color.l + if dark { amount } else { -amount }).clamp(0., 1.);
    color
}

/// Apply after `gpui_component::init(cx)`. The application owns preference
/// persistence and system-appearance subscriptions; this adapter only themes.
pub fn apply(preference: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    let appearance = window
        .as_ref()
        .map(|window| window.appearance())
        .unwrap_or_else(|| cx.window_appearance());
    let dark = preference.resolved_dark(appearance);
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let palette = Palette::for_dark(dark);
    let theme = Theme::global_mut(cx);
    theme.font_family = FONT_UI.into();
    theme.font_size = px(BODY_SIZE);
    theme.mono_font_family = FONT_CODE.into();
    theme.mono_font_size = px(META_SIZE);
    theme.radius = px(RADIUS);
    theme.radius_lg = px(SPACE_2);
    theme.shadow = true;
    theme.tile_shadow = false;
    let colors = &mut theme.colors;
    colors.background = palette.canvas;
    colors.foreground = palette.text;
    colors.border = palette.border;
    colors.window_border = palette.border;
    colors.input = palette.border_strong;
    colors.caret = palette.accent;
    colors.ring = palette.focus;
    colors.accent = palette.hover;
    colors.accent_foreground = palette.text;
    colors.primary = palette.accent;
    colors.primary_hover = emphasize(palette.accent, dark, 0.04);
    colors.primary_active = emphasize(palette.accent, dark, 0.08);
    colors.primary_foreground = palette.accent_text;
    colors.secondary = palette.raised;
    colors.secondary_hover = palette.hover;
    colors.secondary_active = palette.selected;
    colors.secondary_foreground = palette.text;
    colors.muted = palette.disabled_surface;
    colors.muted_foreground = palette.muted;
    colors.selection = palette.selected;
    colors.popover = palette.raised;
    colors.popover_foreground = palette.text;
    colors.overlay = palette.canvas.opacity(0.76);
    colors.link = palette.accent;
    colors.link_hover = emphasize(palette.accent, dark, 0.04);
    colors.link_active = palette.accent;
    colors.accordion = palette.surface;
    colors.accordion_hover = palette.hover;
    colors.group_box = palette.surface;
    colors.group_box_foreground = palette.text;
    colors.description_list_label = palette.surface;
    colors.description_list_label_foreground = palette.secondary;
    colors.drag_border = palette.focus;
    colors.drop_target = palette.selected;
    colors.list = palette.surface;
    colors.list_active = palette.selected;
    colors.list_active_border = palette.focus;
    colors.list_even = palette.surface;
    colors.list_head = palette.raised;
    colors.list_hover = palette.hover;
    colors.table = palette.surface;
    // GPUI Component paints row selection over the cells, so this must be
    // translucent: an opaque fill hides the filename, framework and status.
    colors.table_active = palette.accent.opacity(0.12);
    colors.table_active_border = palette.focus;
    colors.table_even = palette.surface;
    colors.table_head = palette.raised;
    colors.table_head_foreground = palette.secondary;
    colors.table_hover = palette.hover;
    colors.table_row_border = palette.border;
    colors.sidebar = palette.surface;
    colors.sidebar_border = palette.border;
    colors.sidebar_foreground = palette.secondary;
    colors.sidebar_accent = palette.selected;
    colors.sidebar_accent_foreground = palette.text;
    colors.sidebar_primary = palette.accent;
    colors.sidebar_primary_foreground = palette.accent_text;
    colors.tab = palette.surface;
    colors.tab_active = palette.raised;
    colors.tab_active_foreground = palette.text;
    colors.tab_bar = palette.surface;
    colors.tab_bar_segmented = palette.canvas;
    colors.tab_foreground = palette.secondary;
    colors.title_bar = palette.canvas;
    colors.title_bar_border = palette.border;
    colors.tiles = palette.canvas;
    colors.skeleton = palette.disabled_surface;
    colors.progress_bar = palette.accent;
    colors.slider_bar = palette.border_strong;
    colors.slider_thumb = palette.accent;
    colors.switch = palette.border_strong;
    colors.switch_thumb = palette.surface;
    colors.scrollbar = palette.canvas;
    colors.scrollbar_thumb = palette.border_strong;
    colors.scrollbar_thumb_hover = palette.secondary;
    colors.success = palette.success;
    colors.success_hover = emphasize(palette.success, dark, 0.04);
    colors.success_active = emphasize(palette.success, dark, 0.08);
    colors.success_foreground = palette.accent_text;
    colors.danger = palette.failure;
    colors.danger_hover = emphasize(palette.failure, dark, 0.04);
    colors.danger_active = emphasize(palette.failure, dark, 0.08);
    colors.danger_foreground = palette.accent_text;
    colors.warning = palette.warning;
    colors.warning_hover = emphasize(palette.warning, dark, 0.04);
    colors.warning_active = emphasize(palette.warning, dark, 0.08);
    colors.warning_foreground = palette.accent_text;
    colors.info = palette.info;
    colors.info_hover = emphasize(palette.info, dark, 0.04);
    colors.info_active = emphasize(palette.info, dark, 0.08);
    colors.info_foreground = palette.accent_text;
    colors.bullish = palette.success;
    colors.bearish = palette.failure;
    colors.chart_1 = palette.accent;
    colors.chart_2 = palette.info;
    colors.chart_3 = palette.warning;
    colors.chart_4 = palette.secondary;
    colors.chart_5 = palette.failure;
    colors.red = palette.failure;
    colors.red_light = emphasize(palette.failure, dark, 0.08);
    colors.green = palette.success;
    colors.green_light = emphasize(palette.success, dark, 0.08);
    colors.blue = palette.info;
    colors.blue_light = emphasize(palette.info, dark, 0.08);
    colors.yellow = palette.warning;
    colors.yellow_light = emphasize(palette.warning, dark, 0.08);
    colors.magenta = palette.failure;
    colors.magenta_light = emphasize(palette.failure, dark, 0.08);
    colors.cyan = palette.accent;
    colors.cyan_light = emphasize(palette.accent, dark, 0.08);
    if let Some(window) = window {
        window.refresh();
    }
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(first: Hsla, second: Hsla) -> f32 {
        fn luminance(color: Hsla) -> f32 {
            let rgba = color.to_rgb();
            let linear = |channel: f32| {
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
        }
        let a = luminance(first);
        let b = luminance(second);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn theme_preferences_resolve_all_system_appearances() {
        for preference in [
            ThemePreference::Dark,
            ThemePreference::Light,
            ThemePreference::System,
        ] {
            assert_eq!(ThemePreference::parse(preference.as_str()), preference);
        }
        assert_eq!(ThemePreference::parse("obsolete"), ThemePreference::Dark);
        for appearance in [WindowAppearance::Dark, WindowAppearance::VibrantDark] {
            assert!(ThemePreference::System.resolved_dark(appearance));
            assert!(!ThemePreference::Light.resolved_dark(appearance));
        }
        for appearance in [WindowAppearance::Light, WindowAppearance::VibrantLight] {
            assert!(!ThemePreference::System.resolved_dark(appearance));
            assert!(ThemePreference::Dark.resolved_dark(appearance));
        }
    }

    #[test]
    fn ordinary_text_and_primary_controls_meet_contrast_targets() {
        for dark in [true, false] {
            let palette = Palette::for_dark(dark);
            for background in [
                palette.canvas,
                palette.surface,
                palette.raised,
                palette.hover,
                palette.selected,
            ] {
                for foreground in [palette.text, palette.secondary, palette.muted] {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "Text contrast below 4.5:1 in dark={dark}"
                    );
                }
            }
            for background in [
                palette.accent,
                emphasize(palette.accent, dark, 0.04),
                emphasize(palette.accent, dark, 0.08),
            ] {
                assert!(contrast(palette.accent_text, background) >= 4.5);
            }
            assert!(contrast(palette.border_strong, palette.surface) >= 3.);
            assert!(contrast(palette.focus, palette.surface) >= 3.);
        }
    }
}
