//! The Cmd+K command palette: find a node by label, branch, repo, worktree, run objective,
//! task, agent or terminal handle, or run an action (see [`crate::ui::commands`]).

use std::collections::BTreeSet;

use eframe::egui::{self, Key, Modifiers, RichText};

use crate::fleet::FleetData;
use crate::model::{FleetSnapshot, NO_REPO, Node, NodeId, NodeKind, RepoIndex};
use crate::ui::commands::{self, ACTIONS, ActionContext, ActionId, Recents};
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
            let fields: Vec<(Field, u32, &str)> = entry
                .fields
                .iter()
                .map(|(f, text)| (*f, f.weight(), text.as_str()))
                .collect();
            let (score, field) = score_tokens(&tokens, &fields, 0)?;
            Some(SearchHit {
                index,
                score,
                field: field.unwrap_or(Field::Label),
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

/// The summed score of `tokens` against weighted `fields` — each token scores its best field
/// (score × weight) — and the field with the best single token score. `None` unless every
/// token matches somewhere with a [`match_score`] of at least `min`; ties between fields go to
/// the one listed first.
fn score_tokens<F: Copy>(
    tokens: &[&str],
    fields: &[(F, u32, &str)],
    min: u32,
) -> Option<(u32, Option<F>)> {
    let mut total = 0;
    let mut best: Option<(u32, F)> = None;
    for token in tokens {
        let (score, field) = fields
            .iter()
            .enumerate()
            .filter_map(|(i, (f, weight, text))| {
                let score = match_score(token, text).filter(|s| *s >= min)?;
                Some((score * weight, i, *f))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
            .map(|(score, _, f)| (score, f))?;
        total += score;
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, field));
        }
    }
    Some((total, best.map(|(_, f)| f)))
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

/// Nodes listed under an empty query, after the actions.
pub const EMPTY_QUERY_NODES: usize = 6;
/// Actions offered under an empty query before any was run.
const SUGGESTED: [ActionId; 3] = [
    ActionId::OpenChat,
    ActionId::StatusReport,
    ActionId::WhipStalled,
];

/// A palette entry: a ranked node or an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteItem {
    Node(SearchHit),
    Action(ActionId),
}

/// A palette row and the section title drawn above it, if it starts a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    pub item: PaletteItem,
    pub header: Option<&'static str>,
}

/// The palette's rows for `query`. Empty: recent actions (or a few suggestions), then the
/// first nodes. `>` prefix: actions only (all of them, recents first, for a bare `>`).
/// Otherwise nodes and actions ranked together, actions first on equal scores.
pub fn palette_rows(
    entries: &[SearchEntry],
    query: &str,
    recents: &Recents,
    cx: &ActionContext,
) -> Vec<PaletteRow> {
    let trimmed = query.trim_start();
    let (actions_only, query) = match trimmed.strip_prefix('>') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let lower = query.trim().to_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let mut rows = Vec::new();
    let mut section = |title: &'static str, items: Vec<PaletteItem>| {
        rows.extend(items.into_iter().enumerate().map(|(i, item)| PaletteRow {
            item,
            header: (i == 0).then_some(title),
        }));
    };
    let recent: Vec<PaletteItem> = recents.ids().map(PaletteItem::Action).collect();
    if tokens.is_empty() {
        if actions_only {
            let rest = ACTIONS
                .iter()
                .map(|a| a.id)
                .filter(|id| !recents.ids().any(|r| r == *id))
                .map(PaletteItem::Action)
                .collect();
            section("Recent actions", recent);
            section("Actions", rest);
        } else {
            if recent.is_empty() {
                section(
                    "Suggested actions",
                    SUGGESTED.into_iter().map(PaletteItem::Action).collect(),
                );
            } else {
                section("Recent actions", recent);
            }
            let nodes = rank(entries, "", EMPTY_QUERY_NODES);
            section("Nodes", nodes.into_iter().map(PaletteItem::Node).collect());
        }
        return rows;
    }

    let actions = rank_actions(&tokens, cx);
    if actions_only {
        return actions
            .into_iter()
            .map(|(id, _)| PaletteRow {
                item: PaletteItem::Action(id),
                header: None,
            })
            .collect();
    }
    let mut merged: Vec<(u32, bool, PaletteItem)> = actions
        .into_iter()
        .map(|(id, score)| (score, false, PaletteItem::Action(id)))
        .chain(
            rank(entries, query, MAX_RESULTS)
                .into_iter()
                .map(|hit| (hit.score, true, PaletteItem::Node(hit))),
        )
        .collect();
    // Stable: each list keeps its own order within a score.
    merged.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    merged.truncate(MAX_RESULTS);
    merged
        .into_iter()
        .map(|(_, _, item)| PaletteRow { item, header: None })
        .collect()
}

