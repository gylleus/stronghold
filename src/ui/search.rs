//! Command-palette search: find a node by label, branch, repo, worktree, run objective, task,
//! agent or terminal handle.

use std::collections::BTreeSet;

use eframe::egui::{self, Key, Modifiers, RichText};

use crate::fleet::FleetData;
use crate::model::{FleetSnapshot, NO_REPO, Node, NodeId, NodeKind, RepoIndex};
use crate::ui::graph::{kind_label, state_color, state_label};

/// Most results the palette lists.
pub const MAX_RESULTS: usize = 12;

/// A searchable field of a node, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Label,
    TaskTitle,
    Branch,
    Objective,
    Repo,
    Worktree,
    TaskId,
    RunId,
    Agent,
    Handle,
}

impl Field {
    /// How much a match in this field counts relative to others.
    fn weight(self) -> u32 {
        match self {
            Field::Label | Field::TaskTitle => 3,
            Field::Branch | Field::Objective | Field::Repo => 2,
            Field::Worktree | Field::TaskId | Field::RunId | Field::Agent | Field::Handle => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Field::Label => "label",
            Field::TaskTitle => "task",
            Field::Branch => "branch",
            Field::Objective => "objective",
            Field::Repo => "repo",
            Field::Worktree => "worktree",
            Field::TaskId => "task id",
            Field::RunId => "run",
            Field::Agent => "agent",
            Field::Handle => "handle",
        }
    }
}

/// One node's searchable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub fields: Vec<(Field, String)>,
}

impl SearchEntry {
    pub fn field(&self, field: Field) -> Option<&str> {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, s)| s.as_str())
    }
}

/// Search entries for every node of `snapshot`; `data` adds task titles.
pub fn search_entries(snapshot: &FleetSnapshot, data: Option<&FleetData>) -> Vec<SearchEntry> {
    let repos = RepoIndex::new(snapshot);
    snapshot
        .nodes
        .iter()
        .map(|node| entry(node, snapshot, data, &repos))
        .collect()
}

fn entry(
    node: &Node,
    snapshot: &FleetSnapshot,
    data: Option<&FleetData>,
    repos: &RepoIndex,
) -> SearchEntry {
    let run = node.run_id.as_deref();
    let objective = run
        .and_then(|r| snapshot.run(r))
        .and_then(|r| r.objective.clone());
    let task_title =
        data.zip(run)
            .zip(node.task_id.as_deref())
            .and_then(|((data, run), task_id)| {
                let task = data.tasks.get(run)?.iter().find(|t| t.id == task_id)?;
                task.task_title
                    .clone()
                    .or_else(|| task.display_name.clone())
            });
    let fields = [
        (Field::Label, Some(node.label.clone())),
        (Field::TaskTitle, task_title),
        (Field::Branch, node.branch.clone()),
        (Field::Objective, objective),
        (
            Field::Repo,
            repos
                .node(&node.id)
                .filter(|r| *r != NO_REPO && node.kind != NodeKind::Queen)
                .map(str::to_string),
        ),
        (Field::Worktree, node.worktree_path.clone()),
        (Field::TaskId, node.task_id.clone()),
        (Field::RunId, node.run_id.clone()),
        (Field::Agent, node.agent.as_ref().map(|a| a.to_string())),
        (Field::Handle, node.terminal_handle.clone()),
    ]
    .into_iter()
    .filter_map(|(f, s)| Some((f, s.filter(|s| !s.is_empty())?)))
    .collect();
    SearchEntry {
        id: node.id.clone(),
        kind: node.kind,
        label: node.label.clone(),
        fields,
    }
}

/// How well `token` (lowercase) matches `text`: exact beats prefix beats word start beats
/// substring beats a fuzzy subsequence; `None` if the letters don't occur in order.
pub fn match_score(token: &str, text: &str) -> Option<u32> {
    if token.is_empty() {
        return Some(0);
    }
    let text = text.to_lowercase();
    if text == token {
        return Some(100);
    }
    if text.starts_with(token) {
        return Some(80);
    }
    if let Some(pos) = text.find(token) {
        let at_word = text[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_alphanumeric());
        return Some(if at_word { 60 } else { 40 });
    }
    fuzzy_score(token, &text)
}

/// Subsequence match scored by compactness: 1–30.
fn fuzzy_score(token: &str, text: &str) -> Option<u32> {
    let mut chars = text.char_indices();
    let mut first = None;
    let mut last = 0;
    for wanted in token.chars() {
        let (i, _) = chars.find(|(_, c)| *c == wanted)?;
        first.get_or_insert(i);
        last = i;
    }
    let span = (last - first.unwrap_or(0) + 1) as u32;
    let len = token.chars().count() as u32;
    // A span equal to the token length is contiguous (already caught as a substring).
    Some((30 * len / span.max(len)).max(1))
}

