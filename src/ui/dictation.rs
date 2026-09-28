//! Dictation UI: the ⌘E push-to-talk keys, inserting text at a text field's cursor, and the
//! floating "Listening…" pill.

use std::time::Duration;

use eframe::egui::{self, Color32, Event, Key, Modifiers, RichText};
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;

/// Label of the shortcut in the help overlay and palette.
pub const SHORTCUT: &str = "⌘E";

/// A dictation key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationKey {
    /// ⌘E went down (`repeat` for auto-repeats while held).
    Press { repeat: bool },
    /// E or ⌘ was let go while a hold-to-talk recording runs.
    Release,
    /// Esc while recording or transcribing.
    Cancel,
}

/// What the key handling needs to know about the dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyState {
    /// A hold-to-talk recording runs (release ends it).
    pub holding: bool,
    /// Recording or transcribing (Esc cancels).
    pub busy: bool,
}

/// Takes the dictation keys out of `events` (so no other shortcut or widget sees them).
/// `modifiers` are the ones held now: letting go of ⌘ before E also ends a hold.
pub fn take_keys(
    events: &mut Vec<Event>,
    modifiers: Modifiers,
    state: KeyState,
) -> Vec<DictationKey> {
    let mut keys = Vec::new();
    events.retain(|event| {
        let Event::Key {
            key,
            pressed,
            repeat,
            modifiers: m,
            ..
        } = event
        else {
            return true;
        };
        match (key, pressed) {
            (Key::E, true) if m.command_only() => {
                keys.push(DictationKey::Press { repeat: *repeat });
                false
            }
            (Key::E, false) if state.holding || m.command => {
                if state.holding && !keys.contains(&DictationKey::Release) {
                    keys.push(DictationKey::Release);
                }
                false
            }
            (Key::Escape, true) if state.busy => {
                keys.push(DictationKey::Cancel);
                false
            }
            _ => true,
        }
    });
    let pressed_now = keys.iter().any(|k| matches!(k, DictationKey::Press { .. }));
    if state.holding && !modifiers.command && !pressed_now && !keys.contains(&DictationKey::Release)
    {
        keys.push(DictationKey::Release);
    }
    keys
}

/// [`take_keys`] on this frame's input.
pub fn take_frame_keys(ctx: &egui::Context, state: KeyState) -> Vec<DictationKey> {
    ctx.input_mut(|i| {
        let modifiers = i.modifiers;
        take_keys(&mut i.events, modifiers, state)
    })
}

fn glued_before(c: char) -> bool {
    matches!(c, ',' | '.' | '?' | '!' | ';' | ':' | ')' | '%')
}

/// Replaces chars `start..end` of `text` with `insert`, adding a space where it would run into
/// a neighbouring word. Returns the char index just after the inserted text.
pub fn splice_with_spacing(text: &mut String, start: usize, end: usize, insert: &str) -> usize {
    let len = text.chars().count();
    let (start, end) = (start.min(len), end.min(len).max(start.min(len)));
    let before = start.checked_sub(1).and_then(|i| text.chars().nth(i));
    let after = text.chars().nth(end);
    let mut piece = String::new();
    if before.is_some_and(|c| !c.is_whitespace())
        && !insert.starts_with(glued_before)
        && !insert.starts_with(char::is_whitespace)
    {
        piece.push(' ');
    }
    piece.push_str(insert);
    if after.is_some_and(|c| !c.is_whitespace() && !glued_before(c))
        && !insert.ends_with(char::is_whitespace)
    {
        piece.push(' ');
    }
    let byte = |ci: usize| text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b);
    let (bs, be) = (byte(start), byte(end));
    text.replace_range(bs..be, &piece);
    let trailing = usize::from(piece.ends_with(' ') && !insert.ends_with(' '));
    start + piece.chars().count() - trailing
}

