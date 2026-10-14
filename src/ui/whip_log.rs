//! Whip action log: the sidebar section listing every whip decision, newest first.
//!
//! [`WhipLog`] keeps the history the runner delivers (each entry once, by sequence number) and
//! draws it. Entries link to their nodes: clicking one selects and centers the node.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Sense, Ui};
use jiff::tz::TimeZone;

use crate::model::{ClanSnapshot, NodeId};
use crate::ui::clan_view::format_age;
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
    /// Sent because the user asked ("Whip"), not by a tick.
    Manual,
    DryRun,
    Failed,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nudge => "NUDGE",
            Self::Escalate => "ESCALATE",
            Self::NeedsInput => "NEEDS INPUT",
            Self::Manual => "MANUAL",
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
            Self::Manual => (
                Color32::from_rgb(170, 120, 230),
                Color32::from_rgb(110, 60, 170),
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

/// The action badge of an entry, MANUAL for a manual whip, then DRY-RUN or FAILED when that
/// applies.
pub fn badges(item: &WhipLogItem) -> Vec<Badge> {
    let entry = &item.entry;
    let mut badges = Vec::with_capacity(3);
    if entry.action.starts_with("nudge") {
        badges.push(Badge::Nudge);
    } else if entry.action.starts_with("escalate") {
        badges.push(Badge::Escalate);
    } else if entry.action.contains("needs-input") {
        badges.push(Badge::NeedsInput);
    }
    if entry.manual {
        badges.push(Badge::Manual);
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
pub fn link_node(snapshot: &ClanSnapshot, item: &WhipLogItem) -> Option<NodeId> {
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
pub fn target_text(snapshot: &ClanSnapshot, item: &WhipLogItem, link: Option<&NodeId>) -> String {
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
    /// Entries matching each filter of [`LogFilter::ALL`], kept up to date by [`Self::extend`].
    counts: [usize; LogFilter::ALL.len()],
    /// The rows [`Self::section`] shows, derived from the entries, the filter and the snapshot;
    /// `None` when one of those changed.
    rows: Option<Vec<RowView>>,
    /// Drawn height of one row; 0 until the first row is drawn.
    row_height: f32,
}

/// Everything drawn for one entry, computed once per entry, filter and snapshot rather than
/// every frame.
#[derive(Debug, Clone, PartialEq)]
struct RowView {
    link: Option<NodeId>,
    clock: String,
    badges: Vec<Badge>,
    target: String,
    reason: String,
    hover: String,
}

impl RowView {
    fn new(item: &WhipLogItem, snapshot: &ClanSnapshot, tz: &TimeZone) -> Self {
        let entry = &item.entry;
        let link = link_node(snapshot, item);
        let target = target_text(snapshot, item, link.as_ref());
        let hover = format!(
            "{} → {}\n\n{}\n\nOutcome: {}{}",
            entry.action,
            entry.target,
            entry.reason,
            entry.outcome,
            if link.is_some() {
                "\n\nClick to select the node"
            } else {
                "\n\nNode no longer in the clan"
            }
        );
        Self {
            link,
            clock: clock_label_in(entry.at_ms, tz),
            badges: badges(item),
            target,
            reason: entry.reason.lines().next().unwrap_or_default().to_owned(),
            hover,
        }
    }
}

impl WhipLog {
    /// Appends newly delivered entries, skipping any already held.
    pub fn extend(&mut self, items: impl IntoIterator<Item = WhipLogItem>) {
        for item in items {
            if item.seq <= self.last_seq {
                continue;
            }
            self.last_seq = item.seq;
            if self.entries.len() >= HISTORY_LEN
                && let Some(old) = self.entries.pop_front()
            {
                self.count(&old, false);
            }
            self.count(&item, true);
            self.entries.push_back(item);
            self.rows = None;
        }
    }

    fn count(&mut self, item: &WhipLogItem, add: bool) {
        for (count, filter) in self.counts.iter_mut().zip(LogFilter::ALL) {
            if filter.matches(item) {
                *count = if add { *count + 1 } else { *count - 1 };
            }
        }
    }

    /// Call when the snapshot changes: entries link to its nodes.
    pub fn snapshot_changed(&mut self) {
        self.rows = None;
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
        snapshot: &ClanSnapshot,
        now_ms: i64,
    ) -> Option<NodeId> {
        egui::CollapsingHeader::new(format!("Whip ({})", self.entries.len()))
            .id_salt("whip_log")
            .default_open(true)
            .show(ui, |ui| self.contents(ui, status, snapshot, now_ms))
            .body_returned
            .flatten()
    }

    /// The shown rows, newest first, rebuilt if stale.
    fn rows(&mut self, snapshot: &ClanSnapshot) -> &[RowView] {
        let (entries, filter) = (&self.entries, self.filter);
        self.rows.get_or_insert_with(|| {
            let tz = TimeZone::system();
            entries
                .iter()
                .rev()
                .filter(|i| filter.matches(i))
                .take(SHOWN_LEN)
                .map(|i| RowView::new(i, snapshot, &tz))
                .collect()
        })
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        status: &WhipStatus,
        snapshot: &ClanSnapshot,
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
            for (filter, count) in LogFilter::ALL.into_iter().zip(self.counts) {
                let text = format!("{} {count}", filter.label());
                if ui.selectable_label(self.filter == filter, text).clicked()
                    && self.filter != filter
                {
                    self.filter = filter;
                    self.rows = None;
                }
            }
        });
        let estimate = self.row_height;
        let rows = self.rows(snapshot);
        if rows.is_empty() {
            ui.weak("No actions yet");
            return None;
        }
        let mut clicked = None;
        let mut measured = None;
        // Only the rows in view are laid out: the list can hold [`SHOWN_LEN`] entries.
        let row_height = if estimate > 0.0 {
            estimate
        } else {
            estimated_row_height(ui)
        };
        egui::ScrollArea::vertical()
            .id_salt("whip_log_list")
            .max_height(LIST_HEIGHT)
            .auto_shrink([false, true])
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                for row in &rows[range] {
                    let response = entry_row(ui, row);
                    measured.get_or_insert(response.rect.height());
                    if response.clicked() {
                        clicked.clone_from(&row.link);
                    }
                }
            });
        if let Some(height) = measured
            && (height - self.row_height).abs() > 0.5
        {
            self.row_height = height;
            ui.ctx().request_repaint();
        }
        clicked
    }
}

