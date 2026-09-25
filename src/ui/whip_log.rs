//! Whip action log: the sidebar section listing every whip decision, newest first.
//!
//! [`WhipLog`] keeps the history the runner delivers (each entry once, by sequence number) and
//! draws it. Entries link to their nodes: clicking one selects and centers the node.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Sense, Ui};
use jiff::tz::TimeZone;

use crate::model::{FleetSnapshot, NodeId};
use crate::ui::table::format_age;
use crate::whip::{WhipLogItem, WhipStatus};

/// Most entries kept in the app.
pub const HISTORY_LEN: usize = 1000;
/// Most entries drawn at once; older ones stay in the history.
const SHOWN_LEN: usize = 100;
const LIST_HEIGHT: f32 = 360.0;

/// What an entry did, shown as a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Nudge,
    Escalate,
    NeedsInput,
    DryRun,
    Failed,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nudge => "NUDGE",
            Self::Escalate => "ESCALATE",
            Self::NeedsInput => "NEEDS INPUT",
            Self::DryRun => "DRY-RUN",
            Self::Failed => "FAILED",
        }
    }

    fn color(self, dark_mode: bool) -> Color32 {
        let (dark, light) = match self {
            Self::Nudge => (
                Color32::from_rgb(90, 150, 230),
                Color32::from_rgb(30, 90, 180),
            ),
            Self::Escalate => (
                Color32::from_rgb(235, 90, 80),
                Color32::from_rgb(190, 40, 30),
            ),
            Self::NeedsInput => (
                Color32::from_rgb(235, 180, 60),
                Color32::from_rgb(170, 110, 0),
            ),
            Self::DryRun => (Color32::GRAY, Color32::DARK_GRAY),
            Self::Failed => (
                Color32::from_rgb(235, 90, 80),
                Color32::from_rgb(190, 40, 30),
            ),
        };
        if dark_mode { dark } else { light }
    }
}

/// The action badge of an entry, then DRY-RUN or FAILED when that applies.
pub fn badges(item: &WhipLogItem) -> Vec<Badge> {
    let entry = &item.entry;
    let mut badges = Vec::with_capacity(2);
    if entry.action.starts_with("nudge") {
        badges.push(Badge::Nudge);
    } else if entry.action.starts_with("escalate") {
        badges.push(Badge::Escalate);
    } else if entry.action.contains("needs-input") {
        badges.push(Badge::NeedsInput);
    }
    if is_failure(item) {
        badges.push(Badge::Failed);
    } else if entry.dry_run {
        badges.push(Badge::DryRun);
    }
    badges
}

fn is_failure(item: &WhipLogItem) -> bool {
    item.entry.outcome.starts_with("failed")
}

/// Which entries the log shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFilter {
    #[default]
    All,
    /// Not dry-run.
    Live,
    Failures,
}

impl LogFilter {
    const ALL: [Self; 3] = [Self::All, Self::Live, Self::Failures];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Live => "Live only",
            Self::Failures => "Failures",
        }
    }

    pub fn matches(self, item: &WhipLogItem) -> bool {
        match self {
            Self::All => true,
            Self::Live => !item.entry.dry_run,
            Self::Failures => is_failure(item),
        }
    }
}

/// The node an entry is about: the worker node of its task, else its terminal's node, else a
/// node last seen on its terminal, else its run's coordinator.
pub fn link_node(snapshot: &FleetSnapshot, item: &WhipLogItem) -> Option<NodeId> {
    let nodes = &snapshot.nodes;
    if let (Some(run), Some(task)) = (&item.run_id, &item.task_id) {
        let workers: Vec<_> = nodes
            .iter()
            .filter(|n| n.run_id.as_ref() == Some(run) && n.task_id.as_ref() == Some(task))
            .collect();
        if let Some(node) = workers.iter().find(|n| n.live).or(workers.first()) {
            return Some(node.id.clone());
        }
    }
    if let Some(handle) = &item.handle {
        let id = NodeId::terminal(handle);
        if nodes.iter().any(|n| n.id == id) {
            return Some(id);
        }
        if let Some(node) = nodes
            .iter()
            .find(|n| n.terminal_handle.as_ref() == Some(handle))
        {
            return Some(node.id.clone());
        }
    }
    let run = snapshot.run(item.run_id.as_deref()?)?;
    run.coordinator.clone()
}