/// A ranked search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Index into the searched entries.
    pub index: usize,
    pub score: u32,
    /// The field that matched best (the label for an empty query).
    pub field: Field,
}

/// Entries matching every whitespace-separated token of `query`, best first, at most `limit`.
/// Each token scores its best field (score × field weight); ties keep coordinators before
/// workers before terminals before subagents, then sort by label. An empty query lists entries
/// in order.
pub fn rank(entries: &[SearchEntry], query: &str, limit: usize) -> Vec<SearchHit> {
    let query = query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return (0..entries.len().min(limit))
            .map(|index| SearchHit {
                index,
                score: 0,
                field: Field::Label,
            })
            .collect();
    }
    let mut hits: Vec<SearchHit> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let mut total = 0;
            let mut best: Option<(u32, Field)> = None;
            for token in &tokens {
                let (score, field) = entry
                    .fields
                    .iter()
                    .filter_map(|(f, text)| Some((match_score(token, text)? * f.weight(), *f)))
                    .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))?;
                total += score;
                if best.is_none_or(|(s, _)| score > s) {
                    best = Some((score, field));
                }
            }
            Some(SearchHit {
                index,
                score: total,
                field: best.map_or(Field::Label, |(_, f)| f),
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        let (ea, eb) = (&entries[a.index], &entries[b.index]);
        b.score
            .cmp(&a.score)
            .then(ea.kind.cmp(&eb.kind))
            .then_with(|| ea.label.cmp(&eb.label))
            .then(a.index.cmp(&b.index))
    });
    hits.truncate(limit);
    hits
}

/// Whether `event` opens the palette: Cmd+K always, `/` only while no text field has focus.
pub fn is_open_event(event: &egui::Event, text_focused: bool) -> bool {
    match event {
        egui::Event::Key {
            key: Key::K,
            pressed: true,
            modifiers,
            ..
        } => modifiers.command_only(),
        egui::Event::Text(text) => !text_focused && text == "/",
        _ => false,
    }
}

/// Takes this frame's palette-opening events; `true` if there was one.
pub fn take_open_shortcut(ctx: &egui::Context) -> bool {
    let text_focused = ctx.text_edit_focused();
    ctx.input_mut(|i| {
        let before = i.events.len();
        i.events.retain(|e| !is_open_event(e, text_focused));
        i.events.len() != before
    })
}

/// What the user picked in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// Select and center the node.
    Select(NodeId),
    /// Clear the sidebar filters, then select and center the node.
    ClearFiltersAndSelect(NodeId),
}

/// The Cmd+K palette's state.
#[derive(Debug, Default)]
pub struct SearchPalette {
    open: bool,
    query: String,
    cursor: usize,
    focus_query: bool,
}

impl SearchPalette {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.cursor = 0;
        self.focus_query = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Draws the palette if open; returns the picked node. `hidden` are the nodes the sidebar
    /// filter hides.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: Option<&FleetSnapshot>,
        data: Option<&FleetData>,
        hidden: &BTreeSet<NodeId>,
    ) -> Option<SearchOutcome> {
        if !self.open {
            return None;
        }
        let entries = snapshot.map_or_else(Vec::new, |s| search_entries(s, data));
        let hits = rank(&entries, &self.query, MAX_RESULTS);
        self.cursor = self.cursor.min(hits.len().saturating_sub(1));

        let (up, down, enter, shift_enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
            )
        });
        if up {
            self.cursor = self.cursor.saturating_sub(1);
        }
        if down && self.cursor + 1 < hits.len() {
            self.cursor += 1;
        }
        let mut picked: Option<(usize, bool)> = None;
        if (enter || shift_enter) && !hits.is_empty() {
            picked = Some((self.cursor, shift_enter));
        }

        let area = egui::Modal::default_area(egui::Id::new("search_palette"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0));
        let modal = egui::Modal::new(egui::Id::new("search_palette"))
            .area(area)
            .show(ctx, |ui| {
                ui.set_width(520.0_f32.min(ctx.content_rect().width() - 40.0));
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search nodes: title, branch, objective, task, agent…")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_query) {
                    edit.request_focus();
                }
                if edit.changed() {
                    self.cursor = 0;
                }
                ui.separator();
                if hits.is_empty() {
                    ui.weak("No matching nodes");
                }
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for (row, hit) in hits.iter().enumerate() {
                            let entry = &entries[hit.index];
                            let node = snapshot.and_then(|s| s.node(&entry.id));
                            let is_hidden = hidden.contains(&entry.id);
                            let response =
                                result_row(ui, entry, hit, node, is_hidden, row == self.cursor);
                            if row == self.cursor && (up || down) {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                picked = Some((row, false));
                            }
                        }
                    });
                ui.separator();
                ui.weak(
                    "↑↓ move · Enter select · Shift+Enter clear filters and select · Esc close",
                );
            });
        if modal.should_close() {
            self.open = false;
        }
        let (row, clear) = picked?;
        let id = entries[hits.get(row)?.index].id.clone();
        self.open = false;
        Some(if clear && hidden.contains(&id) {
            SearchOutcome::ClearFiltersAndSelect(id)
        } else {
            SearchOutcome::Select(id)
        })
    }
}