/// Inserts `insert` into `text`, the buffer of the `TextEdit` with widget id `id`, at its
/// cursor (replacing the selection), and moves the cursor after it. Without a stored cursor
/// the text is appended.
pub fn insert_at_cursor(ctx: &egui::Context, id: egui::Id, text: &mut String, insert: &str) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    let len = text.chars().count();
    let (start, end) = match state.cursor.char_range() {
        Some(range) => {
            let (a, b) = (range.primary.index.0, range.secondary.index.0);
            (a.min(b), a.max(b))
        }
        None => (len, len),
    };
    let cursor = splice_with_spacing(text, start, end, insert);
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
    state.store(ctx, id);
}

/// Types `text` into whatever text field has focus, as if it were typed (for fields whose
/// buffer the app does not own).
pub fn type_into_focused(ctx: &egui::Context, text: &str) {
    ctx.input_mut(|i| i.events.push(Event::Text(text.to_owned())));
}

/// What the pill shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Pill<'a> {
    Listening {
        level: f32,
        elapsed: Duration,
        /// Started from the palette: stops on ⌘E or the Stop button, not on release.
        latched: bool,
    },
    Transcribing,
    Notice {
        text: &'a str,
        error: bool,
    },
    /// The speech model is not on disk: offer the download.
    ModelMissing,
    Downloading {
        progress: Option<f32>,
        unpacking: bool,
    },
    DownloadFailed(&'a str),
}

/// A pill button the user clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillAction {
    Stop,
    Cancel,
    Download,
    CancelDownload,
    Dismiss,
}

/// Label of the download button.
pub fn download_label() -> String {
    format!(
        "Download speech model ({})",
        crate::voice::models::PARAKEET_SIZE_LABEL
    )
}

/// Draws the pill near the bottom of the window.
pub fn pill(ctx: &egui::Context, pill: &Pill<'_>) -> Option<PillAction> {
    let mut action = None;
    egui::Area::new(egui::Id::new("dictation_pill"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .corner_radius(18.0)
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| pill_contents(ui, pill, &mut action));
                });
        });
    action
}

fn pill_contents(ui: &mut egui::Ui, pill: &Pill<'_>, action: &mut Option<PillAction>) {
    let red = Color32::from_rgb(230, 70, 70);
    let mut button = |ui: &mut egui::Ui, label: &str, a: PillAction| {
        if ui.small_button(label).clicked() {
            *action = Some(a);
        }
    };
    match pill {
        Pill::Listening {
            level,
            elapsed,
            latched,
        } => {
            ui.label(RichText::new("●").color(red));
            ui.label(RichText::new("Listening…").strong());
            level_meter(ui, *level);
            let secs = elapsed.as_secs();
            ui.weak(format!("{}:{:02}", secs / 60, secs % 60));
            if *latched {
                button(ui, "Stop", PillAction::Stop);
                ui.weak("or ⌘E · Esc cancels");
            } else {
                ui.weak("release ⌘E to insert · Esc cancels");
            }
        }
        Pill::Transcribing => {
            ui.spinner();
            ui.label("Transcribing…");
            button(ui, "Cancel", PillAction::Cancel);
        }
        Pill::Notice { text, error } => {
            if *error {
                ui.label(RichText::new("⚠").color(red));
            }
            ui.add(egui::Label::new(*text).wrap());
            button(ui, "✕", PillAction::Dismiss);
        }
        Pill::ModelMissing => {
            ui.label("Dictation needs the Parakeet speech model.");
            button(ui, &download_label(), PillAction::Download);
            button(ui, "✕", PillAction::Dismiss);
        }
        Pill::Downloading {
            progress,
            unpacking,
        } => {
            ui.spinner();
            if *unpacking {
                ui.label("Unpacking the speech model…");
            } else {
                ui.label("Downloading the speech model…");
                if let Some(p) = progress {
                    ui.add(
                        egui::ProgressBar::new(*p)
                            .desired_width(140.0)
                            .show_percentage(),
                    );
                }
            }
            button(ui, "Cancel", PillAction::CancelDownload);
        }
        Pill::DownloadFailed(err) => {
            ui.label(RichText::new("⚠").color(red));
            ui.add(egui::Label::new(format!("Speech model download failed: {err}")).wrap());
            button(ui, "Retry", PillAction::Download);
            button(ui, "✕", PillAction::Dismiss);
        }
    }
}

