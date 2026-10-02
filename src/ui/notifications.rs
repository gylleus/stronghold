//! Notifications in the window: pop-ups (toasts) at the top right, the notifications panel
//! (the history, newest first, with a strip showing the unread count while collapsed) and the
//! top bar's bell that opens it. They report what Karl clicked; the app acts on it.

use std::collections::HashSet;

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
    /// An entry was read in the notifications panel.
    MarkRead(u64),
    /// Open an entry's link (and mark it read).
    OpenUrl(u64),
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

/// The top bar's bell: the unread count in the loudest unread severity's color. Clicking it
/// opens or collapses the notifications panel.
pub fn bell_button(ui: &mut Ui, entries: &[Entry], quiet: bool, open: bool) -> bool {
    let (unread, loudest) = unread_summary(entries);
    let color = loudest.map(|s| severity_color(s, ui.visuals().dark_mode));
    let count = if unread == 0 {
        String::new()
    } else {
        unread.to_string()
    };
    let label = icons::with_icon(
        ui,
        bell_icon(quiet, unread),
        &count,
        TextStyle::Button,
        color,
    );
    let hover = match (unread, open) {
        (0, true) => "Notifications: hide the panel".to_owned(),
        (0, false) => "Notifications: show the panel".to_owned(),
        (n, _) => format!("{n} unread notifications"),
    };
    ui.add(egui::Button::new(label).selected(open))
        .on_hover_text(hover)
        .clicked()
}

fn bell_icon(quiet: bool, unread: usize) -> char {
    match (quiet, unread > 0) {
        (true, _) => icons::BELL_SLASH,
        (false, true) => icons::BELL_RINGING,
        (false, false) => icons::BELL,
    }
}

/// How many entries are unread, and the most severe of them.
fn unread_summary(entries: &[Entry]) -> (usize, Option<Severity>) {
    let unread = entries.iter().filter(|e| !e.read);
    (unread.clone().count(), unread.map(|e| e.severity).max())
}

/// Width of the collapsed panel's strip.
pub const STRIP_WIDTH: f32 = 34.0;

/// The collapsed panel: a strip down the window's edge with the bell and the unread count.
/// Returns whether it was clicked (to open the panel).
pub fn collapsed_strip(ui: &mut Ui, entries: &[Entry], quiet: bool) -> bool {
    let (unread, loudest) = unread_summary(entries);
    let rect = ui.available_rect_before_wrap();
    let response = ui
        .allocate_rect(rect, egui::Sense::click())
        .on_hover_text(match unread {
            0 => "Show notifications".to_owned(),
            n => format!("Show notifications ({n} unread)"),
        });
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    if response.hovered() {
        painter.rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let dark = visuals.dark_mode;
    let icon_color = loudest.map_or(visuals.text_color(), |s| severity_color(s, dark));
    let mut y = rect.top() + 18.0;
    painter.text(
        egui::pos2(rect.center().x, y),
        Align2::CENTER_CENTER,
        bell_icon(quiet, unread),
        egui::FontId::new(18.0, icons::family(ui.ctx())),
        icon_color,
    );
    y += 24.0;
    if let Some(severity) = loudest {
        let text = if unread > 99 {
            "99+".to_owned()
        } else {
            unread.to_string()
        };
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
        let size = egui::vec2(
            galley.size().x.max(galley.size().y) + 8.0,
            galley.size().y + 4.0,
        );
        let pill = egui::Rect::from_center_size(egui::pos2(rect.center().x, y), size);
        painter.rect_filled(pill, size.y / 2.0, severity_color(severity, dark));
        painter.galley(pill.center() - galley.size() / 2.0, galley, Color32::WHITE);
        y += size.y;
    }
    let title = egui::epaint::TextShape::new(
        egui::pos2(rect.center().x + 6.0, y + 12.0),
        painter.layout_no_wrap(
            "Notifications".to_owned(),
            egui::FontId::proportional(12.0),
            visuals.weak_text_color(),
        ),
        visuals.weak_text_color(),
    )
    .with_angle(std::f32::consts::FRAC_PI_2);
    painter.add(title);
    response.clicked()
}

/// What Karl did in the notifications panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    Notice(NoticeAction),
    /// Collapse the panel to its strip.
    Collapse,
    /// Move the panel to the window's other side.
    SwitchSide,
}

/// The notifications panel's own state: which entries are expanded.
#[derive(Debug, Default)]
pub struct NotificationsPanel {
    expanded: HashSet<u64>,
}

