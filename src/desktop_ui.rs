use gpui::{
    AnyElement, Div, ElementId, FontWeight, IntoElement, Rgba, Stateful, WindowAppearance, div,
    prelude::*, px,
};

use gpui::{Image, ImageFormat, img};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app_settings::ColorTheme;

/// Whether the light palette is active. Every color in the UI is written in the dark
/// palette and drawn through [`rgb`], [`rgba`] or [`tone`], which swap in its light
/// counterpart while this is set.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// Resolves the user's theme against the system appearance; call before rendering.
pub(crate) fn apply_theme(theme: ColorTheme, system: WindowAppearance) {
    let light = match theme {
        ColorTheme::Light => true,
        ColorTheme::Dark => false,
        ColorTheme::Auto => matches!(
            system,
            WindowAppearance::Light | WindowAppearance::VibrantLight
        ),
    };
    LIGHT.store(light, Ordering::Relaxed);
}

pub(crate) fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

/// An opaque color (0xRRGGBB) in the active theme.
pub(crate) fn rgb(hex: u32) -> Rgba {
    gpui::rgb(themed(hex))
}

/// A color with alpha (0xRRGGBBAA) in the active theme. Translucent white, which
/// lightens a dark surface, becomes translucent black on a light one.
pub(crate) fn rgba(hex: u32) -> Rgba {
    let (color, alpha) = (hex >> 8, hex & 0xff);
    let color = if is_light() && color == 0xffffff && alpha < 0xff {
        0x000000
    } else {
        themed(color)
    };
    gpui::rgba((color << 8) | alpha)
}

fn themed(hex: u32) -> u32 {
    if is_light() { light_variant(hex) } else { hex }
}

/// The light-palette counterpart of a dark-palette color. Named colors map by hand;
/// other grays mirror around the middle; accents and tints stay as they are.
pub(crate) fn light_variant(hex: u32) -> u32 {
    match hex {
        // Backgrounds: window, sidebar, sheets.
        0x1c1c1e => 0xf5f5f7,
        0x19191c => 0xebebf0,
        0x22212f => 0xf0f0f5,
        0x18181b => 0xe9e9ee,
        0x1f1f22 => 0xfafafc,
        0x111113 => 0xe6e6eb,
        0x1d1d21 => 0xf3f3f6,
        0x09090a => 0xdedee3,
        // Cards, rows and their states.
        0x242427 => 0xffffff,
        0x2c2c30 | 0x2c2c2f => 0xf2f2f5,
        0x2a2a2f => 0xededf1,
        0x36363c => 0xe5e5ea,
        0x303036 => 0xe6e6eb,
        0x37373d => 0xdedee4,
        0x323238 => 0xdedee3,
        0x2f2f36 => 0xdcdce3,
        0x3b3b44 => 0xcdcdd6,
        0x25252b => 0xe3e3e9,
        // Tinted surfaces.
        0x1a2a44 => 0xe3eefc,
        0x1f2a3d => 0xe7effb,
        0x1b2c47 => 0xdce8fb,
        0x0e1522 => 0xeef3fb,
        0x15291b => 0xe2f5e7,
        0x2a5a3a => 0xcdebd6,
        0x2e1718 => 0xfde9e9,
        0x6b2a2c => 0xefb3b1,
        // Text.
        0xf5f5f7 => 0x1d1d1f,
        0xd1d1d6 => 0x3a3a3c,
        0x98989d => 0x6e6e73,
        0x8e8e96 => 0x6e6e78,
        0x6e6e73 => 0x8e8e93,
        // Accents that need more contrast on white.
        0x64b0ff => 0x0a6ed1,
        0x30d158 => 0x1f9d48,
        0xff6961 => 0xd70015,
        _ => mirrored_gray(hex),
    }
}

/// Mirrors a near-neutral gray: dark fills become light fills (raised ones a touch
/// darker, like macOS), light text becomes dark text. Colors pass through.
fn mirrored_gray(hex: u32) -> u32 {
    let channels = [(hex >> 16) & 0xff, (hex >> 8) & 0xff, hex & 0xff];
    let (low, high) = (
        *channels.iter().min().unwrap(),
        *channels.iter().max().unwrap(),
    );
    if high - low > 0x14 || hex == 0xffffff || hex == 0x000000 {
        return hex;
    }
    let level = if high <= 0x1c {
        0xf5 - (0x1c - high) * 2
    } else if high <= 0x24 {
        0xff
    } else if high < 0x70 {
        (0xff - (high - 0x24) * 6 / 5).max(0xc4)
    } else {
        (0xff + 0x08 - high).max(0x1d)
    };
    (level << 16) | (level << 8) | level
}

#[derive(Clone, Copy)]
pub(crate) enum NavigationIcon {
    Activity,
    Appearance,
    Commands,
    Dictionary,
    General,
    History,
    HudLab,
    Meetings,
    Modes,
    Profile,
    Settings,
    Sounds,
    Typing,
}

