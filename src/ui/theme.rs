//! The app's look — fonts, palette, spacing and shape — installed once at startup.
//!
//! Panes read everything through `ui.style()` / `ui.visuals()`, so this is the only place
//! that knows the palette: the accent flows into chat bubbles, badges, links and cards via
//! `selection.bg_fill`, and both system themes get a tuned style.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, vec2};

/// Indigo accent; selection, links, badges and bubbles derive from it.
pub const ACCENT: Color32 = Color32::from_rgb(0x5a, 0x51, 0xe0);

/// macOS faces tried for proportional text, best first. egui's bundled fonts stay behind
/// them for glyphs they lack, and entirely where none of the files exist (CI, Linux).
const SYSTEM_FONT_FILES: [&str; 2] = [
    "/System/Library/Fonts/HelveticaNeue.ttc",
    "/System/Library/Fonts/SFNS.ttf",
];

/// A bold macOS face, a file and its index in it, for [`bold_family`].
const BOLD_FONT_FILE: (&str, u32) = ("/System/Library/Fonts/HelveticaNeue.ttc", 1);
const BOLD_FAMILY_NAME: &str = "bold";

/// macOS faces with arrows (`→`, `↓`) and dingbats (`✓`, `✗`) the text faces lack, tried last
/// for proportional and monospace text.
const SYMBOL_FONT_FILES: [&str; 2] = [
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
];

/// Installs the fonts, image loaders and both theme styles on `ctx`.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    super::art::install(ctx);
    ctx.set_style_of(egui::Theme::Dark, style(egui::Theme::Dark));
    ctx.set_style_of(egui::Theme::Light, style(egui::Theme::Light));
}

/// egui's default fonts with a system face put first for proportional text, a symbol face
/// last for both text families, plus the icon font.
pub fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    for path in SYSTEM_FONT_FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert("system".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "system".into());
        break;
    }
    for path in SYMBOL_FONT_FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert(path.into(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(path.into());
        }
    }
    add_bold_font(&mut fonts);
    super::icons::add_font(&mut fonts);
    fonts
}

/// Adds the bold family: the bold face, then the proportional fonts (alone where the bold
/// face is missing).
fn add_bold_font(fonts: &mut egui::FontDefinitions) {
    let mut chain = Vec::new();
    let (path, index) = BOLD_FONT_FILE;
    if let Ok(bytes) = std::fs::read(path) {
        let mut data = egui::FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert("bold".into(), data.into());
        chain.push("bold".to_owned());
    }
    chain.extend(
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY_NAME.into()), chain);
}

/// The font family for bold text; proportional on a context without [`fonts`] (e.g. headless
/// tests).
pub fn bold_family(ctx: &egui::Context) -> egui::FontFamily {
    let family = egui::FontFamily::Name(BOLD_FAMILY_NAME.into());
    if ctx.fonts(|f| f.definitions().families.contains_key(&family)) {
        family
    } else {
        egui::FontFamily::Proportional
    }
}

/// The style for one system theme: shared type, spacing and shape, themed palette.
pub fn style(theme: egui::Theme) -> egui::Style {
    let mut style = egui::Style::default();
    style.visuals = match theme {
        egui::Theme::Dark => dark(),
        egui::Theme::Light => light(),
    };
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.interact_size.y = 24.0;
    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(19.0)),
        (TextStyle::Body, FontId::proportional(14.5)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Small, FontId::proportional(11.5)),
    ]
    .into();
    style
}

fn dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(0x17, 0x17, 0x1f);
    v.window_fill = Color32::from_rgb(0x1b, 0x1b, 0x24);
    v.extreme_bg_color = Color32::from_rgb(0x0e, 0x0e, 0x15);
    v.faint_bg_color = Color32::from_rgb(0x1e, 0x1e, 0x29);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0xea, 0xea, 0xf6));
    v.hyperlink_color = Color32::from_rgb(0x9d, 0xa7, 0xff);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x26, 0x26, 0x33);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x30, 0x30, 0x40);
    shape(&mut v);
    v
}

fn light() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = Color32::from_rgb(0xf7, 0xf7, 0xfa);
    v.window_fill = Color32::from_rgb(0xfc, 0xfc, 0xfe);
    v.extreme_bg_color = Color32::from_rgb(0xec, 0xec, 0xf2);
    v.faint_bg_color = Color32::from_rgb(0xef, 0xef, 0xf4);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = Color32::from_rgb(0x4a, 0x44, 0xb6);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xe6, 0xe6, 0xed);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xdd, 0xdd, 0xe7);
    shape(&mut v);
    v
}

/// Rounded corners shared by both palettes.
fn shape(v: &mut egui::Visuals) {
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both styles carry the accent, the type scale and the rounded shape.
    #[test]
    fn styles_share_type_and_accent() {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = style(theme);
            assert_eq!(style.visuals.selection.bg_fill, ACCENT);
            assert_eq!(
                style.text_styles[&TextStyle::Body],
                FontId::proportional(14.5)
            );
            assert_eq!(
                style.visuals.widgets.inactive.corner_radius,
                CornerRadius::same(8)
            );
        }
    }

    /// Installing on a fresh context styles both themes and keeps fallback fonts, whether
    /// or not a system face exists on this machine.
    #[test]
    fn install_applies_to_context() {
        let ctx = egui::Context::default();
        install(&ctx);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.selection.bg_fill,
            ACCENT
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Light).visuals.selection.bg_fill,
            ACCENT
        );
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("themed");
            // On macOS; elsewhere none of the files exist.
            if SYMBOL_FONT_FILES
                .iter()
                .any(|f| std::path::Path::new(f).exists())
            {
                for symbol in ["→", "↓", "✓", "✗"] {
                    assert!(crate::ui::chat::can_draw(ui, symbol), "{symbol}");
                }
            }
        });
        output.textures_delta.clear();
    }
}