/// Weakest [`match_score`] an action accepts: a substring. Fuzzy matches would list most
/// actions for any short query.
const MIN_ACTION_MATCH: u32 = 40;

/// Actions matching every token with their scores, best first; the title counts like a node's
/// label, keywords like a branch. Enabled actions come first on equal scores, then registry
/// order.
fn rank_actions(tokens: &[&str], cx: &ActionContext) -> Vec<(ActionId, u32)> {
    let mut hits: Vec<(usize, ActionId, u32)> = ACTIONS
        .iter()
        .enumerate()
        .filter_map(|(order, action)| {
            let title = commands::title(action.id, cx);
            let fields = [
                ((), Field::Label.weight(), title.as_str()),
                ((), Field::Label.weight(), action.title),
                ((), Field::Branch.weight(), action.keywords),
            ];
            let (score, _) = score_tokens(tokens, &fields, MIN_ACTION_MATCH)?;
            Some((order, action.id, score))
        })
        .collect();
    hits.sort_by_key(|(order, id, score)| {
        (
            std::cmp::Reverse(*score),
            commands::disabled_reason(*id, cx).is_some(),
            *order,
        )
    });
    hits.into_iter().map(|(_, id, score)| (id, score)).collect()
}

/// What the user picked in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// Select and center the node.
    Select(NodeId),
    /// Clear the sidebar filters, then select and center the node.
    ClearFiltersAndSelect(NodeId),
    /// Run the action.
    Run(ActionId),
}

/// What picking `item` does; `None` for a disabled action (it cannot run). `clear` is
/// Shift+Enter: clear the filters first if the node is hidden by them.
pub fn activate(
    item: &PaletteItem,
    entries: &[SearchEntry],
    cx: &ActionContext,
    hidden: &BTreeSet<NodeId>,
    clear: bool,
) -> Option<SearchOutcome> {
    match item {
        PaletteItem::Action(id) => commands::disabled_reason(*id, cx)
            .is_none()
            .then_some(SearchOutcome::Run(*id)),
        PaletteItem::Node(hit) => {
            let id = entries.get(hit.index)?.id.clone();
            Some(if clear && hidden.contains(&id) {
                SearchOutcome::ClearFiltersAndSelect(id)
            } else {
                SearchOutcome::Select(id)
            })
        }
    }
}

/// The Cmd+K palette's state.
#[derive(Debug, Default)]
pub struct SearchPalette {
    open: bool,
    query: String,
    cursor: usize,
    focus_query: bool,
    recents: Recents,
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

    /// Recently run actions, newest first.
    pub fn recents(&self) -> &Recents {
        &self.recents
    }

    pub fn set_recents(&mut self, recents: Recents) {
        self.recents = recents;
    }