impl NavigationIcon {
    /// Hello's own line icons: 24-unit strokes, drawn in white.
    fn line_icon(self) -> &'static str {
        match self {
            // Sliders.
            Self::General => {
                r#"<path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>"#
            }
            // Microphone.
            Self::Settings => {
                r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M9 21h6"/>"#
            }
            // Half-filled circle.
            Self::Appearance => {
                r##"<circle cx="12" cy="12" r="8.5"/><path d="M12 3.5a8.5 8.5 0 0 1 0 17z" fill="#ffffff"/>"##
            }
            // Speaker with waves.
            Self::Sounds => {
                r#"<path d="M11 5 6.5 8.5H4v7h2.5L11 19z"/><path d="M15 9a4.5 4.5 0 0 1 0 6M18 6.5a8 8 0 0 1 0 11"/>"#
            }
            // Stacked layers.
            Self::Modes => {
                r#"<path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z"/><path d="m3.5 12.5 8.5 4.5 8.5-4.5"/>"#
            }
            // Terminal prompt.
            Self::Commands => {
                r#"<rect x="3" y="4.5" width="18" height="15" rx="3.5"/><path d="m7.5 9.5 2.5 2.5-2.5 2.5M12.5 14.5h4"/>"#
            }
            // Keyboard.
            Self::Typing => {
                r#"<rect x="2.5" y="6" width="19" height="12" rx="3"/><path d="M6.5 10h1M10 10h1M13.5 10h1M17 10h.5M7.5 14h9"/>"#
            }
            // Open book.
            Self::Dictionary => {
                r#"<path d="M12 6.5c-1.6-1.4-3.8-2-6.5-2H4v13h1.5c2.7 0 4.9.6 6.5 2 1.6-1.4 3.8-2 6.5-2H20v-13h-1.5c-2.7 0-4.9.6-6.5 2z"/><path d="M12 6.5v13"/>"#
            }
            // Clock.
            Self::History => r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#,
            // Person.
            Self::Profile => {
                r#"<circle cx="12" cy="8.5" r="3.5"/><path d="M5 20a7 7 0 0 1 14 0"/>"#
            }
            // Calendar.
            Self::Meetings => {
                r#"<rect x="3.5" y="5" width="17" height="15" rx="3"/><path d="M3.5 10h17M8.5 3v4M15.5 3v4"/>"#
            }
            // Pulse line.
            Self::Activity => r#"<path d="M3 12h4l2.5-6 5 12 2.5-6h4"/>"#,
            // Flask.
            Self::HudLab => {
                r#"<path d="M9 3.5h6M10 3.5v5.5L4.8 18.6A1.6 1.6 0 0 0 6.2 21h11.6a1.6 1.6 0 0 0 1.4-2.4L14 9V3.5"/>"#
            }
        }
    }
}

/// Rendered from SVG at 4x its 24-unit grid so the strokes stay crisp on
/// Retina displays when drawn at sidebar size.
fn navigation_icon(icon: NavigationIcon, selected: bool) -> AnyElement {
    let ink = if is_light() { "#1d1d1f" } else { "#ffffff" };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">{}</svg>"##,
        icon.line_icon()
    )
    .replace("#ffffff", ink);
    div()
        .size(px(17.0))
        .opacity(if selected { 1.0 } else { 0.72 })
        .child(
            img(Arc::new(Image::from_bytes(
                ImageFormat::Svg,
                svg.into_bytes(),
            )))
            .size(px(17.0)),
        )
        .into_any_element()
}

#[cfg(target_os = "macos")]
fn disclosure_chevron() -> AnyElement {
    gpui_symbols::Icon::new("chevron.down")
        .size(px(10.0))
        .color(rgb(MUTED))
        .weight(gpui_symbols::SymbolWeight::Medium)
        .rendering_mode(gpui_symbols::RenderingMode::Monochrome)
        .into_any_element()
}

#[cfg(target_os = "macos")]
fn plus_icon() -> AnyElement {
    gpui_symbols::Icon::new("plus")
        .size(px(11.0))
        .color(rgb(TEXT_SOFT))
        .weight(gpui_symbols::SymbolWeight::Semibold)
        .rendering_mode(gpui_symbols::RenderingMode::Monochrome)
        .into_any_element()
}

pub(crate) const SIDEBAR_WIDTH: f32 = 220.0;
/// Secondary text on the glass sidebar: translucent white reads on any backdrop.
pub(crate) const SIDEBAR_SECONDARY: u32 = 0x8e8e96ff;

/// A color with alpha (0xRRGGBBAA). Fills inside cards are translucent white
/// so they layer over whatever solid surface holds them. Draw with [`tone`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tone(pub(crate) u32);

pub(crate) fn tone(tone: Tone) -> Rgba {
    rgba(tone.0)
}

