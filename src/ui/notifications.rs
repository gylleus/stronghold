//! Notifications in the window: pop-ups (toasts) at the top right, and the top bar's inbox
//! menu (bell with the unread count). Both report what Karl clicked; the app acts on it.

use eframe::egui::{self, Align2, Color32, RichText, TextStyle, Ui};

use crate::notify::Severity;
use crate::notify::history::Entry;
use crate::ui::icons;
use crate::ui::table::format_age;

/// Toasts shown at once; older ones make room.
const MAX_TOASTS: usize = 4;
/// Width of a toast.
const TOAST_WIDTH: f32 = 340.0;
/// Longest message a toast shows.
const TOAST_MESSAGE_CHARS: usize = 280;
/// Inbox entries the menu lists.
const INBOX_SHOWN: usize = 50;

/// The color of a severity's icon, badge and toast stripe.
pub fn severity_color(severity: Severity, dark: bool) -> Color32 {
    match (severity, dark) {
        (Severity::Info, true) => Color32::from_rgb(0x5a, 0x9c, 0xe6),
        (Severity::Info, false) => Color32::from_rgb(0x2a, 0x6c, 0xc0),
        (Severity::Warn, _) => Color32::from_rgb(0xd0, 0x9a, 0x2a),
        (Severity::Urgent, _) => Color32::from_rgb(0xd8, 0x44, 0x44),
    }
}

pub fn severity_icon(severity: Severity) -> char {
    match severity {
        Severity::Info => icons::INFO,
        Severity::Warn => icons::WARNING,
        Severity::Urgent => icons::SIREN,
    }
}

/// How long a toast stays; `None` stays until dismissed.
fn toast_lifetime_s(severity: Severity) -> Option<f64> {
    match severity {
        Severity::Info => Some(8.0),
        Severity::Warn => Some(20.0),
        Severity::Urgent => None,
    }
}

/// What Karl did with a toast or inbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeAction {
    /// Select the entry's node; with `open_url` also open its link.
    Activate {
        id: u64,
        open_url: bool,
    },
    Dismiss(u64),
    MarkAllRead,
    Clear,
    /// Quiet mode switched in the inbox.
    SetQuiet(bool),
}

#[derive(Debug, Clone, PartialEq)]
struct Toast {
    id: u64,
    severity: Severity,
    title: String,
    message: String,
    agent: Option<String>,
    count: u32,
    has_url: bool,
    /// egui time it was (re)shown; set on the first frame that draws it.
    shown_at: Option<f64>,
}

/// The pop-ups on screen, oldest first.
#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// Shows `entry`, replacing its toast if it is up (a collapsed duplicate restarts it).
    pub fn push(&mut self, entry: &Entry) {
        self.remove(entry.id);
        self.items.push(Toast {
            id: entry.id,
            severity: entry.severity,
            title: entry.title.clone(),
            message: entry.message.chars().take(TOAST_MESSAGE_CHARS).collect(),
            agent: entry.agent_label.clone(),
            count: entry.count,
            has_url: entry.url.is_some(),
            shown_at: None,
        });
        let excess = self.items.len().saturating_sub(MAX_TOASTS);
        self.items.drain(..excess);
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|t| t.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn ids(&self) -> Vec<u64> {
        self.items.iter().map(|t| t.id).collect()
    }

    /// Draws the toasts below `top` at the right edge, newest on top, and drops expired ones.
    pub fn show(&mut self, ctx: &egui::Context, top: f32) -> Option<NoticeAction> {
        let now = ctx.input(|i| i.time);
        self.items.retain(|t| {
            let expired = t.shown_at.zip(toast_lifetime_s(t.severity));
            expired.is_none_or(|(at, life)| now - at < life)
        });
        if self.items.is_empty() {
            return None;
        }
        let mut action = None;
        let mut next_expiry = f64::INFINITY;
        egui::Area::new(egui::Id::new("notification_toasts"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_TOP, egui::vec2(-12.0, top + 8.0))
            .show(ctx, |ui| {
                ui.set_width(TOAST_WIDTH);
                for toast in self.items.iter_mut().rev() {
                    let shown_at = *toast.shown_at.get_or_insert(now);
                    if let Some(life) = toast_lifetime_s(toast.severity) {
                        next_expiry = next_expiry.min(shown_at + life - now);
                    }
                    if let Some(a) = toast_ui(ui, toast) {
                        action = Some(a);
                    }
                    ui.add_space(6.0);
                }
            });
        if next_expiry.is_finite() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next_expiry.max(0.05)));
        }
        if let Some(NoticeAction::Dismiss(id) | NoticeAction::Activate { id, .. }) = action {
            self.remove(id);
        }
        action
    }
}