    /// Draws the palette if open; returns the picked node or action. `hidden` are the nodes
    /// the sidebar filter hides; `cx` decides the actions' titles and which can run.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: Option<&FleetSnapshot>,
        data: Option<&FleetData>,
        hidden: &BTreeSet<NodeId>,
        cx: &ActionContext,
    ) -> Option<SearchOutcome> {
        if !self.open {
            return None;
        }
        let entries = snapshot.map_or_else(Vec::new, |s| search_entries(s, data));
        let rows = palette_rows(&entries, &self.query, &self.recents, cx);
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));

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
        if down && self.cursor + 1 < rows.len() {
            self.cursor += 1;
        }
        let mut picked: Option<(usize, bool)> = None;
        if (enter || shift_enter) && !rows.is_empty() {
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
                        .hint_text("Search nodes and actions (> for actions only)…")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_query) {
                    edit.request_focus();
                }
                if edit.changed() {
                    self.cursor = 0;
                }
                ui.separator();
                if rows.is_empty() {
                    ui.weak("No matching nodes or actions");
                }
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for (i, row) in rows.iter().enumerate() {
                            if let Some(header) = row.header {
                                ui.add_space(2.0);
                                ui.label(RichText::new(header).small().strong().weak());
                            }
                            let highlighted = i == self.cursor;
                            let response = match &row.item {
                                PaletteItem::Node(hit) => {
                                    let entry = &entries[hit.index];
                                    let node = snapshot.and_then(|s| s.node(&entry.id));
                                    let is_hidden = hidden.contains(&entry.id);
                                    result_row(ui, entry, hit, node, is_hidden, highlighted)
                                }
                                PaletteItem::Action(id) => action_row(ui, *id, cx, highlighted),
                            };
                            if highlighted && (up || down) {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                picked = Some((i, false));
                            }
                        }
                    });
                ui.separator();
                ui.weak(
                    "↑↓ move · Enter run / select · Shift+Enter clear filters and select · \
                     > actions only · Esc close",
                );
            });
        if modal.should_close() {
            self.open = false;
        }
        let (i, clear) = picked?;
        let outcome = activate(&rows.get(i)?.item, &entries, cx, hidden, clear)?;
        if let SearchOutcome::Run(id) = outcome {
            self.recents.push(id);
        }
        self.open = false;
        Some(outcome)
    }
}

fn row_frame(ui: &egui::Ui, highlighted: bool) -> egui::Frame {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 3))
        .corner_radius(4.0)
        .fill(if highlighted {
            ui.visuals().selection.bg_fill.gamma_multiply(0.5)
        } else {
            egui::Color32::TRANSPARENT
        })
}

/// An action: an "action" badge, the title (greyed with its reason when it cannot run) and
/// the shortcut right-aligned.
fn action_row(
    ui: &mut egui::Ui,
    id: ActionId,
    cx: &ActionContext,
    highlighted: bool,
) -> egui::Response {
    let def = id.def();
    let reason = commands::disabled_reason(id, cx);
    let inner = row_frame(ui, highlighted).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(4, 0))
                .corner_radius(3.0)
                .fill(ui.visuals().faint_bg_color)
                .show(ui, |ui| ui.label(RichText::new("action").small().weak()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(shortcut) = def.shortcut {
                    ui.label(RichText::new(shortcut).monospace().weak());
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let text = RichText::new(commands::title(id, cx));
                    let text = if reason.is_some() {
                        text.weak()
                    } else {
                        text.strong()
                    };
                    ui.add(egui::Label::new(text).truncate());
                });
            });
        });
        if let Some(reason) = reason {
            ui.label(RichText::new(reason).small().weak());
        }
    });
    let sense = if reason.is_some() {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    ui.interact(
        inner.response.rect,
        ui.id().with(("palette_action", def.key)),
        sense,
    )
}