/// The window behind the sidebar.
pub(crate) const WINDOW: Tone = Tone(0x1c1c1eff);
/// The pane content beside the sidebar.
pub(crate) const CONTENT: Tone = Tone(0x1c1c1eff);
/// A recessed list column inside a sheet.
pub(crate) const SIDEBAR: Tone = Tone(0x19191cff);
/// The sidebar: solid, with a faint indigo tint at the top.
const SIDEBAR_TOP: u32 = 0x22212f;
const SIDEBAR_BOTTOM: u32 = 0x18181b;
const NAV_SELECTED: u32 = 0x2f2f36;
const NAV_SELECTED_EDGE: u32 = 0x3b3b44;
const NAV_HOVER: u32 = 0x25252b;
/// Modal sheets such as the model picker: solid so nothing bleeds through.
pub(crate) const SHEET: u32 = 0x1f1f22;
/// Recessed wells for controls inside cards.
pub(crate) const CANVAS: Tone = Tone(0x18181bff);
/// Grouped cards. Every surface is opaque so nothing behind shows through.
pub(crate) const SURFACE: Tone = Tone(0x242427ff);
pub(crate) const SURFACE_HOVER: Tone = Tone(0x2c2c30ff);
pub(crate) const SURFACE_SELECTED: Tone = Tone(0x36363cff);
const SEGMENTED_ITEM_HOVER: Tone = Tone(0x2a2a2fff);
pub(crate) const LINE: Tone = Tone(0x323238ff);
pub(crate) const ACCENT_SURFACE: Tone = Tone(0x1a2a44ff);
/// Menus and pickers float above content, so they stay opaque for legibility.
pub(crate) const POPOVER: u32 = 0x2c2c2f;
pub(crate) const ACCENT: u32 = 0x0a84ff;
/// Text on an accent fill; white in both themes.
pub(crate) const ON_ACCENT: u32 = 0xffffff;
pub(crate) const ACCENT_SOFT: u32 = 0x64b0ff;
pub(crate) const POSITIVE: u32 = 0x30d158;
pub(crate) const POSITIVE_SURFACE: u32 = 0x15291b;
pub(crate) const TEXT: u32 = 0xf5f5f7;
pub(crate) const TEXT_SOFT: u32 = 0xd1d1d6;
pub(crate) const MUTED: u32 = 0x98989d;
pub(crate) const FAINT: u32 = 0x6e6e73;
pub(crate) const NEGATIVE: u32 = 0xff6961;
pub(crate) const DANGER_SURFACE: u32 = 0x2e1718;
pub(crate) const DANGER_BORDER: u32 = 0x6b2a2c;

pub(crate) const CONTROL_HEIGHT: f32 = 32.0;
pub(crate) const TEXT_INPUT_HEIGHT: f32 = 34.0;
pub(crate) const MULTILINE_INPUT_HEIGHT: f32 = 76.0;
pub(crate) const PANEL_RADIUS: f32 = 14.0;
pub(crate) const COMPACT_PANEL_HEADER_HEIGHT: f32 = 38.0;
pub(crate) const SECTION_GAP: f32 = 8.0;

pub(crate) fn window_frame() -> Div {
    div()
        .size_full()
        .relative()
        .overflow_hidden()
        .flex()
        .bg(tone(WINDOW))
        .text_color(rgb(TEXT))
}

/// The sidebar runs the full height of the window with the traffic lights
/// at its top, like Finder and System Settings.
pub(crate) fn sidebar_frame() -> Div {
    div()
        .flex_none()
        .border_r_1()
        .border_color(rgb(0x2c2c32))
        .bg(gpui::linear_gradient(
            180.0,
            gpui::linear_color_stop(rgb(SIDEBAR_TOP), 0.0),
            gpui::linear_color_stop(rgb(SIDEBAR_BOTTOM), 0.55),
        ))
}

