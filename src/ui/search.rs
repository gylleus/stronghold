//! The Cmd+K command palette: find an agent node by label, branch, repo, worktree, run
//! objective, task, agent, terminal handle or node id, or run an action (see
//! [`crate::ui::commands`]).

use std::collections::BTreeSet;
use std::rc::Rc;

use eframe::egui::{self, Key, Modifiers, RichText};

use crate::clan::ClanData;
use crate::model::{ClanSnapshot, NO_REPO, Node, NodeId, NodeKind, NodeState, RepoIndex};
use crate::ui::clan_view::{kind_label, state_color, state_label};
use crate::ui::commands::{self, ACTIONS, ActionContext, ActionId, Recents};

/// Most agents the palette lists for a query.
pub const MAX_RESULTS: usize = 12;
/// Most actions the palette lists for a query (all of them after `>`).
const MAX_ACTION_RESULTS: usize = 6;

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
    NodeId,
}

impl Field {
    /// How much a match in this field counts relative to others.
    fn weight(self) -> u32 {
        match self {
            Field::Label | Field::TaskTitle => 3,
            Field::Branch | Field::Objective | Field::Repo => 2,
            Field::Worktree
            | Field::TaskId
            | Field::RunId
            | Field::Agent
            | Field::Handle
            | Field::NodeId => 1,
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
            Field::NodeId => "id",
        }
    }
}

/// Whether a node is worth listing first: live agents before finished ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Activity {
    NeedsInput,
    Active,
    /// Done, failed, released, parked, or its terminal is gone.
    Finished,
}

impl Activity {
    pub fn of(node: &Node) -> Self {
        let finished = matches!(node.state, NodeState::Done | NodeState::Failed)
            || node.released_at_ms.is_some()
            || node.is_parked()
            || !node.live;
        match node.state {
            _ if finished => Activity::Finished,
            NodeState::NeedsInput => Activity::NeedsInput,
            _ => Activity::Active,
        }
    }
}

/// One node's searchable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub state: NodeState,
    pub activity: Activity,
    pub fields: Vec<(Field, String)>,
    /// `fields`' text lowercased, in the same order.
    lower: Vec<String>,
}

impl SearchEntry {
    pub fn new(
        id: NodeId,
        kind: NodeKind,
        label: String,
        state: NodeState,
        activity: Activity,
        fields: Vec<(Field, String)>,
    ) -> Self {
        let lower = fields.iter().map(|(_, s)| s.to_lowercase()).collect();
        Self {
            id,
            kind,
            label,
            state,
            activity,
            fields,
            lower,
        }
    }

    pub fn field(&self, field: Field) -> Option<&str> {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, s)| s.as_str())
    }
}

/// Search entries for every node of `snapshot`; `data` adds task titles.
pub fn search_entries(snapshot: &ClanSnapshot, data: Option<&ClanData>) -> Vec<SearchEntry> {
    let repos = RepoIndex::new(snapshot);
    snapshot
        .nodes
        .iter()
        .map(|node| entry(node, snapshot, data, &repos))
        .collect()
}

fn entry(
    node: &Node,
    snapshot: &ClanSnapshot,
    data: Option<&ClanData>,
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
                .filter(|r| *r != NO_REPO && node.kind != NodeKind::King)
                .map(str::to_string),
        ),
        (Field::Worktree, node.worktree_path.clone()),
        (Field::TaskId, node.task_id.clone()),
        (Field::RunId, node.run_id.clone()),
        (Field::Agent, node.agent.as_ref().map(|a| a.to_string())),
        (Field::Handle, node.terminal_handle.clone()),
        (Field::NodeId, Some(node.id.to_string())),
    ]
    .into_iter()
    .filter_map(|(f, s)| Some((f, s.filter(|s| !s.is_empty())?)))
    .collect();
    SearchEntry::new(
        node.id.clone(),
        node.kind,
        node.label.clone(),
        node.state,
        Activity::of(node),
        fields,
    )
}