fn result_row(
    ui: &mut egui::Ui,
    entry: &SearchEntry,
    hit: &SearchHit,
    node: Option<&Node>,
    is_hidden: bool,
    highlighted: bool,
) -> egui::Response {
    let inner = row_frame(ui, highlighted).show(ui, |ui| {
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
    fn actions(rows: &[PaletteRow]) -> Vec<ActionId> {
        rows.iter()
            .filter_map(|r| match r.item {
                PaletteItem::Action(id) => Some(id),
                PaletteItem::Node(_) => None,
            })
            .collect()
    }

    fn row_names(entries: &[SearchEntry], rows: &[PaletteRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.item {
                PaletteItem::Action(id) => format!(">{}", id.def().key),
                PaletteItem::Node(hit) => entries[hit.index].id.0.clone(),
            })
            .collect()
    }

    fn ready() -> ActionContext {
        ActionContext {
            has_data: true,
            ..ActionContext::default()
        }
    }

    #[test]
    fn nodes_and_actions_rank_together() {
        let entries = vec![
            entry("whipper", NodeKind::Worker, &[(Field::Label, "whipper")]),
            entry(
                "fits",
                NodeKind::Worker,
                &[(Field::Worktree, "/x/fit-thing")],
            ),
            entry("chat-node", NodeKind::Worker, &[(Field::Label, "chat")]),
        ];
        let cx = ready();
        // "fit": the action's title prefix beats the node's worktree word match.
        let rows = palette_rows(&entries, "fit", &Recents::default(), &cx);
        assert_eq!(row_names(&entries, &rows)[..2], [">fit_graph", "fits"]);
        // An exact node label outranks an action matching only by keyword.
        let rows = palette_rows(&entries, "chat", &Recents::default(), &cx);
        assert_eq!(row_names(&entries, &rows)[0], "chat-node");
        assert!(actions(&rows).contains(&ActionId::OpenChat));
        // Every token must match an action's title or keywords.
        let rows = palette_rows(&entries, "whip stalled", &Recents::default(), &cx);
        assert_eq!(actions(&rows), [ActionId::WhipStalled]);
        // Equal scores list actions first, enabled before disabled.
        let rows = palette_rows(&entries, "whip", &Recents::default(), &cx);
        let names = row_names(&entries, &rows);
        assert_eq!(names[0], ">whip_stalled", "{names:?}");
        assert!(names.contains(&"whipper".to_string()));
        assert!(rows.iter().all(|r| r.header.is_none()));
        // Dynamic titles are searchable, and so are the fixed ones.
        let rows = palette_rows(&entries, "hide loose", &Recents::default(), &cx);
        assert_eq!(actions(&rows), [ActionId::ToggleLoose]);
        let rows = palette_rows(&entries, "toggle loose", &Recents::default(), &cx);
        assert_eq!(actions(&rows), [ActionId::ToggleLoose]);
    }

    #[test]
    fn action_prefix_lists_actions_only() {
        let entries = vec![entry(
            "fit-node",
            NodeKind::Worker,
            &[(Field::Label, "fit")],
        )];
        let cx = ready();
        let mut recents = Recents::default();
        recents.push(ActionId::KeyboardHelp);
        let rows = palette_rows(&entries, ">", &recents, &cx);
        assert_eq!(rows.len(), ACTIONS.len());
        assert_eq!(actions(&rows).len(), ACTIONS.len());
        assert_eq!(rows[0].item, PaletteItem::Action(ActionId::KeyboardHelp));
        assert_eq!(rows[0].header, Some("Recent actions"));
        assert_eq!(rows[1].header, Some("Actions"));
        assert_eq!(rows[1].item, PaletteItem::Action(ACTIONS[0].id));

        let rows = palette_rows(&entries, " > fit", &recents, &cx);
        assert_eq!(actions(&rows), [ActionId::FitGraph]);
        assert_eq!(rows.len(), 1, "the node named fit is left out");
        assert!(palette_rows(&entries, ">zzzz", &recents, &cx).is_empty());
    }

    #[test]
    fn empty_query_shows_recents_then_nodes() {
        let entries: Vec<SearchEntry> = (0..10)
            .map(|i| {
                let id = format!("n{i}");
                entry(&id, NodeKind::Worker, &[(Field::Label, &id)])
            })
            .collect();
        let cx = ready();
        let rows = palette_rows(&entries, "", &Recents::default(), &cx);
        assert_eq!(rows[0].header, Some("Suggested actions"));
        assert_eq!(actions(&rows), SUGGESTED);

        let mut recents = Recents::default();
        for id in [ActionId::FitGraph, ActionId::OpenChat, ActionId::FitGraph] {
            recents.push(id);
        }
        let rows = palette_rows(&entries, "  ", &recents, &cx);
        assert_eq!(
            row_names(&entries, &rows),
            [
                ">fit_graph",
                ">open_chat",
                "n0",
                "n1",
                "n2",
                "n3",
                "n4",
                "n5"
            ]
        );
        assert_eq!(rows[0].header, Some("Recent actions"));
        assert_eq!(rows[1].header, None);
        assert_eq!(rows[2].header, Some("Nodes"));
    }

    #[test]
    fn disabled_actions_do_not_run() {
        let entries = vec![entry("a", NodeKind::Worker, &[(Field::Label, "a")])];
        let hidden = BTreeSet::from([NodeId("a".into())]);
        let cx = ready();
        let whip = PaletteItem::Action(ActionId::WhipSelected);
        assert_eq!(activate(&whip, &entries, &cx, &hidden, false), None);
        let with_selection = ActionContext {
            whip_targets: 1,
            ..cx.clone()
        };
        assert_eq!(
            activate(&whip, &entries, &with_selection, &hidden, false),
            Some(SearchOutcome::Run(ActionId::WhipSelected))
        );
        let node = PaletteItem::Node(SearchHit {
            index: 0,
            score: 0,
            field: Field::Label,
        });
        assert_eq!(
            activate(&node, &entries, &cx, &hidden, true),
            Some(SearchOutcome::ClearFiltersAndSelect(NodeId("a".into())))
        );
        assert_eq!(
            activate(&node, &entries, &cx, &BTreeSet::new(), true),
            Some(SearchOutcome::Select(NodeId("a".into())))
        );
    }

    /// Runs `palette` for a few frames with `events` in the last one; returns what it picked
    /// and the last frame's output.
    fn run_palette(
        palette: &mut SearchPalette,
        snapshot: &FleetSnapshot,
        cx: &ActionContext,
        events: Vec<egui::Event>,
    ) -> (Option<SearchOutcome>, egui::FullOutput) {
        let ctx = egui::Context::default();
        let mut outcome = None;
        let mut output = None;
        for i in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                time: Some(f64::from(i) / 60.0),
                events: if i == 2 { events.clone() } else { vec![] },
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let picked = palette.show(ui.ctx(), Some(snapshot), None, &BTreeSet::new(), cx);
                outcome = outcome.take().or(picked);
            });
            out.textures_delta.clear();
            output = Some(out);
        }
        (outcome, output.unwrap())
    }

    #[test]
    fn palette_renders_actions_with_shortcuts_and_reasons() {
        let snapshot = FleetSnapshot::build(&FleetData::default(), 0);
        let cx = ready();
        let mut palette = SearchPalette::default();
        palette.open();
        palette.query = ">".into();
        let (outcome, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert_eq!(outcome, None);
        use crate::ui::test_util::text_center;
        for text in [
            "Open Queen chat",
            "⌘J",
            "Toggle whip dry-run (now on)",
            "Select a node first",
            "action",
            "Actions",
        ] {
            assert!(text_center(&output, text).is_some(), "{text:?} is drawn");
        }

        palette.query = "que".into();
        let (_, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert!(
            text_center(&output, "Queen").is_some(),
            "nodes and actions mix"
        );
        assert!(text_center(&output, "Open Queen chat").is_some());
    }

    #[test]
    fn enter_runs_an_enabled_action_and_records_it() {
        let snapshot = FleetSnapshot::build(&FleetData::default(), 0);
        let cx = ready();
        let enter = key(Key::Enter, Modifiers::NONE);

        let mut palette = SearchPalette::default();
        palette.open();
        palette.query = ">whip selected".into();
        let (outcome, _) = run_palette(&mut palette, &snapshot, &cx, vec![enter.clone()]);
        assert_eq!(outcome, None, "disabled: Enter does nothing");
        assert!(palette.is_open());
        assert_eq!(palette.recents().ids().count(), 0);

        palette.query = ">fit".into();
        let (outcome, _) = run_palette(&mut palette, &snapshot, &cx, vec![enter]);
        assert_eq!(outcome, Some(SearchOutcome::Run(ActionId::FitGraph)));
        assert!(!palette.is_open());
        assert_eq!(
            palette.recents().ids().collect::<Vec<_>>(),
            [ActionId::FitGraph]
        );
    }
}