pub(crate) fn navigation_item(icon: NavigationIcon, selected: bool) -> Div {
    div()
        .h(px(32.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(9.0))
        .border_1()
        .text_size(px(13.0))
        .font_weight(if selected {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(if selected { rgb(TEXT) } else { rgb(TEXT_SOFT) })
        .when(selected, |item| {
            item.bg(rgb(NAV_SELECTED))
                .border_color(rgb(NAV_SELECTED_EDGE))
        })
        .when(!selected, |item| {
            item.border_color(rgba(0x00000000))
                .hover(|item| item.bg(rgb(NAV_HOVER)))
        })
        .child(
            div()
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(navigation_icon(icon, selected)),
        )
}

/// The one content width every pane is bounded to. Headers and bodies share
/// it; panes must not introduce their own content widths.
pub(crate) const PANE_CONTENT_WIDTH: f32 = 940.0;

/// The one fixed list-column width every list+detail pane uses.
pub(crate) const PANE_LIST_WIDTH: f32 = 320.0;

/// The one list column of a list+detail pane: fixed to [`PANE_LIST_WIDTH`],
/// scrolling, with the pane's empty notice and load error ahead of its rows.
/// Callers decide when `empty` applies and append their rows.
pub(crate) fn pane_list(
    id: impl Into<ElementId>,
    empty: Option<&'static str>,
    error_title: &'static str,
    error: Option<String>,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(PANE_LIST_WIDTH))
        .h_full()
        .flex_none()
        .overflow_y_scroll()
        .when_some(empty, |list, message| list.child(empty_message(message)))
        .when_some(error, |list, error| {
            list.child(error_message(error_title, error))
        })
}

const LIST_ROW_SELECTED: Tone = Tone(0x303036ff);
const LIST_ROW_SELECTED_HOVER: Tone = Tone(0x37373dff);

/// A rounded, selectable row inside a list column.
pub(crate) fn list_row(selected: bool) -> Div {
    div()
        .w_full()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .rounded(px(10.0))
        .when(selected, |row| {
            row.bg(tone(LIST_ROW_SELECTED))
                .hover(|row| row.bg(tone(LIST_ROW_SELECTED_HOVER)))
        })
        .when(!selected, |row| {
            row.hover(|row| row.bg(tone(SURFACE_HOVER)))
        })
}

pub(crate) fn pane_header(title: &'static str) -> AnyElement {
    pane_header_with_action(title, None)
}

/// The one pane header: title left, optional action right, bounded to
/// [`PANE_CONTENT_WIDTH`] like the body below it.
pub(crate) fn pane_header_with_action(
    title: &'static str,
    action: Option<AnyElement>,
) -> AnyElement {
    div()
        .h(px(70.0))
        .px_8()
        .flex_none()
        .flex()
        .justify_center()
        .child(
            div()
                .h_full()
                .w_full()
                .max_w(px(PANE_CONTENT_WIDTH))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(26.0))
                        .font_weight(FontWeight::BOLD)
                        .child(title),
                )
                .when_some(action, |header, action| header.child(action)),
        )
        .into_any_element()
}

/// The one pane body: fills the space under the header and centers its
/// children. Put the pane's content column inside [`pane_content`].
pub(crate) fn pane_body() -> Div {
    div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .flex()
        .justify_center()
}

/// The one pane-header action button: a bordered 30-point chip. Every
/// clickable header action renders this.
pub(crate) fn header_button(label: impl IntoElement) -> Div {
    div()
        .h(px(30.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(0x3a3a40))
        .bg(rgb(0x2c2c31))
        .text_size(px(12.0))
        .text_color(rgb(TEXT_SOFT))
        .hover(|button| button.bg(rgb(0x36363c)).text_color(rgb(TEXT)))
        .child(label)
}

/// The one pane content column, bounded to [`PANE_CONTENT_WIDTH`].
pub(crate) fn pane_content() -> Div {
    div()
        .w_full()
        .max_w(px(PANE_CONTENT_WIDTH))
        .min_h(px(0.0))
        .flex()
        .flex_col()
}

pub(crate) fn section_label(label: &'static str) -> AnyElement {
    div()
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(FAINT))
        .child(label)
        .into_any_element()
}

pub(crate) fn listener_status(
    status: impl IntoElement,
    device: impl IntoElement,
    active: bool,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(8.0))
                .flex_none()
                .rounded_full()
                .bg(if active { rgb(POSITIVE) } else { rgb(FAINT) }),
        )
        .child(
            div()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child(status),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(FAINT))
                        .child(device),
                ),
        )
}

pub(crate) fn hotkey_keycaps(parts: Vec<String>, opacity: f32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(3.0))
        .opacity(opacity)
        .children(parts.into_iter().map(|part| {
            let (side, label) = split_sided_keycap(&part);
            div()
                .min_w(px(34.0))
                .h(px(24.0))
                .px_2()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(0x45454c))
                .bg(rgb(0x2e2e34))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(TEXT))
                .when_some(side, |keycap, side| {
                    keycap.child(
                        div()
                            .absolute()
                            .top(px(2.0))
                            .left(px(4.0))
                            .text_size(px(7.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(FAINT))
                            .child(side),
                    )
                })
                .child(label)
        }))
        .into_any_element()
}

fn split_sided_keycap(part: &str) -> (Option<&'static str>, String) {
    match part {
        "L⌃" => (Some("L"), "⌃".into()),
        "R⌃" => (Some("R"), "⌃".into()),
        "L⌥" => (Some("L"), "⌥".into()),
        "R⌥" => (Some("R"), "⌥".into()),
        "L⇧" => (Some("L"), "⇧".into()),
        "R⇧" => (Some("R"), "⇧".into()),
        "L⌘" => (Some("L"), "⌘".into()),
        "R⌘" => (Some("R"), "⌘".into()),
        _ => (None, part.to_string()),
    }
}