impl NotificationsPanel {
    /// Expands entry `id`, as clicking it would (without marking it read).
    pub fn expand(&mut self, id: u64) {
        self.expanded.insert(id);
    }

    /// The header (unread count, quiet mode, mark all read, clear, side and collapse) over the
    /// history, newest first. `right` is whether the panel sits at the window's right.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        entries: &[Entry],
        quiet: bool,
        right: bool,
        now_ms: i64,
    ) -> Option<PanelAction> {
        if !self.expanded.is_empty() {
            self.expanded
                .retain(|id| entries.iter().any(|e| e.id == *id));
        }
        let mut action = None;
        egui::Frame::side_top_panel(ui.style()).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            if let Some(a) = panel_header(ui, entries, quiet, right) {
                action = Some(a);
            }
            ui.separator();
            if entries.is_empty() {
                ui.add_space(8.0);
                ui.weak("Nothing yet. Agents reach you here with the notify tool.");
                return;
            }
            let dark = ui.visuals().dark_mode;
            egui::ScrollArea::vertical()
                .id_salt("notifications_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for entry in entries.iter().rev() {
                        let expanded = self.expanded.contains(&entry.id);
                        let row = entry_row(ui, entry, expanded, now_ms, dark);
                        if row.toggled {
                            if expanded {
                                self.expanded.remove(&entry.id);
                            } else {
                                self.expanded.insert(entry.id);
                                if !entry.read {
                                    action =
                                        Some(PanelAction::Notice(NoticeAction::MarkRead(entry.id)));
                                }
                            }
                        }
                        if let Some(a) = row.action {
                            action = Some(PanelAction::Notice(a));
                        }
                        ui.add_space(4.0);
                    }
                });
        });
        action
    }
}

fn panel_header(ui: &mut Ui, entries: &[Entry], quiet: bool, right: bool) -> Option<PanelAction> {
    let (unread, _) = unread_summary(entries);
    let mut action = None;
    ui.horizontal(|ui| {
        ui.strong("Notifications");
        if unread > 0 {
            ui.label(RichText::new(format!("{unread} unread")).small());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icon_text(ui, icons::X))
                .on_hover_text("Collapse the panel to a strip at the window's edge")
                .clicked()
            {
                action = Some(PanelAction::Collapse);
            }
            let (arrow, other) = if right {
                (icons::ARROW_LEFT, "left")
            } else {
                (icons::ARROW_RIGHT, "right")
            };
            if ui
                .small_button(icon_text(ui, arrow))
                .on_hover_text(format!(
                    "Move the panel to the {other} side (or drag its tab anywhere)"
                ))
                .clicked()
            {
                action = Some(PanelAction::SwitchSide);
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(unread > 0, egui::Button::new("Mark all read").small())
            .clicked()
        {
            action = Some(PanelAction::Notice(NoticeAction::MarkAllRead));
        }
        if ui
            .add_enabled(!entries.is_empty(), egui::Button::new("Clear").small())
            .on_hover_text("Delete every notification in the history")
            .clicked()
        {
            action = Some(PanelAction::Notice(NoticeAction::Clear));
        }
        let mut quiet_now = quiet;
        if ui
            .checkbox(&mut quiet_now, "Quiet mode")
            .on_hover_text("Only urgent notifications pop up or play a sound")
            .changed()
        {
            action = Some(PanelAction::Notice(NoticeAction::SetQuiet(quiet_now)));
        }
    });
    action
}

/// `icon` alone, in the icon font.
fn icon_text(ui: &Ui, icon: char) -> RichText {
    RichText::new(icon.to_string()).family(icons::family(ui.ctx()))
}

/// What Karl did with one entry of the panel.
struct RowResponse {
    /// Its header was clicked: expand or collapse it.
    toggled: bool,
    action: Option<NoticeAction>,
}

/// What raised an entry, for its meta line; `None` for an agent's own notification (its agent
/// says it). New sources show under their API name.
fn source_label(entry: &Entry) -> Option<String> {
    match entry.source.name() {
        "agent" => None,
        name => Some(name.replace('_', " ")),
    }
}

/// One entry: severity, title, agent and age; expanded, also the full message, the link and
/// a button to the agent's node. Unread entries are tinted and striped in their severity's
/// color, with a strong title.
fn entry_row(ui: &mut Ui, entry: &Entry, expanded: bool, now_ms: i64, dark: bool) -> RowResponse {
    let color = severity_color(entry.severity, dark);
    let mut action = None;
    // The background goes under the content once its size and hover are known.
    let background = ui.painter().add(egui::Shape::Noop);
    let margin = if expanded {
        egui::Margin {
            left: 12,
            right: 8,
            top: 8,
            bottom: 8,
        }
    } else {
        egui::Margin {
            left: 10,
            right: 6,
            top: 5,
            bottom: 5,
        }
    };
    let shown = egui::Frame::new().inner_margin(margin).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let header = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                entry_header(ui, entry, expanded, now_ms, color);
            })
            .response;
        if expanded {
            ui.add_space(2.0);
            ui.separator();
            if let Some(a) = entry_details(ui, entry, now_ms) {
                action = Some(a);
            }
        }
        header
    });
    let rect = shown.response.rect;
    let header = shown.inner;
    let visuals = ui.visuals();
    let accent = visuals.selection.bg_fill;
    let (fill, stroke) = if expanded {
        (
            accent.gamma_multiply(if dark { 0.22 } else { 0.12 }),
            egui::Stroke::new(1.0, accent.gamma_multiply(0.8)),
        )
    } else if header.hovered() {
        (visuals.widgets.hovered.weak_bg_fill, egui::Stroke::NONE)
    } else if !entry.read {
        (color.gamma_multiply(0.12), egui::Stroke::NONE)
    } else {
        (Color32::TRANSPARENT, egui::Stroke::NONE)
    };
    ui.painter()
        .set(background, egui::Shape::rect_filled(rect, 5.0, fill));
    if stroke != egui::Stroke::NONE {
        ui.painter()
            .rect_stroke(rect, 5.0, stroke, egui::StrokeKind::Inside);
    }
    if !entry.read || expanded {
        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
        ui.painter().rect_filled(
            stripe,
            egui::CornerRadius {
                nw: 5,
                sw: 5,
                ne: 0,
                se: 0,
            },
            if expanded { accent } else { color },
        );
    }
    if header.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let toggled = header
        .on_hover_text(if expanded {
            "Collapse"
        } else {
            "Show the full notification"
        })
        .clicked();
    RowResponse { toggled, action }
}