/// Five bars lit by `level` (0..=1).
pub fn level_meter(ui: &mut egui::Ui, level: f32) {
    const BARS: usize = 5;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 14.0), egui::Sense::hover());
    let lit = (level.clamp(0.0, 1.0) * BARS as f32).ceil() as usize;
    let w = rect.width() / BARS as f32;
    for i in 0..BARS {
        let h = rect.height() * (0.4 + 0.6 * (i as f32 + 1.0) / BARS as f32);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * w + 1.0, rect.bottom() - h),
            egui::vec2(w - 2.0, h),
        );
        let color = if i < lit {
            ui.visuals().selection.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        ui.painter().rect_filled(bar, 1.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, pressed: bool, repeat: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        }
    }

    const IDLE: KeyState = KeyState {
        holding: false,
        busy: false,
    };
    const HOLDING: KeyState = KeyState {
        holding: true,
        busy: true,
    };

    #[test]
    fn cmd_e_press_and_release_are_taken() {
        let mut events = vec![
            key(Key::E, true, false, Modifiers::COMMAND),
            Event::Text("x".into()),
        ];
        let keys = take_keys(&mut events, Modifiers::COMMAND, IDLE);
        assert_eq!(keys, [DictationKey::Press { repeat: false }]);
        assert_eq!(events, [Event::Text("x".into())], "other events stay");

        let mut events = vec![
            key(Key::E, true, true, Modifiers::COMMAND),
            key(Key::E, true, true, Modifiers::COMMAND),
        ];
        let keys = take_keys(&mut events, Modifiers::COMMAND, HOLDING);
        assert_eq!(keys, [DictationKey::Press { repeat: true }; 2]);
        assert!(events.is_empty());

        let mut events = vec![key(Key::E, false, false, Modifiers::COMMAND)];
        let keys = take_keys(&mut events, Modifiers::COMMAND, HOLDING);
        assert_eq!(keys, [DictationKey::Release]);
        assert!(events.is_empty());
    }

    #[test]
    fn letting_go_of_cmd_first_releases() {
        let mut events = vec![];
        assert_eq!(
            take_keys(&mut events, Modifiers::NONE, HOLDING),
            [DictationKey::Release]
        );
        // The E key-up that follows (without ⌘) is still swallowed, once.
        let mut events = vec![key(Key::E, false, false, Modifiers::NONE)];
        assert_eq!(
            take_keys(&mut events, Modifiers::NONE, HOLDING),
            [DictationKey::Release]
        );
        assert!(events.is_empty());
        assert!(take_keys(&mut vec![], Modifiers::NONE, IDLE).is_empty());
    }

    #[test]
    fn plain_e_and_other_combos_pass_through() {
        let mut events = vec![
            key(Key::E, true, false, Modifiers::NONE),
            key(Key::E, true, false, Modifiers::COMMAND | Modifiers::SHIFT),
            key(Key::E, false, false, Modifiers::NONE),
            key(Key::Escape, true, false, Modifiers::NONE),
        ];
        let before = events.clone();
        assert!(take_keys(&mut events, Modifiers::NONE, IDLE).is_empty());
        assert_eq!(events, before);
    }

    #[test]
    fn esc_cancels_only_while_busy() {
        let mut events = vec![key(Key::Escape, true, false, Modifiers::NONE)];
        let busy = KeyState {
            holding: false,
            busy: true,
        };
        assert_eq!(
            take_keys(&mut events, Modifiers::NONE, busy),
            [DictationKey::Cancel]
        );
        assert!(events.is_empty(), "Esc does not also unfocus the field");
    }

    #[test]
    fn splice_adds_spaces_between_words() {
        let mut t = String::from("hello world");
        assert_eq!(splice_with_spacing(&mut t, 5, 5, "there"), 11);
        assert_eq!(t, "hello there world");

        let mut t = String::from("ask");
        assert_eq!(splice_with_spacing(&mut t, 3, 3, "Show me."), 12);
        assert_eq!(t, "ask Show me.");

        let mut t = String::new();
        assert_eq!(splice_with_spacing(&mut t, 0, 0, "Hi."), 3);
        assert_eq!(t, "Hi.");

        let mut t = String::from("abc ");
        assert_eq!(splice_with_spacing(&mut t, 4, 4, "d"), 5);
        assert_eq!(t, "abc d");

        // Before punctuation no space is added; a selection is replaced.
        let mut t = String::from("one TWO, three");
        assert_eq!(splice_with_spacing(&mut t, 4, 7, "two"), 7);
        assert_eq!(t, "one two, three");

        // Char (not byte) indices, out-of-range clamps to the end.
        let mut t = String::from("åäö");
        assert_eq!(splice_with_spacing(&mut t, 1, 1, "x"), 3);
        assert_eq!(t, "å x äö");
        let mut t = String::from("end");
        splice_with_spacing(&mut t, 99, 99, "z");
        assert_eq!(t, "end z");
    }

    fn run(ctx: &egui::Context, events: Vec<Event>, text: &mut String, id: egui::Id) {
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let response = ui.add(egui::TextEdit::singleline(text).id(id));
            if !response.has_focus() {
                response.request_focus();
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn inserts_at_the_text_edit_cursor() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("field");
        let mut text = String::from("hello world");
        run(&ctx, vec![], &mut text, id);
        run(&ctx, vec![], &mut text, id);

        let mut state = TextEditState::load(&ctx, id).expect("the field stored its state");
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(5))));
        state.store(&ctx, id);

        insert_at_cursor(&ctx, id, &mut text, "big");
        assert_eq!(text, "hello big world");
        let cursor = TextEditState::load(&ctx, id)
            .and_then(|s| s.cursor.char_range())
            .unwrap();
        assert_eq!(cursor.primary.index.0, 9);

        // The widget keeps the moved cursor: typing goes after the insert.
        run(&ctx, vec![Event::Text("!".into())], &mut text, id);
        assert_eq!(text, "hello big! world");

        // With no stored state the text is appended.
        let mut other = String::from("tail");
        insert_at_cursor(&ctx, egui::Id::new("never drawn"), &mut other, "end");
        assert_eq!(other, "tail end");
    }

    #[test]
    fn typing_into_the_focused_field() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("query");
        let mut text = String::from("stalled");
        run(&ctx, vec![], &mut text, id);
        run(&ctx, vec![], &mut text, id);
        let mut typed = String::new();
        let input = egui::RawInput::default();
        let mut output = ctx.run_ui(input, |ui| {
            type_into_focused(ui.ctx(), " agents");
            ui.add(egui::TextEdit::singleline(&mut text).id(id));
            typed.clone_from(&text);
        });
        output.textures_delta.clear();
        assert_eq!(typed, "stalled agents");
    }

    #[test]
    fn pill_buttons_report_actions() {
        use crate::ui::test_util::click_label;
        let label = download_label();
        let action = click_label(&label, |ui| pill(ui.ctx(), &Pill::ModelMissing));
        assert_eq!(action, Some(PillAction::Download));
        let action = click_label("Stop", |ui| {
            pill(
                ui.ctx(),
                &Pill::Listening {
                    level: 0.5,
                    elapsed: Duration::from_secs(3),
                    latched: true,
                },
            )
        });
        assert_eq!(action, Some(PillAction::Stop));
        let action = click_label("Cancel", |ui| pill(ui.ctx(), &Pill::Transcribing));
        assert_eq!(action, Some(PillAction::Cancel));
    }
}