/// An Apple-style switch: a capsule track with a round knob.
pub(crate) fn toggle(position: f32) -> AnyElement {
    let color_position = position.clamp(0.0, 1.0);
    div()
        .w(px(36.0))
        .h(px(20.0))
        .p(px(2.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded_full()
        .bg(mix_color(rgb(0x3a3a40), rgb(ACCENT), color_position))
        .child(
            div()
                .ml(px(16.0 * position.clamp(-0.04, 1.04)))
                .size(px(16.0))
                .rounded_full()
                .bg(rgb(0xffffff))
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0.0, 0.0, 0.0, 0.3),
                    offset: gpui::point(px(0.0), px(1.0)),
                    blur_radius: px(3.0),
                    spread_radius: px(0.0),
                }]),
        )
        .into_any_element()
}

/// Group headings read like Apple's grouped lists: title case, not all caps.
fn heading_case(label: &str) -> String {
    label
        .split(' ')
        .map(|word| {
            let lower = word.to_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn settings_section_label(label: &'static str) -> AnyElement {
    div()
        .pt_7()
        .pb_2()
        .px_1()
        .text_size(px(13.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT_SOFT))
        .child(heading_case(label))
        .into_any_element()
}

pub(crate) fn settings_copy(title: &'static str, description: impl IntoElement) -> AnyElement {
    div()
        .debug_selector(|| "settings-copy".into())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(description),
        )
        .into_any_element()
}

pub(crate) fn compact_button(label: impl IntoElement) -> Div {
    div()
        .h(px(30.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .text_size(px(12.0))
        .text_color(rgb(TEXT_SOFT))
        .hover(|button| button.bg(tone(SURFACE_HOVER)))
        .child(label)
}

pub(crate) fn compact_plus_button() -> Div {
    compact_button(plus_icon())
        .size(px(28.0))
        .px_0()
        .justify_center()
}

pub(crate) fn compact_header_plus_button() -> Div {
    compact_plus_button().mr(px(-7.0))
}

pub(crate) fn compact_panel() -> Div {
    div()
        .w_full()
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(tone(LINE))
        .bg(tone(SURFACE))
        .overflow_hidden()
}

pub(crate) fn compact_panel_header(title: impl IntoElement, action: Option<AnyElement>) -> Div {
    div()
        .min_h(px(COMPACT_PANEL_HEADER_HEIGHT))
        .px_4()
        .py(px(10.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(tone(LINE))
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(title),
        )
        .when_some(action, |header, action| header.child(action))
}

pub(crate) const SOUND_VOLUME_STEPS: [(&str, f32); 5] = [
    ("Off", 0.0),
    ("25%", 0.25),
    ("50%", 0.5),
    ("75%", 0.75),
    ("100%", 1.0),
];

pub(crate) fn canvas_button() -> Div {
    div()
        .flex()
        .items_center()
        .rounded(px(8.0))
        .border_1()
        .border_color(tone(LINE))
        .bg(tone(CANVAS))
        .text_size(px(11.0))
        .text_color(rgb(TEXT_SOFT))
        .hover(|button| button.bg(tone(SURFACE_HOVER)).text_color(rgb(TEXT)))
}

pub(crate) fn disclosure_button(label: impl IntoElement) -> Div {
    canvas_button()
        .w(px(220.0))
        .h(px(CONTROL_HEIGHT))
        .px_3()
        .flex_none()
        .gap_2()
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .overflow_x_hidden()
                .truncate()
                .child(label),
        )
        .child(
            div()
                .size(px(10.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(disclosure_chevron()),
        )
}

pub(crate) fn settings_row(
    title: &'static str,
    description: impl IntoElement,
    control: impl IntoElement,
) -> Div {
    div()
        .w_full()
        .min_h(px(72.0))
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .border_b_1()
        .border_color(tone(LINE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(settings_copy(title, description)),
        )
        .child(control)
}

pub(crate) fn settings_panel() -> Div {
    compact_panel().relative()
}

pub(crate) fn segmented_control() -> Div {
    div()
        .h(px(CONTROL_HEIGHT))
        .p(px(2.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(0x2e2e34))
        .bg(rgb(0x18181b))
}

pub(crate) fn segmented_item(selected: bool) -> Div {
    div()
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .text_size(px(11.5))
        .font_weight(if selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(if selected { rgb(TEXT) } else { rgb(MUTED) })
        .when(selected, |item| item.bg(tone(SURFACE_SELECTED)))
        .when(!selected, |item| {
            item.hover(|item| {
                item.bg(tone(SEGMENTED_ITEM_HOVER))
                    .text_color(rgb(TEXT_SOFT))
            })
        })
}

/// A [`segmented_control`] whose selection indicator slides between items.
/// `position` is a fractional item index and `widths` lists every item's
/// width in order; append the items with [`sliding_segmented_item`].
pub(crate) fn sliding_segmented_control(position: f32, widths: &[f32]) -> Div {
    let (left, width) = segmented_geometry(position, widths);
    segmented_control().relative().child(
        div()
            .absolute()
            .left(px(left))
            .top(px(2.0))
            .w(px(width))
            .h(px(28.0))
            .rounded_full()
            .border_1()
            .border_color(rgb(0x4a4a52))
            .bg(rgb(0x3a3a41))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0.0, 0.0, 0.0, 0.25),
                offset: gpui::point(px(0.0), px(1.0)),
                blur_radius: px(4.0),
                spread_radius: px(0.0),
            }]),
    )
}

/// A fixed-width, centered item that stays transparent so the sliding
/// indicator beneath it shows through.
pub(crate) fn sliding_segmented_item(width: f32, selected: bool) -> Div {
    segmented_item(selected)
        .w(px(width))
        .px(px(0.0))
        .justify_center()
        .bg(rgba(0x00000000))
}

fn segmented_geometry(position: f32, widths: &[f32]) -> (f32, f32) {
    let last = widths.len().saturating_sub(1);
    let position = position.clamp(0.0, last as f32);
    let lower = (position.floor() as usize).min(last);
    let upper = (lower + 1).min(last);
    let progress = position - lower as f32;
    let left = |index: usize| 2.0 + widths[..index].iter().sum::<f32>();
    (
        left(lower) + (left(upper) - left(lower)) * progress,
        widths[lower] + (widths[upper] - widths[lower]) * progress,
    )
}

pub(crate) fn mix_color(from: Rgba, to: Rgba, position: f32) -> Rgba {
    let position = position.clamp(0.0, 1.0);
    Rgba {
        r: from.r + (to.r - from.r) * position,
        g: from.g + (to.g - from.g) * position,
        b: from.b + (to.b - from.b) * position,
        a: from.a + (to.a - from.a) * position,
    }
}

pub(crate) fn empty_message(message: &'static str) -> AnyElement {
    div()
        .p_6()
        .text_size(px(12.0))
        .text_color(rgb(FAINT))
        .child(message)
        .into_any_element()
}

pub(crate) fn error_message(message: &'static str, error: String) -> AnyElement {
    div()
        .p_6()
        .flex()
        .flex_col()
        .gap_2()
        .text_size(px(12.0))
        .text_color(rgb(NEGATIVE))
        .child(message)
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(rgb(MUTED))
                .child(error),
        )
        .into_any_element()
}

/// Line glyphs for page content: badges, buttons and chips. Same 24-unit
/// stroke grid as the sidebar icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    Apps,
    Bolt,
    Book,
    Clock,
    Copy,
    Cursor,
    Globe,
    Keyboard,
    Layers,
    Mic,
    Pencil,
    People,
    Plus,
    Power,
    Replace,
    Sparkles,
    Speed,
    Star,
    Text,
    Trash,
    Wand,
}

impl Glyph {
    fn paths(self) -> &'static str {
        match self {
            Self::Apps => {
                r#"<rect x="4" y="4" width="6.5" height="6.5" rx="2"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="2"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="2"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="2"/>"#
            }
            Self::Bolt => r#"<path d="M13 3 5 13.5h6L10 21l8-10.5h-6z"/>"#,
            Self::Book => {
                r#"<path d="M12 6.5c-1.6-1.4-3.8-2-6.5-2H4v13h1.5c2.7 0 4.9.6 6.5 2 1.6-1.4 3.8-2 6.5-2H20v-13h-1.5c-2.7 0-4.9.6-6.5 2z"/><path d="M12 6.5v13"/>"#
            }
            Self::Clock => r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#,
            Self::Copy => {
                r#"<rect x="8.5" y="8.5" width="11.5" height="11.5" rx="2.5"/><path d="M15.5 8.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7.5a2 2 0 0 0 2 2h2.5"/>"#
            }
            Self::Cursor => r#"<path d="m5 4 14 6-6 2-2 6z"/><path d="m13 13 5 5"/>"#,
            Self::Globe => {
                r#"<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.5 2.6 3.5 5.4 3.5 8.5s-1 5.9-3.5 8.5c-2.5-2.6-3.5-5.4-3.5-8.5s1-5.9 3.5-8.5z"/>"#
            }
            Self::Keyboard => {
                r#"<rect x="2.5" y="6" width="19" height="12" rx="3"/><path d="M6.5 10h1M10 10h1M13.5 10h1M17 10h.5M7.5 14h9"/>"#
            }
            Self::Layers => {
                r#"<path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z"/><path d="m3.5 12.5 8.5 4.5 8.5-4.5"/>"#
            }
            Self::Mic => {
                r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21"/>"#
            }
            Self::Pencil => {
                r#"<path d="M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16z"/><path d="m13.5 6.5 4 4"/>"#
            }
            Self::People => {
                r#"<circle cx="9" cy="8" r="3.2"/><path d="M3 19.5a6 6 0 0 1 12 0"/><path d="M15.5 5a3 3 0 0 1 0 6M17 14a5.5 5.5 0 0 1 4 5.5"/>"#
            }
            Self::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
            Self::Power => r#"<path d="M12 3.5V11"/><path d="M7 6.5a7.5 7.5 0 1 0 10 0"/>"#,
            Self::Replace => r#"<path d="M4 8h13l-3.5-3.5M20 16H7l3.5 3.5"/>"#,
            Self::Sparkles => {
                r#"<path d="M11 3.5 12.6 8.4 17.5 10l-4.9 1.6L11 16.5l-1.6-4.9L4.5 10l4.9-1.6z"/><path d="m18 14.5.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"/>"#
            }
            Self::Speed => {
                r#"<path d="M4.5 17a8.5 8.5 0 1 1 15 0"/><path d="m12 13 4-5"/><circle cx="12" cy="13.5" r="1"/>"#
            }
            Self::Star => {
                r#"<path d="m12 3.5 2.6 5.4 5.9.8-4.3 4.1 1.1 5.9L12 16.9l-5.3 2.8 1.1-5.9-4.3-4.1 5.9-.8z"/>"#
            }
            Self::Text => r#"<path d="M4 6h16M4 11h16M4 16h10"/>"#,
            Self::Trash => {
                r#"<path d="M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 12.5h9l1-12.5M10 11v5M14 11v5"/>"#
            }
            Self::Wand => {
                r#"<path d="m4 20 11-11M13 7l4 4"/><path d="M17 3v3M15.5 4.5h3M20 8v2M19 9h2"/>"#
            }
        }
    }
}

/// Accent tints for page badges. Each badge pairs a white glyph with a dark
/// tinted square so colour reads as identity, not decoration.
pub(crate) const TINT_BLUE: u32 = 0x0a84ff;
pub(crate) const TINT_PURPLE: u32 = 0xbf5af2;
pub(crate) const TINT_GREEN: u32 = 0x30d158;
pub(crate) const TINT_ORANGE: u32 = 0xff9f0a;
pub(crate) const TINT_PINK: u32 = 0xff375f;
pub(crate) const TINT_TEAL: u32 = 0x40c8e0;
pub(crate) const TINT_INDIGO: u32 = 0x5e5ce6;
pub(crate) const TINT_GRAY: u32 = 0x8e8e93;

/// `tint` laid over the card surface at `amount`, as an opaque colour.
pub(crate) fn tinted(tint: u32, amount: f32) -> Rgba {
    mix_color(tone(SURFACE), rgb(tint), amount)
}

/// A crisp line glyph drawn in `color` (0xRRGGBB).
pub(crate) fn glyph(glyph: Glyph, size: f32, color: u32) -> AnyElement {
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 24 24" fill="none" stroke="#{color:06x}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{}</svg>"##,
        glyph.paths(),
        color = themed(color)
    );
    img(Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        svg.into_bytes(),
    )))
    .size(px(size))
    .flex_none()
    .into_any_element()
}