fn toast_ui(ui: &mut Ui, toast: &Toast) -> Option<NoticeAction> {
    let dark = ui.visuals().dark_mode;
    let color = severity_color(toast.severity, dark);
    let mut action = None;
    let frame = egui::Frame::popup(ui.style())
        .stroke(egui::Stroke::new(1.5, color))
        .inner_margin(egui::Margin::symmetric(10, 8));
    frame.show(ui, |ui| {
        ui.set_width(TOAST_WIDTH - 20.0);
        ui.horizontal(|ui| {
            let mut title = toast.title.clone();
            if toast.count > 1 {
                title.push_str(&format!("  ×{}", toast.count));
            }
            let job = icons::with_icon(
                ui,
                severity_icon(toast.severity),
                &title,
                TextStyle::Body,
                Some(color),
            );
            let clicked = ui
                .add(egui::Label::new(job).sense(egui::Sense::click()).truncate())
                .on_hover_text("Show the agent")
                .clicked();
            if clicked {
                action = Some(NoticeAction::Activate {
                    id: toast.id,
                    open_url: false,
                });
            }
        });
        if let Some(agent) = &toast.agent {
            ui.label(RichText::new(agent).small().weak());
        }
        if !toast.message.is_empty() {
            ui.add(egui::Label::new(&toast.message).wrap());
        }
        ui.horizontal(|ui| {
            if ui.small_button("Show").clicked() {
                action = Some(NoticeAction::Activate {
                    id: toast.id,
                    open_url: false,
                });
            }
            if toast.has_url && ui.small_button("Open link").clicked() {
                action = Some(NoticeAction::Activate {
                    id: toast.id,
                    open_url: true,
                });
            }
            if ui.small_button("Dismiss").clicked() {
                action = Some(NoticeAction::Dismiss(toast.id));
            }
        });
    });
    action
}

/// The top bar's inbox: a bell with the unread count, opening the newest notifications.
pub fn inbox_menu(
    ui: &mut Ui,
    entries: &[Entry],
    quiet: bool,
    now_ms: i64,
) -> Option<NoticeAction> {
    let dark = ui.visuals().dark_mode;
    let (unread, loudest) = unread_summary(entries);
    let icon = match (quiet, loudest.is_some()) {
        (true, _) => icons::BELL_SLASH,
        (false, true) => icons::BELL_RINGING,
        (false, false) => icons::BELL,
    };
    let count = if unread == 0 {
        String::new()
    } else {
        unread.to_string()
    };
    let color = loudest.map(|s| severity_color(s, dark));
    let label = icons::with_icon(ui, icon, &count, TextStyle::Button, color);
    let mut action = None;
    let response = ui.menu_button(egui::WidgetText::from(label), |ui| {
        ui.set_min_width(380.0);
        ui.horizontal(|ui| {
            ui.strong(format!("Notifications ({unread} unread)"));
            if ui
                .add_enabled(unread > 0, egui::Button::new("Mark all read"))
                .clicked()
            {
                action = Some(NoticeAction::MarkAllRead);
            }
            if ui
                .add_enabled(!entries.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                action = Some(NoticeAction::Clear);
            }
        });
        let mut quiet_now = quiet;
        if ui
            .checkbox(&mut quiet_now, "Quiet mode")
            .on_hover_text("Only urgent notifications pop up or play a sound")
            .changed()
        {
            action = Some(NoticeAction::SetQuiet(quiet_now));
        }
        ui.separator();
        if entries.is_empty() {
            ui.weak("Nothing yet. Agents reach you here with the notify tool.");
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| {
                for entry in entries.iter().rev().take(INBOX_SHOWN) {
                    if inbox_row(ui, entry, now_ms, dark) {
                        action = Some(NoticeAction::Activate {
                            id: entry.id,
                            open_url: true,
                        });
                    }
                }
            });
    });
    response.response.on_hover_text(if unread == 0 {
        "Notifications".to_owned()
    } else {
        format!("{unread} unread notifications")
    });
    if action.is_some() {
        ui.close();
    }
    action
}