fn entry_header(ui: &mut Ui, entry: &Entry, expanded: bool, now_ms: i64, color: Color32) {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 12.0), egui::Sense::hover());
        let c = rect.center();
        let points = if expanded {
            vec![
                egui::pos2(c.x - 4.0, c.y - 2.0),
                egui::pos2(c.x + 4.0, c.y - 2.0),
                egui::pos2(c.x, c.y + 3.0),
            ]
        } else {
            vec![
                egui::pos2(c.x - 2.0, c.y - 4.0),
                egui::pos2(c.x + 3.0, c.y),
                egui::pos2(c.x - 2.0, c.y + 4.0),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            visuals.weak_text_color(),
            egui::Stroke::NONE,
        ));
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
                visuals.text_color()
            } else {
                visuals.strong_text_color()
            }),
        );
        if let Some(first) = job.sections.first_mut() {
            first.format.color = color;
        }
        if expanded && let Some(text) = job.sections.get_mut(1) {
            text.format.font_id.family = crate::ui::theme::bold_family(ui.ctx());
            text.format.color = visuals.strong_text_color();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format_age(now_ms - entry.at_ms))
                    .small()
                    .weak(),
            );
            if !entry.read {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, color);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(job).truncate().selectable(false));
            });
        });
    });
    let mut meta = Vec::new();
    meta.push(
        entry
            .agent_label
            .clone()
            .or_else(|| entry.sender.clone())
            .unwrap_or_else(|| "Myrstack".to_owned()),
    );
    meta.extend(source_label(entry));
    if entry.url.is_some() {
        meta.push("link".into());
    }
    ui.add(
        egui::Label::new(RichText::new(meta.join(" · ")).small().weak())
            .truncate()
            .selectable(false),
    );
    if !expanded && !entry.message.is_empty() {
        let first_line = entry.message.lines().next().unwrap_or_default();
        ui.add(
            egui::Label::new(RichText::new(first_line).small())
                .truncate()
                .selectable(false),
        );
    }
}