/// A row's height before one has been drawn: a line of labels and a line of small text in a
/// group frame.
fn estimated_row_height(ui: &Ui) -> f32 {
    let frame = egui::Frame::group(ui.style());
    ui.spacing().interact_size.y
        + ui.spacing().item_spacing.y
        + ui.text_style_height(&egui::TextStyle::Small)
        + frame.total_margin().sum().y
}

/// One entry; the response is clickable when the entry links to a node.
fn entry_row(ui: &mut Ui, row: &RowView) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let frame = egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&row.clock).small().weak());
            for badge in &row.badges {
                ui.label(
                    RichText::new(badge.label())
                        .small()
                        .strong()
                        .color(Color32::WHITE)
                        .background_color(badge.color(dark)),
                );
            }
            ui.add(egui::Label::new(RichText::new(&row.target).strong()).truncate());
        });
        ui.add(egui::Label::new(RichText::new(&row.reason).small()).truncate());
    });
    let response = frame.response.interact(Sense::click()).on_hover_ui(|ui| {
        ui.label(&row.hover);
    });
    if row.link.is_some() {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
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
                reason: "Stronghold: no progress\nsecond line".into(),
                dry_run: true,
                outcome: "dry-run".into(),
                manual: false,
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
            parked_at_ms: None,
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
    fn filter_counts_follow_the_bounded_history() {
        let mut log = WhipLog::default();
        let mut failed = item(1);
        failed.entry.dry_run = false;
        failed.entry.outcome = "failed: boom".into();
        log.extend([failed]);
        assert_eq!(log.counts, [1, 1, 1]);
        log.extend((2..=HISTORY_LEN as u64).map(item));
        assert_eq!(log.counts, [HISTORY_LEN, 1, 1]);
        // The failed entry is evicted.
        log.extend([item(HISTORY_LEN as u64 + 1)]);
        assert_eq!(log.counts, [HISTORY_LEN, 0, 0]);
        let recount: Vec<usize> = LogFilter::ALL
            .iter()
            .map(|f| log.entries().iter().filter(|i| f.matches(i)).count())
            .collect();
        assert_eq!(log.counts.to_vec(), recount);
    }

    #[test]
    fn rows_are_cached_until_entries_or_snapshot_change() {
        let mut s = ClanSnapshot::default();
        let mut log = WhipLog::default();
        log.extend((1..=SHOWN_LEN as u64 + 5).map(item));
        let rows = log.rows(&s).to_vec();
        assert_eq!(rows.len(), SHOWN_LEN);
        assert_eq!(rows[0].target, "term_w");
        assert_eq!(rows[0].link, None);
        assert_eq!(rows[0].reason, "Stronghold: no progress");

        // A new snapshot is only seen after `snapshot_changed`.
        let mut term = node(NodeId::terminal("term_w"), "claude on feat");
        term.terminal_handle = Some("term_w".into());
        s.nodes = vec![term];
        assert_eq!(log.rows(&s)[0].target, "term_w");
        log.snapshot_changed();
        assert_eq!(log.rows(&s)[0].target, "claude on feat");
        assert_eq!(log.rows(&s)[0].link, Some(NodeId::terminal("term_w")));

        log.extend([item(1000)]);
        assert!(log.rows.is_none());
    }

    #[test]
    fn draws_only_rows_in_view() {
        let s = ClanSnapshot::default();
        let mut log = WhipLog::default();
        log.extend((1..=SHOWN_LEN as u64).map(item));
        let ctx = egui::Context::default();
        let mut labels = 0;
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                log.section(ui, &WhipStatus::default(), &s, 0);
            });
            labels = output
                .shapes
                .iter()
                .filter(|c| matches!(c.shape, egui::Shape::Text(_)))
                .count();
            output.textures_delta.clear();
        }
        assert!(log.row_height > 0.0);
        // Two lines per row plus the header, status and filters; far fewer than all rows.
        assert!(labels < SHOWN_LEN / 2, "{labels} text shapes");
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
        let mut manual = item(9);
        manual.entry.action = "nudge via terminal input".into();
        manual.entry.dry_run = false;
        manual.entry.manual = true;
        assert_eq!(badges(&manual), vec![Badge::Nudge, Badge::Manual]);
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
        let mut s = ClanSnapshot::default();
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
        let mut s = ClanSnapshot::default();
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