/// How well `token` (lowercase) matches `text`: exact beats prefix beats word start beats
/// substring beats a fuzzy subsequence; `None` if the letters don't occur in order.
pub fn match_score(token: &str, text: &str) -> Option<u32> {
    match_lower(token, &text.to_lowercase())
}

/// [`match_score`] against already lowercased `text`.
fn match_lower(token: &str, text: &str) -> Option<u32> {
    if token.is_empty() {
        return Some(0);
    }
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
    fuzzy_score(token, text)
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

/// Weakest [`match_score`] of a strong match: a substring. Actions need every token to match
/// strongly; agents matching only fuzzily rank after every strong match.
const MIN_STRONG_MATCH: u32 = 40;

/// Entries matching every whitespace-separated token of `query`, best first, at most `limit`.
/// Each token scores its best field (score × field weight). Entries whose every token matches
/// strongly come first; then live agents before finished ones, higher scores, needs-input
/// first, coordinators before workers before terminals before subagents, and label. An empty
/// query lists entries in order, needs-input first and finished last.
pub fn rank(entries: &[SearchEntry], query: &str, limit: usize) -> Vec<SearchHit> {
    let query = query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&i| (entries[i].activity, i));
        return order
            .into_iter()
            .take(limit)
            .map(|index| SearchHit {
                index,
                score: 0,
                field: Field::Label,
            })
            .collect();
    }
    let mut hits: Vec<(bool, SearchHit)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let fields: Vec<(Field, u32, &str)> = entry
                .fields
                .iter()
                .zip(&entry.lower)
                .map(|((f, _), text)| (*f, f.weight(), text.as_str()))
                .collect();
            let m = score_tokens(&tokens, &fields, 0)?;
            let hit = SearchHit {
                index,
                score: m.score,
                field: m.field.unwrap_or(Field::Label),
            };
            Some((m.weakest >= MIN_STRONG_MATCH, hit))
        })
        .collect();
    hits.sort_by(|(strong_a, a), (strong_b, b)| {
        let (ea, eb) = (&entries[a.index], &entries[b.index]);
        let finished = |e: &SearchEntry| e.activity == Activity::Finished;
        strong_b
            .cmp(strong_a)
            .then(finished(ea).cmp(&finished(eb)))
            .then(b.score.cmp(&a.score))
            .then(ea.activity.cmp(&eb.activity))
            .then(ea.kind.cmp(&eb.kind))
            .then_with(|| ea.label.cmp(&eb.label))
            .then(a.index.cmp(&b.index))
    });
    hits.truncate(limit);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

/// How a query's tokens matched one entry's fields.
struct TokenMatch<F> {
    /// Each token's best field score (score × weight), summed.
    score: u32,
    /// The field with the best single token score; ties go to the one listed first.
    field: Option<F>,
    /// The lowest unweighted [`match_score`] among the tokens' best fields.
    weakest: u32,
}

/// How `tokens` (lowercase) match weighted, lowercased `fields`. `None` unless every token
/// matches somewhere with a [`match_score`] of at least `min`.
fn score_tokens<F: Copy>(
    tokens: &[&str],
    fields: &[(F, u32, &str)],
    min: u32,
) -> Option<TokenMatch<F>> {
    let mut total = 0;
    let mut weakest = u32::MAX;
    let mut best: Option<(u32, F)> = None;
    for token in tokens {
        let (score, raw, field) = fields
            .iter()
            .enumerate()
            .filter_map(|(i, (f, weight, text))| {
                let raw = match_lower(token, text).filter(|s| *s >= min)?;
                Some((raw * weight, raw, i, *f))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)))
            .map(|(score, raw, _, f)| (score, raw, f))?;
        total += score;
        weakest = weakest.min(raw);
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, field));
        }
    }
    Some(TokenMatch {
        score: total,
        field: best.map(|(_, f)| f),
        weakest,
    })
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