/// How many entries are unread, and the most severe of them.
fn unread_summary(entries: &[Entry]) -> (usize, Option<Severity>) {
    let unread = entries.iter().filter(|e| !e.read);
    (unread.clone().count(), unread.map(|e| e.severity).max())
}

/// One inbox entry; returns whether it was clicked.
fn inbox_row(ui: &mut Ui, entry: &Entry, now_ms: i64, dark: bool) -> bool {
    let color = severity_color(entry.severity, dark);
    let mut title = entry.title.clone();
    if entry.count > 1 {
        title.push_str(&format!("  ×{}", entry.count));
    }
    let mut job = icons::with_icon(
        ui,
        severity_icon(entry.severity),
        &title,
        TextStyle::Body,
        Some(if entry.read {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().strong_text_color()
        }),
    );
    if let Some(first) = job.sections.first_mut() {
        first.format.color = color;
    }
    let mut meta = vec![format!("{} ago", format_age(now_ms - entry.at_ms))];
    if let Some(agent) = &entry.agent_label {
        meta.insert(0, agent.clone());
    }
    if entry.url.is_some() {
        meta.push("link".into());
    }
    let response = ui
        .vertical(|ui| {
            let clicked = ui
                .add(egui::Button::new(job).frame(false).truncate())
                .clicked();
            ui.label(RichText::new(meta.join(" · ")).small().weak());
            if !entry.message.is_empty() {
                let first_line = entry.message.lines().next().unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(first_line).small()).truncate());
            }
            clicked
        })
        .inner;
    ui.add_space(4.0);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Source;
    use crate::ui::test_util::click_label;

    fn entry(id: u64, severity: Severity) -> Entry {
        Entry {
            id,
            at_ms: 0,
            first_at_ms: 0,
            source: Source::Agent,
            severity,
            title: format!("Title {id}"),
            message: "details".into(),
            url: Some("https://example.com".into()),
            target: Some("term_a".into()),
            sender: Some("term_a".into()),
            agent_label: Some("fix-login".into()),
            count: 1,
            read: false,
        }
    }

    #[test]
    fn toasts_replace_duplicates_and_keep_the_newest() {
        let mut t = Toasts::default();
        for id in 1..=6 {
            t.push(&entry(id, Severity::Info));
        }
        assert_eq!(t.ids(), vec![3, 4, 5, 6]);
        t.push(&entry(4, Severity::Info));
        assert_eq!(t.ids(), vec![3, 5, 6, 4], "a repeat moves to the top");
        t.remove(5);
        assert_eq!(t.ids(), vec![3, 6, 4]);
    }

    fn toast_click(label: &str, e: Entry) -> Option<NoticeAction> {
        let mut toasts = Toasts::default();
        toasts.push(&e);
        let mut last = None;
        click_label(label, |ui| {
            if let Some(a) = toasts.show(ui.ctx(), 0.0) {
                last = Some(a);
            }
        });
        last
    }

    #[test]
    fn toast_buttons_report_actions() {
        assert_eq!(
            toast_click("Open link", entry(7, Severity::Warn)),
            Some(NoticeAction::Activate {
                id: 7,
                open_url: true
            })
        );
        assert_eq!(
            toast_click("Show", entry(7, Severity::Warn)),
            Some(NoticeAction::Activate {
                id: 7,
                open_url: false
            })
        );
        assert_eq!(
            toast_click("Dismiss", entry(7, Severity::Urgent)),
            Some(NoticeAction::Dismiss(7))
        );
    }

    #[test]
    fn toasts_expire_except_urgent() {
        let ctx = egui::Context::default();
        let mut toasts = Toasts::default();
        toasts.push(&entry(1, Severity::Info));
        toasts.push(&entry(2, Severity::Urgent));
        for time in [0.0, 100.0] {
            let input = egui::RawInput {
                time: Some(time),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                toasts.show(ui.ctx(), 0.0);
            });
            output.textures_delta.clear();
        }
        assert_eq!(toasts.ids(), vec![2]);
    }

    #[test]
    fn the_bell_counts_unread_and_takes_the_loudest_color() {
        let mut read = entry(1, Severity::Urgent);
        read.read = true;
        let entries = vec![read, entry(2, Severity::Info), entry(3, Severity::Warn)];
        assert_eq!(unread_summary(&entries), (2, Some(Severity::Warn)));
        assert_eq!(unread_summary(&entries[..1]), (0, None));
    }
}
