//! Icons from Phosphor's bold weight (MIT, see `assets/LICENSES.md`), bundled as a font: one
//! glyph per node kind and state, drawn as text so they stay crisp at every size.

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Color32, FontFamily, FontId, TextFormat, TextStyle, Ui};

use crate::model::{NodeKind, NodeState};

const FONT: &[u8] = include_bytes!("../../assets/fonts/Phosphor-Bold.ttf");
const FONT_NAME: &str = "phosphor-bold";
const FAMILY_NAME: &str = "icons";

pub const CROWN: char = '\u{e614}';
pub const TREE_STRUCTURE: char = '\u{e67c}';
pub const HEADSET: char = '\u{e584}';
pub const HARD_HAT: char = '\u{ed46}';
pub const TERMINAL_WINDOW: char = '\u{eae8}';
pub const ROBOT: char = '\u{e762}';
pub const PLAY: char = '\u{e3d0}';
pub const MOON: char = '\u{e330}';
pub const EXCLAMATION_MARK: char = '\u{ee44}';
pub const HOURGLASS_HIGH: char = '\u{e2b4}';
pub const CHECK: char = '\u{e182}';
pub const X: char = '\u{e4f6}';
pub const ARROW_LEFT: char = '\u{e058}';
pub const ARROW_RIGHT: char = '\u{e06c}';
pub const BELL: char = '\u{e0ce}';
pub const BELL_RINGING: char = '\u{e5e8}';
pub const BELL_SLASH: char = '\u{e0d4}';
pub const INFO: char = '\u{e2ce}';
pub const WARNING: char = '\u{e4e0}';
pub const SIREN: char = '\u{e9b8}';
pub const ARROW_SQUARE_OUT: char = '\u{e5de}';
pub const SPEAKER_HIGH: char = '\u{e44a}';

fn icon_family() -> FontFamily {
    FontFamily::Name(FAMILY_NAME.into())
}

/// The font family icons are drawn in: Phosphor, then the proportional fonts. Proportional
/// alone on a context without [`add_font`]'s fonts (e.g. headless tests), where icons then
/// show as missing glyphs instead of panicking.
pub fn family(ctx: &egui::Context) -> FontFamily {
    let family = icon_family();
    if ctx.fonts(|f| f.definitions().families.contains_key(&family)) {
        family
    } else {
        FontFamily::Proportional
    }
}

/// Adds the icon font and its family to `fonts`. Call after the proportional family is final.
pub fn add_font(fonts: &mut egui::FontDefinitions) {
    fonts
        .font_data
        .insert(FONT_NAME.into(), egui::FontData::from_static(FONT).into());
    let mut chain = vec![FONT_NAME.to_owned()];
    chain.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(icon_family(), chain);
}

pub fn kind_icon(kind: NodeKind) -> char {
    match kind {
        NodeKind::Envoy => '✉',
        NodeKind::Queen => CROWN,
        NodeKind::Coordinator => TREE_STRUCTURE,
        NodeKind::Attendant => HEADSET,
        NodeKind::Worker => HARD_HAT,
        NodeKind::Terminal => TERMINAL_WINDOW,
        NodeKind::Subagent => ROBOT,
    }
}

pub fn state_icon(state: NodeState) -> char {
    match state {
        NodeState::Working => PLAY,
        NodeState::Idle => MOON,
        NodeState::NeedsInput => EXCLAMATION_MARK,
        NodeState::Stalled => HOURGLASS_HIGH,
        NodeState::Done => CHECK,
        NodeState::Failed => X,
    }
}

/// `icon`, a space and `text` as one label in `style`. `color` `None` takes the widget's text
/// color.
pub fn with_icon(
    ui: &Ui,
    icon: char,
    text: &str,
    style: TextStyle,
    color: Option<Color32>,
) -> LayoutJob {
    let size = style.resolve(ui.style()).size;
    let color = color.unwrap_or(Color32::PLACEHOLDER);
    let mut job = LayoutJob::default();
    job.append(
        &icon.to_string(),
        0.0,
        TextFormat::simple(FontId::new(size, family(ui.ctx())), color),
    );
    job.append(
        text,
        size * 0.35,
        TextFormat::simple(FontId::new(size, FontFamily::Proportional), color),
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::graph::ALL_STATES;

    const ALL_KINDS: [NodeKind; 6] = [
        NodeKind::Queen,
        NodeKind::Coordinator,
        NodeKind::Attendant,
        NodeKind::Worker,
        NodeKind::Terminal,
        NodeKind::Subagent,
    ];

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        ctx
    }

    /// Every icon the app uses is in the bundled font, so none falls back to a tofu box.
    #[test]
    fn icons_are_in_the_font() {
        let ctx = context();
        let icons = ALL_KINDS
            .map(kind_icon)
            .into_iter()
            .chain(ALL_STATES.map(state_icon))
            .chain([ARROW_LEFT, ARROW_RIGHT]);
        ctx.fonts_mut(|f| {
            for icon in icons {
                assert!(
                    f.has_glyph(&FontId::new(14.0, icon_family()), icon),
                    "U+{:04X}",
                    icon as u32
                );
            }
        });
    }

    #[test]
    fn kinds_and_states_have_distinct_icons() {
        let kinds: std::collections::HashSet<_> = ALL_KINDS.map(kind_icon).into();
        assert_eq!(kinds.len(), ALL_KINDS.len());
        let states: std::collections::HashSet<_> = ALL_STATES.map(state_icon).into();
        assert_eq!(states.len(), ALL_STATES.len());
    }

    /// Icons are one em wide, which node label extents rely on.
    #[test]
    fn icons_are_one_em_wide() {
        let ctx = context();
        let width = ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                HARD_HAT.to_string(),
                FontId::new(16.0, icon_family()),
                Color32::WHITE,
            )
            .size()
            .x
        });
        assert!((width - 16.0).abs() < 0.5, "{width}");
    }
}