/// Agents listed under an empty query, after the actions.
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
/// first agents. `>` prefix: actions only (all of them, recents first, for a bare `>`).
/// Otherwise an "Dwarves" and an "Actions" section, the one with the better top match first
/// (actions on a tie).
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
            section(
                "Dwarves",
                nodes.into_iter().map(PaletteItem::Node).collect(),
            );
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
    let agents = rank(entries, query, MAX_RESULTS);
    let agents_first = match (agents.first(), actions.first()) {
        (Some(agent), Some((_, action))) => agent.score > *action,
        _ => true,
    };
    let agent_items = agents.into_iter().map(PaletteItem::Node).collect();
    let action_items = actions
        .into_iter()
        .take(MAX_ACTION_RESULTS)
        .map(|(id, _)| PaletteItem::Action(id))
        .collect();
    if agents_first {
        section("Dwarves", agent_items);
        section("Actions", action_items);
    } else {
        section("Actions", action_items);
        section("Dwarves", agent_items);
    }
    rows
}

/// Actions matching every token with their scores, best first; the title counts like a node's
/// label, keywords like a branch. Enabled actions come first on equal scores, then registry
/// order.
fn rank_actions(tokens: &[&str], cx: &ActionContext) -> Vec<(ActionId, u32)> {
    let mut hits: Vec<(usize, ActionId, u32)> = ACTIONS
        .iter()
        .enumerate()
        .filter_map(|(order, action)| {
            let title = commands::title(action.id, cx).to_lowercase();
            let (fixed, keywords) = (action.title.to_lowercase(), action.keywords.to_lowercase());
            let fields = [
                ((), Field::Label.weight(), title.as_str()),
                ((), Field::Label.weight(), fixed.as_str()),
                ((), Field::Branch.weight(), keywords.as_str()),
            ];
            let m = score_tokens(tokens, &fields, MIN_STRONG_MATCH)?;
            Some((order, action.id, m.score))
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
    /// The current snapshot's entries, built on first use after [`Self::snapshot_changed`].
    entries: Option<Rc<Vec<SearchEntry>>>,
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

    /// Drops the cached entries; call whenever the snapshot (or the task titles) change.
    pub fn snapshot_changed(&mut self) {
        self.entries = None;
    }

    /// Draws the palette if open; returns the picked node or action. `hidden` are the nodes
    /// the sidebar filter hides; `cx` decides the actions' titles and which can run. Entries
    /// are built from `snapshot` once per [`Self::snapshot_changed`].
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: Option<&ClanSnapshot>,
        data: Option<&ClanData>,
        hidden: &BTreeSet<NodeId>,
        cx: &ActionContext,
    ) -> Option<SearchOutcome> {
        if !self.open {
            return None;
        }
        let entries = Rc::clone(self.entries.get_or_insert_with(|| {
            Rc::new(snapshot.map_or_else(Vec::new, |s| search_entries(s, data)))
        }));
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
                        .hint_text("Search dwarves and actions (> for actions only)…")
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
                    ui.weak("No matching dwarves or actions");
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
                                    let is_hidden = hidden.contains(&entry.id);
                                    result_row(ui, entry, hit, is_hidden, highlighted)
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

/// An agent: a state dot and the label (weak once finished), then a line of kind, state,
/// repo and branch, the run, and the field that matched if not already shown.
fn result_row(
    ui: &mut egui::Ui,
    entry: &SearchEntry,
    hit: &SearchHit,
    is_hidden: bool,
    highlighted: bool,
) -> egui::Response {
    let inner = row_frame(ui, highlighted).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(
                rect.center(),
                5.0,
                state_color(entry.state, ui.visuals().dark_mode),
            );
            let label = RichText::new(&entry.label);
            let label = if entry.activity == Activity::Finished {
                label.weak()
            } else {
                label.strong()
            };
            ui.add(egui::Label::new(label).truncate());
            if is_hidden {
                ui.label(
                    RichText::new("hidden by filter")
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            }
        });
        ui.add(egui::Label::new(RichText::new(result_meta(entry, hit)).small().weak()).truncate());
    });
    ui.interact(
        inner.response.rect,
        ui.id().with(("search_row", &entry.id)),
        egui::Sense::click(),
    )
}