/// The linked node's label, else the terminal handle, else the logged target text.
pub fn target_text(snapshot: &FleetSnapshot, item: &WhipLogItem, link: Option<&NodeId>) -> String {
    link.and_then(|id| snapshot.nodes.iter().find(|n| &n.id == id))
        .map(|n| n.label.clone())
        .filter(|l| !l.is_empty())
        .or_else(|| item.handle.clone())
        .unwrap_or_else(|| item.entry.target.clone())
}

/// `12:04` in `tz`.
pub fn clock_label_in(at_ms: i64, tz: &TimeZone) -> String {
    jiff::Timestamp::from_millisecond(at_ms)
        .map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string())
        .unwrap_or_else(|_| "--:--".into())
}

/// `12:04` in local time.
pub fn clock_label(at_ms: i64) -> String {
    clock_label_in(at_ms, &TimeZone::system())
}

/// E.g. `on · dry-run · 2 findings · last tick 12s ago`.
pub fn status_line(status: &WhipStatus, now_ms: i64) -> String {
    let mut parts = vec![
        if status.enabled { "on" } else { "off" }.to_string(),
        if status.dry_run { "dry-run" } else { "live" }.to_string(),
        match status.findings {
            1 => "1 finding".to_string(),
            n => format!("{n} findings"),
        },
    ];
    parts.push(match status.last_tick_ms {
        Some(t) => format!("last tick {} ago", format_age(now_ms - t)),
        None => "no tick yet".to_string(),
    });
    parts.join(" · ")
}

/// The whip log history and its view state.
#[derive(Debug, Default)]
pub struct WhipLog {
    /// Oldest first, at most [`HISTORY_LEN`].
    entries: VecDeque<WhipLogItem>,
    last_seq: u64,
    filter: LogFilter,
}

impl WhipLog {
    /// Appends newly delivered entries, skipping any already held.
    pub fn extend(&mut self, items: impl IntoIterator<Item = WhipLogItem>) {
        for item in items {
            if item.seq <= self.last_seq {
                continue;
            }
            self.last_seq = item.seq;
            if self.entries.len() >= HISTORY_LEN {
                self.entries.pop_front();
            }
            self.entries.push_back(item);
        }
    }

    /// Oldest first.
    pub fn entries(&self) -> &VecDeque<WhipLogItem> {
        &self.entries
    }

    /// Draws the sidebar section; returns the node of a clicked entry.
    pub fn section(
        &mut self,
        ui: &mut Ui,
        status: &WhipStatus,
        snapshot: &FleetSnapshot,
        now_ms: i64,
    ) -> Option<NodeId> {
        egui::CollapsingHeader::new(format!("Whip ({})", self.entries.len()))
            .id_salt("whip_log")
            .default_open(true)
            .show(ui, |ui| self.contents(ui, status, snapshot, now_ms))
            .body_returned
            .flatten()
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        status: &WhipStatus,
        snapshot: &FleetSnapshot,
        now_ms: i64,
    ) -> Option<NodeId> {
        let status_text = RichText::new(status_line(status, now_ms)).small();
        let status_text = if status.enabled {
            status_text
        } else {
            status_text.weak()
        };
        ui.add(egui::Label::new(status_text).truncate());
        ui.horizontal_wrapped(|ui| {
            for filter in LogFilter::ALL {
                let count = self.entries.iter().filter(|i| filter.matches(i)).count();
                let text = format!("{} {count}", filter.label());
                if ui.selectable_label(self.filter == filter, text).clicked() {
                    self.filter = filter;
                }
            }
        });
        let filter = self.filter;
        let shown: Vec<&WhipLogItem> = self
            .entries
            .iter()
            .rev()
            .filter(|i| filter.matches(i))
            .take(SHOWN_LEN)
            .collect();
        if shown.is_empty() {
            ui.weak("No actions yet");
            return None;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("whip_log_list")
            .max_height(LIST_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for item in shown {
                    if let Some(id) = entry_row(ui, item, snapshot) {
                        clicked = Some(id);
                    }
                }
            });
        clicked
    }
}