/// A rounded tinted square holding a white glyph.
pub(crate) fn glyph_badge(icon: Glyph, size: f32, tint: u32) -> AnyElement {
    // On white cards a pale tint would wash out the white glyph, so the
    // light theme uses a fuller fill, like the icons in System Settings.
    let (border, top, bottom) = if is_light() {
        (0.9, 0.92, 0.78)
    } else {
        (0.45, 0.42, 0.26)
    };
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * 0.3))
        .border_1()
        .border_color(tinted(tint, border))
        .bg(gpui::linear_gradient(
            180.0,
            gpui::linear_color_stop(tinted(tint, top), 0.0),
            gpui::linear_color_stop(tinted(tint, bottom), 1.0),
        ))
        .child(glyph(icon, size * 0.52, 0xffffff))
        .into_any_element()
}

/// A small read-only capsule for metadata (app names, durations, aliases).
pub(crate) fn chip(label: impl IntoElement) -> Div {
    chip_frame().px(px(10.0)).child(label)
}

/// A metadata capsule led by an icon.
pub(crate) fn icon_chip(icon: impl IntoElement, label: impl IntoElement) -> Div {
    chip_frame()
        .pl(px(7.0))
        .pr(px(10.0))
        .child(icon)
        .child(label)
}

fn chip_frame() -> Div {
    div()
        .h(px(24.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .border_1()
        .border_color(rgb(0x37373e))
        .bg(rgb(0x2b2b30))
        .text_size(px(11.0))
        .text_color(rgb(TEXT_SOFT))
}

/// A header action with a leading glyph, styled like [`header_button`].
pub(crate) fn glyph_button(icon: Glyph, label: impl IntoElement) -> Div {
    div()
        .h(px(30.0))
        .pl(px(11.0))
        .pr(px(14.0))
        .flex()
        .items_center()
        .gap(px(7.0))
        .rounded_full()
        .border_1()
        .border_color(rgb(0x3a3a40))
        .bg(rgb(0x2c2c31))
        .text_size(px(12.0))
        .text_color(rgb(TEXT_SOFT))
        .hover(|button| button.bg(rgb(0x36363c)).text_color(rgb(TEXT)))
        .child(glyph(icon, 13.0, 0xd1d1d6))
        .child(label)
}

/// A pane header with a one-line explanation under the title.
pub(crate) fn pane_header_with_subtitle(
    title: &'static str,
    subtitle: &'static str,
    action: Option<AnyElement>,
) -> AnyElement {
    div()
        .h(px(84.0))
        .px_8()
        .flex_none()
        .flex()
        .justify_center()
        .child(
            div()
                .h_full()
                .w_full()
                .max_w(px(PANE_CONTENT_WIDTH))
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_size(px(26.0))
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(rgb(MUTED))
                                .child(subtitle),
                        ),
                )
                .when_some(action, |header, action| header.child(action)),
        )
        .into_any_element()
}