/// An agent row's second line: "worker · needs input · stronghold @ feat/x · Objective".
pub fn result_meta(entry: &SearchEntry, hit: &SearchHit) -> String {
    let mut meta = vec![
        kind_label(entry.kind).to_string(),
        state_label(entry.state).to_string(),
    ];
    let mut shown = vec![Field::Label];
    let place = match (entry.field(Field::Repo), entry.field(Field::Branch)) {
        (Some(repo), Some(branch)) => Some(format!("{repo} @ {branch}")),
        (repo, branch) => repo.or(branch).map(str::to_string),
    };
    if let Some(place) = place {
        meta.push(place);
        shown.extend([Field::Repo, Field::Branch]);
    }
    if let Some(run) = entry.field(Field::Objective) {
        meta.push(run.to_string());
        shown.push(Field::Objective);
    } else if let Some(run) = entry.field(Field::RunId) {
        meta.push(format!("run {run}"));
        shown.push(Field::RunId);
    }
    if !shown.contains(&hit.field)
        && let Some(text) = entry.field(hit.field)
    {
        meta.push(format!("{}: {text}", hit.field.name()));
    }
    meta.join(" · ")
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
            parked_at_ms: None,
        }
    }

    fn entry(id: &str, kind: NodeKind, fields: &[(Field, &str)]) -> SearchEntry {
        entry_in(id, kind, NodeState::Working, true, fields)
    }

    fn entry_in(
        id: &str,
        kind: NodeKind,
        state: NodeState,
        live: bool,
        fields: &[(Field, &str)],
    ) -> SearchEntry {
        let label = fields
            .iter()
            .find(|(f, _)| *f == Field::Label)
            .map_or(id, |(_, s)| s);
        let mut n = node(id, kind, label);
        n.state = state;
        n.live = live;
        SearchEntry::new(
            n.id.clone(),
            kind,
            label.to_string(),
            state,
            Activity::of(&n),
            fields.iter().map(|(f, s)| (*f, s.to_string())).collect(),
        )
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
        let mut snapshot = ClanSnapshot::default();
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
        w.worktree_path = Some("/Users/x/repo/hackathon/storefront".into());
        snapshot.nodes = vec![w];

        let mut data = ClanData::default();
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
        assert_eq!(e.field(Field::Repo), Some("storefront"));
        let hits = rank(&entries, "storefront", 5);
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
    fn search_finds_the_king() {
        let snapshot = ClanSnapshot::build(&ClanData::default(), 0);
        let entries = search_entries(&snapshot, None);
        let hits = rank(&entries, "kin", MAX_RESULTS);
        assert_eq!(entries[hits[0].index].id, NodeId::king());
    }

    #[test]
    fn rank_puts_live_agents_before_finished_ones() {
        let entries = vec![
            entry_in(
                "done",
                NodeKind::Worker,
                NodeState::Done,
                true,
                &[(Field::Label, "pay")],
            ),
            entry_in(
                "gone",
                NodeKind::Worker,
                NodeState::Idle,
                false,
                &[(Field::Label, "pay")],
            ),
            entry_in(
                "idle",
                NodeKind::Worker,
                NodeState::Idle,
                true,
                &[(Field::Label, "repay")],
            ),
            entry_in(
                "ask",
                NodeKind::Terminal,
                NodeState::NeedsInput,
                true,
                &[(Field::Label, "repay")],
            ),
            entry_in(
                "fail",
                NodeKind::Worker,
                NodeState::Failed,
                true,
                &[(Field::Label, "pay")],
            ),
        ];
        // The live ones lead despite weaker label matches; on a tie needs-input comes first even
        // before a worker. Finished ones keep entry order among themselves.
        assert_eq!(
            ids(&entries, &rank(&entries, "pay", 10)),
            ["ask", "idle", "done", "gone", "fail"]
        );
        // Empty query: needs-input first, finished last, otherwise in order.
        assert_eq!(
            ids(&entries, &rank(&entries, "", 10)),
            ["ask", "idle", "done", "gone", "fail"]
        );
    }

    #[test]
    fn rank_puts_fuzzy_matches_after_substring_matches() {
        let entries = vec![
            entry_in(
                "live-fuzzy",
                NodeKind::Coordinator,
                NodeState::Working,
                true,
                &[(Field::Label, "pull a yak")],
            ),
            entry_in(
                "old-exact",
                NodeKind::Worker,
                NodeState::Done,
                true,
                &[(Field::Label, "pay")],
            ),
        ];
        assert_eq!(
            ids(&entries, &rank(&entries, "pay", 10)),
            ["old-exact", "live-fuzzy"]
        );
    }

    #[test]
    fn rank_matches_every_identifying_field_case_insensitively() {
        let mut w = node("dispatch:d-42", NodeKind::Worker, "worker 3");
        w.branch = Some("feat/Stripe-Refunds".into());
        w.run_id = Some("run-9f".into());
        w.task_id = Some("task-7".into());
        w.terminal_handle = Some("term_ABC".into());
        w.worktree_path = Some("/x/storefront".into());
        let mut snapshot = ClanSnapshot::default();
        snapshot.nodes = vec![w, node("term:other", NodeKind::Terminal, "worker 3")];
        let entries = search_entries(&snapshot, None);
        for (query, field) in [
            ("WORKER 3", Field::Label),
            ("stripe-refunds", Field::Branch),
            ("RUN-9F", Field::RunId),
            ("task-7", Field::TaskId),
            ("term_abc", Field::Handle),
            ("storefront", Field::Repo),
            ("d-42", Field::NodeId),
        ] {
            let hits = rank(&entries, query, 5);
            assert_eq!(hits[0].index, 0, "{query}");
            assert_eq!(hits[0].field, field, "{query}");
        }
        // Two tokens in different fields narrow it to one.
        assert_eq!(
            ids(&entries, &rank(&entries, "worker stripe", 5)),
            ["dispatch:d-42"]
        );
    }

    #[test]
    fn result_meta_tells_similar_agents_apart() {
        let a = entry_in(
            "a",
            NodeKind::Worker,
            NodeState::NeedsInput,
            true,
            &[
                (Field::Label, "worker"),
                (Field::Repo, "stronghold"),
                (Field::Branch, "feat/a"),
                (Field::Objective, "Ship search"),
                (Field::RunId, "run1"),
                (Field::Handle, "term_1"),
            ],
        );
        let hit = |field| SearchHit {
            index: 0,
            score: 1,
            field,
        };
        assert_eq!(
            result_meta(&a, &hit(Field::Label)),
            "worker · needs input · stronghold @ feat/a · Ship search"
        );
        assert_eq!(
            result_meta(&a, &hit(Field::Branch)),
            "worker · needs input · stronghold @ feat/a · Ship search"
        );
        assert_eq!(
            result_meta(&a, &hit(Field::Handle)),
            "worker · needs input · stronghold @ feat/a · Ship search · handle: term_1"
        );
        let b = entry_in(
            "b",
            NodeKind::Terminal,
            NodeState::Done,
            true,
            &[(Field::Label, "worker"), (Field::RunId, "run2")],
        );
        assert_eq!(
            result_meta(&b, &hit(Field::Label)),
            "terminal · done · run run2"
        );
    }

    #[test]
    fn entries_follow_the_snapshot_once_invalidated() {
        let mut snapshot = ClanSnapshot::build(&ClanData::default(), 0);
        let mut palette = SearchPalette::default();
        palette.open();
        palette.query = "newc".into();
        let cx = ready();
        let (_, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        use crate::ui::test_util::text_center;
        assert!(text_center(&output, "newcomer").is_none());

        snapshot
            .nodes
            .push(node("term:n", NodeKind::Terminal, "newcomer"));
        let (_, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert!(text_center(&output, "newcomer").is_none(), "cached");
        palette.snapshot_changed();
        let (_, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert!(text_center(&output, "newcomer").is_some());
        assert!(text_center(&output, "Dwarves").is_some());
    }

    #[test]
    fn ranks_hundreds_of_agents() {
        let entries: Vec<SearchEntry> = (0..600)
            .map(|i| {
                let state = [NodeState::Working, NodeState::Done, NodeState::NeedsInput][i % 3];
                let label = format!("worker {i}");
                let branch = format!("feat/task-{i}");
                entry_in(
                    &format!("dispatch:{i}"),
                    NodeKind::Worker,
                    state,
                    true,
                    &[(Field::Label, &label), (Field::Branch, &branch)],
                )
            })
            .collect();
        let hits = rank(&entries, "task-599", MAX_RESULTS);
        assert_eq!(entries[hits[0].index].id.0, "dispatch:599");
        let hits = rank(&entries, "worker", MAX_RESULTS);
        assert_eq!(hits.len(), MAX_RESULTS);
        assert!(
            hits.iter()
                .all(|h| entries[h.index].activity != Activity::Finished)
        );
        assert_eq!(entries[hits[0].index].state, NodeState::NeedsInput);
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
        // "fit": the action's title prefix beats the node's worktree word match, so the
        // Actions section comes first.
        let rows = palette_rows(&entries, "fit", &Recents::default(), &cx);
        assert_eq!(row_names(&entries, &rows)[..2], [">fit_farm", "fits"]);
        // An exact node label outranks an action matching only by keyword.
        let rows = palette_rows(&entries, "chat", &Recents::default(), &cx);
        assert_eq!(row_names(&entries, &rows)[0], "chat-node");
        assert_eq!(rows[0].header, Some("Dwarves"));
        assert!(actions(&rows).contains(&ActionId::OpenChat));
        // Every token must match an action's title or keywords.
        let rows = palette_rows(&entries, "whip stalled", &Recents::default(), &cx);
        assert_eq!(actions(&rows), [ActionId::WhipStalled]);
        // Equal scores list actions first, enabled before disabled.
        let rows = palette_rows(&entries, "whip", &Recents::default(), &cx);
        let names = row_names(&entries, &rows);
        assert_eq!(names[0], ">whip_stalled", "{names:?}");
        assert!(names.contains(&"whipper".to_string()));
        let headers: Vec<_> = rows.iter().filter_map(|r| r.header).collect();
        assert_eq!(headers, ["Actions", "Dwarves"]);
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
        assert_eq!(actions(&rows), [ActionId::FitFarm]);
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
        for id in [ActionId::FitFarm, ActionId::OpenChat, ActionId::FitFarm] {
            recents.push(id);
        }
        let rows = palette_rows(&entries, "  ", &recents, &cx);
        assert_eq!(
            row_names(&entries, &rows),
            [
                ">fit_farm",
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
        assert_eq!(rows[2].header, Some("Dwarves"));
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
        snapshot: &ClanSnapshot,
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
        let snapshot = ClanSnapshot::build(&ClanData::default(), 0);
        let cx = ready();
        let mut palette = SearchPalette::default();
        palette.open();
        palette.query = ">".into();
        let (outcome, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert_eq!(outcome, None);
        use crate::ui::test_util::text_center;
        for text in [
            "Open King chat",
            "⌘J",
            "Toggle whip dry-run (now on)",
            "Select a node first",
            "action",
            "Actions",
        ] {
            assert!(text_center(&output, text).is_some(), "{text:?} is drawn");
        }

        palette.query = "kin".into();
        let (_, output) = run_palette(&mut palette, &snapshot, &cx, vec![]);
        assert!(
            text_center(&output, "King").is_some(),
            "nodes and actions mix"
        );
        assert!(text_center(&output, "Open King chat").is_some());
    }

    #[test]
    fn enter_runs_an_enabled_action_and_records_it() {
        let snapshot = ClanSnapshot::build(&ClanData::default(), 0);
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
        assert_eq!(outcome, Some(SearchOutcome::Run(ActionId::FitFarm)));
        assert!(!palette.is_open());
        assert_eq!(
            palette.recents().ids().collect::<Vec<_>>(),
            [ActionId::FitFarm]
        );
    }
}