/// One entry; returns its node when clicked.
fn entry_row(ui: &mut Ui, item: &WhipLogItem, snapshot: &FleetSnapshot) -> Option<NodeId> {
    let entry = &item.entry;
    let link = link_node(snapshot, item);
    let target = target_text(snapshot, item, link.as_ref());
    let dark = ui.visuals().dark_mode;
    let frame = egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(clock_label(entry.at_ms)).small().weak());
            for badge in badges(item) {
                ui.label(
                    RichText::new(badge.label())
                        .small()
                        .strong()
                        .color(Color32::WHITE)
                        .background_color(badge.color(dark)),
                );
            }
            ui.add(egui::Label::new(RichText::new(&target).strong()).truncate());
        });
        let reason = entry.reason.lines().next().unwrap_or_default();
        ui.add(egui::Label::new(RichText::new(reason).small()).truncate());
    });
    let hover = format!(
        "{} → {}\n\n{}\n\nOutcome: {}{}",
        entry.action,
        entry.target,
        entry.reason,
        entry.outcome,
        if link.is_some() {
            "\n\nClick to select the node"
        } else {
            "\n\nNode no longer in the fleet"
        }
    );
    let response = frame.response.interact(Sense::click()).on_hover_text(hover);
    let response = if link.is_some() {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };
    if response.clicked() { link } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind, NodeState, RunSummary};
    use crate::whip::WhipLogEntry;

    fn item(seq: u64) -> WhipLogItem {
        WhipLogItem {
            seq,
            entry: WhipLogEntry {
                at_ms: 0,
                action: "nudge #1 via bus to dispatch:d1".into(),
                target: "worker term_w task t1".into(),
                reason: "Myrstack: no progress\nsecond line".into(),
                dry_run: true,
                outcome: "dry-run".into(),
            },
            handle: Some("term_w".into()),
            run_id: Some("r1".into()),
            task_id: Some("t1".into()),
        }
    }

    fn node(id: NodeId, label: &str) -> Node {
        Node {
            id,
            kind: NodeKind::Worker,
            label: label.into(),
            terminal_handle: None,
            live: true,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: NodeState::Working,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
        }
    }

    fn seqs(log: &WhipLog) -> Vec<u64> {
        log.entries().iter().map(|i| i.seq).collect()
    }

    #[test]
    fn history_skips_duplicates_and_stays_bounded() {
        let mut log = WhipLog::default();
        log.extend([item(1), item(2)]);
        log.extend([item(2), item(1), item(3)]);
        assert_eq!(seqs(&log), vec![1, 2, 3]);
        log.extend((4..=HISTORY_LEN as u64 + 10).map(item));
        assert_eq!(log.entries().len(), HISTORY_LEN);
        assert_eq!(log.entries().front().unwrap().seq, 11);
        assert_eq!(log.entries().back().unwrap().seq, HISTORY_LEN as u64 + 10);
    }

    #[test]
    fn badges_classify_action_and_outcome() {
        let mut i = item(1);
        assert_eq!(badges(&i), vec![Badge::Nudge, Badge::DryRun]);
        i.entry.dry_run = false;
        i.entry.outcome = "sent".into();
        assert_eq!(badges(&i), vec![Badge::Nudge]);
        i.entry.action = "escalate".into();
        i.entry.outcome = "failed: orca timed out".into();
        assert_eq!(badges(&i), vec![Badge::Escalate, Badge::Failed]);
        i.entry.action = "notify needs-input".into();
        i.entry.outcome = "notifications off".into();
        assert_eq!(badges(&i), vec![Badge::NeedsInput]);
    }

    #[test]
    fn filters() {
        let dry = item(1);
        let mut live = item(2);
        live.entry.dry_run = false;
        let mut failed = item(3);
        failed.entry.dry_run = false;
        failed.entry.outcome = "failed: boom".into();
        let pass = |f: LogFilter| {
            [&dry, &live, &failed]
                .iter()
                .map(|i| f.matches(i))
                .collect::<Vec<_>>()
        };
        assert_eq!(pass(LogFilter::All), vec![true, true, true]);
        assert_eq!(pass(LogFilter::Live), vec![false, true, true]);
        assert_eq!(pass(LogFilter::Failures), vec![false, false, true]);
    }

    #[test]
    fn links_prefer_the_task_then_terminal_then_coordinator() {
        let mut s = FleetSnapshot::default();
        let mut worker = node(NodeId::dispatch("r1", "d1"), "worker t1");
        worker.run_id = Some("r1".into());
        worker.task_id = Some("t1".into());
        worker.live = false;
        let mut term = node(NodeId::terminal("term_w"), "claude on feat");
        term.terminal_handle = Some("term_w".into());
        let coordinator = node(NodeId::terminal("term_c"), "coordinator");
        s.nodes = vec![worker, term, coordinator];
        s.runs = vec![RunSummary {
            id: "r1".into(),
            objective: None,
            coordinator: Some(NodeId::terminal("term_c")),
            task_counts: None,
        }];

        let i = item(1);
        let link = link_node(&s, &i);
        assert_eq!(link, Some(NodeId::dispatch("r1", "d1")));
        assert_eq!(target_text(&s, &i, link.as_ref()), "worker t1");

        // Unknown task: the terminal's node.
        let mut i = item(2);
        i.task_id = Some("t9".into());
        assert_eq!(link_node(&s, &i), Some(NodeId::terminal("term_w")));

        // Terminal gone: the run's coordinator.
        i.handle = Some("term_gone".into());
        assert_eq!(link_node(&s, &i), Some(NodeId::terminal("term_c")));

        // Nothing left: no link, the handle as label.
        i.run_id = Some("r_gone".into());
        assert_eq!(link_node(&s, &i), None);
        assert_eq!(target_text(&s, &i, None), "term_gone");
        i.handle = None;
        assert_eq!(target_text(&s, &i, None), "worker term_w task t1");
    }

    #[test]
    fn links_a_released_worker_by_its_last_terminal() {
        let mut s = FleetSnapshot::default();
        let mut worker = node(NodeId::dispatch("r1", "d1"), "worker");
        worker.terminal_handle = Some("term_w".into());
        worker.live = false;
        s.nodes = vec![worker];
        let mut i = item(1);
        i.task_id = None;
        assert_eq!(link_node(&s, &i), Some(NodeId::dispatch("r1", "d1")));
    }

    #[test]
    fn clock_label_formats_hours_and_minutes() {
        // 2026-09-25 12:04:30 UTC.
        let ms = 1_790_337_870_000;
        assert_eq!(clock_label_in(ms, &TimeZone::UTC), "12:04");
        let cet = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(clock_label_in(ms, &cet), "14:04");
    }

    #[test]
    fn status_line_summarises_the_whip() {
        let status = WhipStatus {
            enabled: true,
            dry_run: true,
            findings: 2,
            last_tick_ms: Some(88_000),
            ..WhipStatus::default()
        };
        assert_eq!(
            status_line(&status, 100_000),
            "on · dry-run · 2 findings · last tick 12s ago"
        );
        let off = WhipStatus {
            findings: 1,
            ..WhipStatus::default()
        };
        assert_eq!(status_line(&off, 0), "off · live · 1 finding · no tick yet");
    }
}