/// A panel title: tinted badge, title and one line of explanation.
pub(crate) fn panel_title(
    icon: Glyph,
    tint: u32,
    title: &'static str,
    subtitle: &'static str,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(glyph_badge(icon, 28.0, tint))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(rgb(MUTED))
                        .child(subtitle),
                ),
        )
        .into_any_element()
}

/// A section heading inside list+detail panes: title case with an optional
/// count.
pub(crate) fn group_heading(label: impl Into<String>, count: Option<usize>) -> Div {
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(MUTED))
                .child(heading_case(&label.into())),
        )
        .when_some(count, |heading, count| {
            heading.child(
                div()
                    .px(px(6.0))
                    .rounded_full()
                    .bg(rgb(0x2e2e34))
                    .text_size(px(10.0))
                    .text_color(rgb(FAINT))
                    .child(count.to_string()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{segmented_geometry, split_sided_keycap};

    #[test]
    fn side_badges_only_apply_to_sided_modifiers() {
        assert_eq!(split_sided_keycap("L⌥"), (Some("L"), "⌥".into()));
        assert_eq!(split_sided_keycap("R⌘"), (Some("R"), "⌘".into()));
        assert_eq!(split_sided_keycap("Return"), (None, "Return".into()));
    }

    #[test]
    fn segmented_indicator_interpolates_between_uneven_items() {
        let widths = [50.0, 90.0, 80.0];
        assert_eq!(segmented_geometry(0.0, &widths), (2.0, 50.0));
        assert_eq!(segmented_geometry(1.0, &widths), (52.0, 90.0));
        assert_eq!(segmented_geometry(2.0, &widths), (142.0, 80.0));
        assert_eq!(segmented_geometry(0.5, &widths), (27.0, 70.0));
        assert_eq!(segmented_geometry(-1.0, &widths), (2.0, 50.0));
        assert_eq!(segmented_geometry(5.0, &widths), (142.0, 80.0));
        assert_eq!(segmented_geometry(2.5, &[34.0; 5]), (87.0, 34.0));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod layout_tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, Window, size};

    struct SettingsRow;

    impl Render for SettingsRow {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w_full().child(
                settings_row(
                    "Microphone mode",
                    "Default: keeps the microphone open for the fastest start. A short pre-roll helps catch the beginning of speech. Audio is not saved by default.",
                    div()
                        .debug_selector(|| "settings-control".into())
                        .w(px(220.0))
                        .h(px(CONTROL_HEIGHT))
                        .flex_none(),
                )
                .debug_selector(|| "settings-row".into()),
            )
        }
    }

    #[gpui::test]
    fn settings_row_wraps_copy_without_displacing_fixed_control(cx: &mut TestAppContext) {
        // Real GPUI/Taffy layout with deterministic test-platform text metrics.
        let (_, cx) = cx.add_window_view(|_, _| SettingsRow);
        let mut heights = Vec::new();
        for width in [940.0, 756.0, 576.0] {
            cx.simulate_resize(size(px(width), px(400.0)));
            cx.run_until_parked();
            let row = cx.debug_bounds("settings-row").unwrap();
            let copy = cx.debug_bounds("settings-copy").unwrap();
            let control = cx.debug_bounds("settings-control").unwrap();
            assert_eq!(row.size.width, px(width));
            assert_eq!(control.size.width, px(220.0));
            assert_eq!(control.size.height, px(CONTROL_HEIGHT));
            assert!(
                control.right() <= row.right(),
                "{width}: {control:?} outside {row:?}"
            );
            assert!(copy.left() >= row.left());
            assert!(
                copy.right() + px(16.0) <= control.left(),
                "{width}: gap between {copy:?} and {control:?} is below 16px"
            );
            assert!(copy.top() >= row.top() && copy.bottom() <= row.bottom());
            heights.push((row.size.height, copy.size.height));
        }
        assert!(
            heights[2].0 > heights[0].0,
            "narrow row must grow: {heights:?}"
        );
        assert!(
            heights[2].1 > heights[0].1,
            "narrow copy must wrap: {heights:?}"
        );
    }
}