fn result_row(
    ui: &mut egui::Ui,
    entry: &SearchEntry,
    hit: &SearchHit,
    node: Option<&Node>,
    is_hidden: bool,
    highlighted: bool,
) -> egui::Response {
    let frame = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 3))
        .corner_radius(4.0)
        .fill(if highlighted {
            ui.visuals().selection.bg_fill.gamma_multiply(0.5)
        } else {
            egui::Color32::TRANSPARENT
        });
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            if let Some(node) = node {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    5.0,
                    state_color(node.state, ui.visuals().dark_mode),
                );
            }
            ui.add(egui::Label::new(RichText::new(&entry.label).strong()).truncate());
            if is_hidden {
                ui.label(
                    RichText::new("hidden by filter")
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            }
        });
        let mut meta = vec![kind_label(entry.kind).to_string()];
        if let Some(node) = node {
            meta.push(state_label(node.state).to_string());
        }
        if let Some(run) = entry
            .field(Field::Objective)
            .or_else(|| entry.field(Field::RunId))
        {
            meta.push(run.to_string());
        }
        if hit.field != Field::Label
            && let Some(text) = entry.field(hit.field)
        {
            meta.push(format!("{}: {text}", hit.field.name()));
        }
        ui.add(egui::Label::new(RichText::new(meta.join(" · ")).small().weak()).truncate());
    });
    ui.interact(
        inner.response.rect,
        ui.id().with(("search_row", &entry.id)),
        egui::Sense::click(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeState, RunSummary};
    use crate::orca::{Task, TaskStatus};

    fn node(id: &str, kind: NodeKind, label: &str) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind,
            label: label.to_string(),
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

    fn entry(id: &str, kind: NodeKind, fields: &[(Field, &str)]) -> SearchEntry {
        let label = fields
            .iter()
            .find(|(f, _)| *f == Field::Label)
            .map_or(id, |(_, s)| s);
        SearchEntry {
            id: NodeId(id.to_string()),
            kind,
            label: label.to_string(),
            fields: fields.iter().map(|(f, s)| (*f, s.to_string())).collect(),
        }
    }

    fn ids(entries: &[SearchEntry], hits: &[SearchHit]) -> Vec<String> {
        hits.iter().map(|h| entries[h.index].id.0.clone()).collect()
    }

    #[test]
    fn match_score_orders_match_quality() {
        let exact = match_score("pay", "Pay").unwrap();
        let prefix = match_score("pay", "payments").unwrap();
        let word = match_score("pay", "stripe-payments").unwrap();
        let inner = match_score("pay", "repayments").unwrap();
        let fuzzy = match_score("pmt", "payments").unwrap();
        assert!(exact > prefix && prefix > word && word > inner && inner > fuzzy);
        assert!(fuzzy >= 1);
        assert_eq!(match_score("xyz", "payments"), None);
        assert_eq!(match_score("tp", "pt"), None);
    }

    #[test]
    fn fuzzy_prefers_compact_matches() {
        let compact = match_score("fpy", "feat-pay").unwrap();
        let spread = match_score("fpy", "fix the parser yesterday").unwrap();
        assert!(compact > spread);
    }

    #[test]
    fn rank_prefers_label_over_weak_fields() {
        let entries = vec![
            entry(
                "w",
                NodeKind::Worker,
                &[(Field::Label, "cleanup"), (Field::Worktree, "/x/checkout")],
            ),
            entry("c", NodeKind::Worker, &[(Field::Label, "checkout flow")]),
        ];
        let hits = rank(&entries, "checkout", 10);
        assert_eq!(ids(&entries, &hits), ["c", "w"]);
        assert_eq!(hits[0].field, Field::Label);
        assert_eq!(hits[1].field, Field::Worktree);
    }

    #[test]
    fn rank_requires_every_token() {
        let entries = vec![
            entry(
                "a",
                NodeKind::Worker,
                &[(Field::Label, "pay intents"), (Field::Branch, "feat/pay")],
            ),
            entry(
                "b",
                NodeKind::Worker,
                &[(Field::Label, "pay refunds"), (Field::Agent, "codex")],
            ),
        ];
        assert_eq!(ids(&entries, &rank(&entries, "pay codex", 10)), ["b"]);
        assert_eq!(ids(&entries, &rank(&entries, "PAY", 10)).len(), 2);
        assert!(rank(&entries, "pay nothing", 10).is_empty());
    }

    #[test]
    fn rank_ties_by_kind_then_label_and_limits() {
        let entries = vec![
            entry("t", NodeKind::Terminal, &[(Field::Label, "alpha")]),
            entry("w2", NodeKind::Worker, &[(Field::Label, "alpha")]),
            entry("c", NodeKind::Coordinator, &[(Field::Label, "alpha")]),
        ];
        assert_eq!(
            ids(&entries, &rank(&entries, "alpha", 10)),
            ["c", "w2", "t"]
        );
        assert_eq!(rank(&entries, "alpha", 2).len(), 2);
    }

    #[test]
    fn empty_query_lists_entries_in_order() {
        let entries = vec![
            entry("b", NodeKind::Worker, &[(Field::Label, "b")]),
            entry("a", NodeKind::Worker, &[(Field::Label, "a")]),
        ];
        assert_eq!(ids(&entries, &rank(&entries, "  ", 10)), ["b", "a"]);
        assert_eq!(rank(&entries, "", 1).len(), 1);
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn open_shortcuts() {
        assert!(is_open_event(&key(Key::K, Modifiers::COMMAND), false));
        assert!(is_open_event(&key(Key::K, Modifiers::COMMAND), true));
        assert!(!is_open_event(&key(Key::K, Modifiers::NONE), false));
        assert!(!is_open_event(
            &key(Key::K, Modifiers::COMMAND | Modifiers::SHIFT),
            false
        ));
        let slash = egui::Event::Text("/".into());
        assert!(is_open_event(&slash, false));
        assert!(!is_open_event(&slash, true));
    }

    #[test]
    fn entries_include_objective_and_task_title() {
        let mut snapshot = FleetSnapshot::default();
        snapshot.runs = vec![RunSummary {
            id: "run1".into(),
            objective: Some("Migrate checkout".into()),
            coordinator: None,
            task_counts: None,
        }];
        let mut w = node("w", NodeKind::Worker, "w1");
        w.run_id = Some("run1".into());
        w.task_id = Some("task-7".into());
        w.branch = Some("feat/stripe".into());
        w.terminal_handle = Some("term_abc".into());
        w.worktree_path = Some("/Users/x/repo/hackathon/commerce-platform".into());
        snapshot.nodes = vec![w];

        let mut data = FleetData::default();
        data.tasks.insert(
            "run1".into(),
            vec![Task {
                id: "task-7".into(),
                task_title: Some("Wire up payment intents".into()),
                status: TaskStatus::Ready,
                ..serde_json::from_str::<Task>(r#"{"id":"x","status":"ready"}"#).unwrap()
            }],
        );
        let entries = search_entries(&snapshot, Some(&data));
        let e = &entries[0];
        assert_eq!(e.field(Field::Objective), Some("Migrate checkout"));
        assert_eq!(e.field(Field::TaskTitle), Some("Wire up payment intents"));
        assert_eq!(e.field(Field::TaskId), Some("task-7"));
        assert_eq!(e.field(Field::Handle), Some("term_abc"));
        assert_eq!(e.field(Field::Repo), Some("commerce-platform"));
        let hits = rank(&entries, "commerce", 5);
        assert_eq!(hits[0].field, Field::Repo);

        for query in [
            "payment", "stripe", "checkout", "task-7", "term_abc", "run1",
        ] {
            assert_eq!(rank(&entries, query, 5).len(), 1, "{query}");
        }
        assert_eq!(rank(&entries, "intents", 5)[0].field, Field::TaskTitle);
        assert_eq!(
            search_entries(&snapshot, None)[0].field(Field::TaskTitle),
            None
        );
    }

    #[test]
    fn search_finds_the_queen() {
        let snapshot = FleetSnapshot::build(&FleetData::default(), 0);
        let entries = search_entries(&snapshot, None);
        let hits = rank(&entries, "que", MAX_RESULTS);
        assert_eq!(entries[hits[0].index].id, NodeId::queen());
    }
}