fn entry_details(ui: &mut Ui, entry: &Entry, now_ms: i64) -> Option<NoticeAction> {
    let mut action = None;
    if !entry.message.is_empty() {
        ui.add(egui::Label::new(&entry.message).wrap());
    }
    if let Some(url) = &entry.url {
        ui.add_space(2.0);
        if ui
            .add(egui::Link::new(RichText::new(url).small()))
            .on_hover_text("Open in the browser")
            .clicked()
        {
            action = Some(NoticeAction::OpenUrl(entry.id));
        }
    }
    if entry.count > 1 {
        ui.label(
            RichText::new(format!(
                "{} times, first {} ago",
                entry.count,
                format_age(now_ms - entry.first_at_ms)
            ))
            .small()
            .weak(),
        );
    }
    if entry.target.is_some() {
        ui.add_space(2.0);
        if ui
            .small_button("Show agent")
            .on_hover_text("Select the agent's node")
            .clicked()
        {
            action = Some(NoticeAction::Activate {
                id: entry.id,
                open_url: false,
            });
        }
    }
    action
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

    fn panel_click(label: &str, panel: &mut NotificationsPanel, e: Entry) -> Option<PanelAction> {
        let entries = vec![e];
        let mut last = None;
        click_label(label, |ui| {
            if let Some(a) = panel.show(ui, &entries, false, false, 60_000) {
                last = Some(a);
            }
        });
        last
    }

    #[test]
    fn expanding_an_unread_entry_marks_it_read() {
        let mut panel = NotificationsPanel::default();
        assert_eq!(
            panel_click("fix-login · link", &mut panel, entry(3, Severity::Warn)),
            Some(PanelAction::Notice(NoticeAction::MarkRead(3)))
        );
        assert!(panel.expanded.contains(&3));
        let mut read = entry(3, Severity::Warn);
        read.read = true;
        assert_eq!(panel_click("fix-login · link", &mut panel, read), None);
        assert!(!panel.expanded.contains(&3), "a second click collapses it");
    }

    #[test]
    fn an_expanded_entry_opens_its_link_and_shows_its_agent() {
        let mut panel = NotificationsPanel::default();
        panel.expanded.insert(5);
        assert_eq!(
            panel_click("https://example.com", &mut panel, entry(5, Severity::Info)),
            Some(PanelAction::Notice(NoticeAction::OpenUrl(5)))
        );
        assert_eq!(
            panel_click("Show agent", &mut panel, entry(5, Severity::Info)),
            Some(PanelAction::Notice(NoticeAction::Activate {
                id: 5,
                open_url: false
            }))
        );
        assert_eq!(
            panel_click("details", &mut panel, entry(5, Severity::Info)),
            None,
            "the message is text, not a toggle"
        );
    }

    #[test]
    fn expanded_entries_that_left_the_history_are_forgotten() {
        let mut panel = NotificationsPanel::default();
        panel.expanded.extend([1, 2]);
        click_label("Notifications", |ui| {
            panel.show(ui, &[entry(2, Severity::Info)], false, false, 0)
        });
        assert_eq!(panel.expanded, HashSet::from([2]));
    }

    #[test]
    fn other_sources_name_themselves_in_the_meta_line() {
        let mut e = entry(1, Severity::Info);
        assert_eq!(source_label(&e), None);
        e.source = Source::NeedsInput;
        assert_eq!(source_label(&e).as_deref(), Some("needs input"));
        let mut panel = NotificationsPanel::default();
        e.agent_label = None;
        e.sender = None;
        assert_eq!(
            panel_click("Myrstack · needs input · link", &mut panel, e),
            Some(PanelAction::Notice(NoticeAction::MarkRead(1)))
        );
    }

    #[test]
    fn the_header_collapses_and_switches_sides() {
        let mut panel = NotificationsPanel::default();
        assert_eq!(
            panel_click(&icons::X.to_string(), &mut panel, entry(1, Severity::Info)),
            Some(PanelAction::Collapse)
        );
        assert_eq!(
            panel_click(
                &icons::ARROW_RIGHT.to_string(),
                &mut panel,
                entry(1, Severity::Info)
            ),
            Some(PanelAction::SwitchSide)
        );
        assert_eq!(
            panel_click("Mark all read", &mut panel, entry(1, Severity::Info)),
            Some(PanelAction::Notice(NoticeAction::MarkAllRead))
        );
    }

    #[test]
    fn the_collapsed_strip_opens_on_click() {
        let entries = vec![entry(1, Severity::Urgent), entry(2, Severity::Info)];
        assert!(click_label("2", |ui| collapsed_strip(ui, &entries, false)));
    }
}
