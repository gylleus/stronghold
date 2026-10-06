//! Sidebar: runs and loose terminals grouped by repo, and fleet filters.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, RichText, Sense, Ui};

use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState, RepoIndex, TaskCounts};
use crate::park::SLEEPING_ROOM;
use crate::ui::graph::{kind_label, state_color, state_label};

pub use crate::model::{NO_REPO, node_repo, repo_of};

const ALL_STATES: [NodeState; 6] = [
    NodeState::Working,
    NodeState::NeedsInput,
    NodeState::Stalled,
    NodeState::Idle,
    NodeState::Done,
    NodeState::Failed,
];
const ALL_KINDS: [NodeKind; 6] = [
    NodeKind::Envoy,
    NodeKind::Coordinator,
    NodeKind::Attendant,
    NodeKind::Worker,
    NodeKind::Terminal,
    NodeKind::Subagent,
];
const DOT_RADIUS: f32 = 5.0;

/// Which nodes the graph shows. Empty sets mean "all".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FleetFilter {
    pub repos: BTreeSet<String>,
    pub states: BTreeSet<NodeState>,
    pub kinds: BTreeSet<NodeKind>,
    /// Show live terminals outside any run.
    pub show_loose: bool,
    /// Show recently released (fading) workers.
    pub show_released: bool,
    /// Case-insensitive substring of a node's label, branch, run objective or repo.
    pub text: String,
}

impl Default for FleetFilter {
    fn default() -> Self {
        Self {
            repos: BTreeSet::new(),
            states: BTreeSet::new(),
            kinds: BTreeSet::new(),
            show_loose: true,
            show_released: true,
            text: String::new(),
        }
    }
}

impl FleetFilter {
    /// Whether `node` is shown. The queen always is.
    pub fn matches(&self, node: &Node, snapshot: &FleetSnapshot) -> bool {
        if node.kind == NodeKind::Queen {
            return true;
        }
        if !self.show_loose && node.kind == NodeKind::Terminal {
            return false;
        }
        if !self.show_released && node.released_at_ms.is_some() {
            return false;
        }
        if !self.states.is_empty() && !self.states.contains(&node.state) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&node.kind) {
            return false;
        }
        if !self.repos.is_empty() && !self.repos.contains(&node_repo(node, snapshot)) {
            return false;
        }
        let needle = self.text.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let objective = node
            .run_id
            .as_deref()
            .and_then(|r| snapshot.run(r))
            .and_then(|r| r.objective.as_deref());
        let repo = node_repo(node, snapshot);
        [
            Some(node.label.as_str()),
            node.branch.as_deref(),
            objective,
            Some(repo.as_str()).filter(|r| *r != NO_REPO),
        ]
        .into_iter()
        .flatten()
        .any(|s| s.to_lowercase().contains(&needle))
    }

    /// IDs of the snapshot's nodes this filter hides.
    pub fn hidden(&self, snapshot: &FleetSnapshot) -> BTreeSet<NodeId> {
        snapshot
            .nodes
            .iter()
            .filter(|n| !self.matches(n, snapshot))
            .map(|n| n.id.clone())
            .collect()
    }

    /// Whether this filter can hide anything.
    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }
}

/// Like [`FleetSnapshot::node`], but also works on snapshots assembled by hand.
fn find_node<'a>(snapshot: &'a FleetSnapshot, id: &NodeId) -> Option<&'a Node> {
    snapshot
        .node(id)
        .or_else(|| snapshot.nodes.iter().find(|n| &n.id == id))
}

pub fn repo_counts(snapshot: &FleetSnapshot) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for node in snapshot.agent_nodes() {
        *counts.entry(node_repo(node, snapshot)).or_insert(0) += 1;
    }
    counts
}

pub fn state_counts(snapshot: &FleetSnapshot) -> BTreeMap<NodeState, usize> {
    let mut counts = BTreeMap::new();
    for node in snapshot.agent_nodes() {
        *counts.entry(node.state).or_insert(0) += 1;
    }
    counts
}

pub fn kind_counts(snapshot: &FleetSnapshot) -> BTreeMap<NodeKind, usize> {
    let mut counts = BTreeMap::new();
    for node in snapshot.agent_nodes() {
        *counts.entry(node.kind).or_insert(0) += 1;
    }
    counts
}

/// Toggles `key` in a set where empty means "all of `universe`"; a set that ends up holding
/// every member of `universe` collapses back to empty.
pub fn toggle_member<K: Ord + Clone>(set: &mut BTreeSet<K>, universe: &[K], key: &K) {
    if set.is_empty() {
        set.extend(universe.iter().filter(|k| *k != key).cloned());
    } else if !set.remove(key) {
        set.insert(key.clone());
    }
    if universe.iter().all(|k| set.contains(k)) {
        set.clear();
    }
}

/// Toggles a chip: selected chips narrow the set; none selected means "all".
pub fn toggle_chip<K: Ord>(set: &mut BTreeSet<K>, key: K) {
    if !set.remove(&key) {
        set.insert(key);
    }
}

/// Whether `key` passes a set where empty means "all".
fn is_on<K: Ord>(set: &BTreeSet<K>, key: &K) -> bool {
    set.is_empty() || set.contains(key)
}

/// One row of the run list.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub objective: Option<String>,
    pub coordinator_state: Option<NodeState>,
    pub tasks: Option<TaskCounts>,
    pub workers: usize,
    /// Latest terminal output among the run's nodes.
    pub last_activity_ms: Option<i64>,
}

/// The snapshot's runs, most recently active first.
pub fn run_rows(snapshot: &FleetSnapshot) -> Vec<RunRow> {
    let mut rows: Vec<RunRow> = snapshot
        .runs
        .iter()
        .map(|run| {
            let nodes: Vec<&Node> = snapshot.nodes_in_run(&run.id).collect();
            RunRow {
                id: run.id.clone(),
                objective: run.objective.clone(),
                coordinator_state: run
                    .coordinator
                    .as_ref()
                    .and_then(|c| find_node(snapshot, c))
                    .map(|n| n.state),
                tasks: run.task_counts,
                workers: nodes.iter().filter(|n| n.kind == NodeKind::Worker).count(),
                last_activity_ms: nodes.iter().filter_map(|n| n.last_output_ms).max(),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.last_activity_ms
            .cmp(&a.last_activity_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// One repo of the sidebar: its runs and loose terminals.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoGroup {
    pub repo: String,
    /// The repo's runs, most recently active first.
    pub runs: Vec<RunRow>,
    /// Live terminals of the repo outside any run, by label.
    pub loose: Vec<LooseRow>,
    /// Nodes of the repo per state (runs and loose terminals, subagents included).
    pub states: BTreeMap<NodeState, usize>,
}

/// A loose terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LooseRow {
    pub id: NodeId,
    pub label: String,
    pub state: NodeState,
}

/// Runs and loose terminals grouped by repo, repos by name with [`NO_REPO`] last.
pub fn repo_groups(snapshot: &FleetSnapshot) -> Vec<RepoGroup> {
    let index = RepoIndex::new(snapshot);
    let mut out: BTreeMap<(bool, String), RepoGroup> = BTreeMap::new();
    fn entry<'a>(
        out: &'a mut BTreeMap<(bool, String), RepoGroup>,
        repo: &str,
    ) -> &'a mut RepoGroup {
        out.entry((repo == NO_REPO, repo.to_string()))
            .or_insert_with(|| RepoGroup {
                repo: repo.to_string(),
                runs: Vec::new(),
                loose: Vec::new(),
                states: BTreeMap::new(),
            })
    }
    for row in run_rows(snapshot) {
        let repo = index.run(&row.id).unwrap_or(NO_REPO);
        entry(&mut out, repo).runs.push(row);
    }
    for node in snapshot.agent_nodes() {
        // Parked agents sleep in their own group, last, without state counts.
        if node.is_parked() {
            let group = out
                .entry((true, SLEEPING_ROOM.to_string()))
                .or_insert_with(|| RepoGroup {
                    repo: SLEEPING_ROOM.to_string(),
                    runs: Vec::new(),
                    loose: Vec::new(),
                    states: BTreeMap::new(),
                });
            group.loose.push(LooseRow {
                id: node.id.clone(),
                label: node.label.clone(),
                state: node.state,
            });
            continue;
        }
        let repo = index.node(&node.id).unwrap_or(NO_REPO);
        let group = entry(&mut out, repo);
        *group.states.entry(node.state).or_insert(0) += 1;
        if node.kind == NodeKind::Terminal {
            group.loose.push(LooseRow {
                id: node.id.clone(),
                label: node.label.clone(),
                state: node.state,
            });
        }
    }
    let mut out: Vec<RepoGroup> = out.into_values().collect();
    for group in &mut out {
        group
            .loose
            .sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    }
    out
}

/// "3 ready · 2 running · 14 done", listing only non-zero buckets.
pub fn task_summary(counts: &TaskCounts) -> String {
    let parts = [
        (counts.pending, "pending"),
        (counts.ready, "ready"),
        (counts.dispatched, "running"),
        (counts.blocked, "blocked"),
        (counts.failed, "failed"),
        (counts.completed, "done"),
        (counts.other, "other"),
    ];
    let text: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect();
    if text.is_empty() {
        "no tasks".to_string()
    } else {
        text.join(" · ")
    }
}

/// What the user did in the sidebar this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarResponse {
    /// A run was clicked: focus the graph on it.
    pub focus_run: Option<String>,
    /// A repo header was clicked: focus the graph on its region.
    pub focus_repo: Option<String>,
    /// A loose terminal was clicked: pan the graph onto it (it is also in `select`).
    pub focus_node: Option<NodeId>,
    /// A node to select (the clicked run's coordinator).
    pub select: Option<NodeId>,
    /// The filter changed this frame.
    pub filter_changed: bool,
    /// "Close shown…" was clicked: ask to close the nodes the filter shows.
    pub close_shown: bool,
    /// A repo header's "+" was clicked: open "New ant…" for that repo.
    pub new_session_repo: Option<String>,
}

/// What the sidebar shows of one snapshot, derived once per snapshot rather than every frame.
#[derive(Debug, Default)]
pub struct SidebarCache {
    derived: Option<Derived>,
    /// Nodes a filter hides, for the filter it was counted for.
    hidden: Option<(FleetFilter, usize)>,
}

#[derive(Debug)]
struct Derived {
    groups: Vec<RepoView>,
    repos: BTreeMap<String, usize>,
    states: BTreeMap<NodeState, usize>,
    kinds: BTreeMap<NodeKind, usize>,
}

/// A repo group with its runs' display text.
#[derive(Debug)]
struct RepoView {
    repo: String,
    runs: Vec<RunView>,
    loose: Vec<LooseRow>,
    states: BTreeMap<NodeState, usize>,
}

impl RepoView {
    fn new(group: RepoGroup) -> Self {
        Self {
            repo: group.repo,
            runs: group.runs.into_iter().map(RunView::new).collect(),
            loose: group.loose,
            states: group.states,
        }
    }
}

/// A run row with its display text.
#[derive(Debug)]
struct RunView {
    row: RunRow,
    title: String,
    summary: String,
}

impl RunView {
    fn new(row: RunRow) -> Self {
        let tasks = row
            .tasks
            .as_ref()
            .map_or_else(|| "tasks unknown".to_string(), task_summary);
        let workers = match row.workers {
            1 => "1 worker".to_string(),
            n => format!("{n} workers"),
        };
        Self {
            title: row.objective.clone().unwrap_or_else(|| row.id.clone()),
            summary: format!("{tasks} · {workers}"),
            row,
        }
    }
}

impl SidebarCache {
    /// Call when the snapshot changes.
    pub fn snapshot_changed(&mut self) {
        *self = Self::default();
    }

    fn derived(&mut self, snapshot: &FleetSnapshot) -> &Derived {
        self.derived.get_or_insert_with(|| Derived {
            groups: repo_groups(snapshot)
                .into_iter()
                .map(RepoView::new)
                .collect(),
            repos: repo_counts(snapshot),
            states: state_counts(snapshot),
            kinds: kind_counts(snapshot),
        })
    }

    fn hidden_count(&mut self, snapshot: &FleetSnapshot, filter: &FleetFilter) -> usize {
        match &self.hidden {
            Some((cached, count)) if cached == filter => *count,
            _ => {
                let count = filter.hidden(snapshot).len();
                self.hidden = Some((filter.clone(), count));
                count
            }
        }
    }
}

/// Draws the run list and filter controls; edits `filter` in place.
pub fn sidebar(ui: &mut Ui, snapshot: &FleetSnapshot, filter: &mut FleetFilter) -> SidebarResponse {
    sidebar_with(ui, snapshot, filter, |_| {})
}

/// Like [`sidebar`], with `extra` sections (e.g. the whip log) drawn between the runs and the
/// filters, inside the sidebar's scroll area.
pub fn sidebar_with(
    ui: &mut Ui,
    snapshot: &FleetSnapshot,
    filter: &mut FleetFilter,
    extra: impl FnOnce(&mut Ui),
) -> SidebarResponse {
    sidebar_cached(ui, snapshot, &mut SidebarCache::default(), filter, extra)
}

/// Like [`sidebar_with`], reusing what `cache` derived from `snapshot` in earlier frames; call
/// [`SidebarCache::snapshot_changed`] when the snapshot changes.
pub fn sidebar_cached(
    ui: &mut Ui,
    snapshot: &FleetSnapshot,
    cache: &mut SidebarCache,
    filter: &mut FleetFilter,
    extra: impl FnOnce(&mut Ui),
) -> SidebarResponse {
    let before = filter.clone();
    let mut response = SidebarResponse::default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let groups = &cache.derived(snapshot).groups;
            let title = format!(
                "Repos ({}) · {} run{}",
                groups.len(),
                snapshot.runs.len(),
                if snapshot.runs.len() == 1 { "" } else { "s" }
            );
            egui::CollapsingHeader::new(title)
                .id_salt("repos_section")
                .default_open(true)
                .show(ui, |ui| {
                    repos_section(ui, snapshot, groups, &mut response);
                });
            extra(ui);
            egui::CollapsingHeader::new("Filters")
                .default_open(true)
                .show(ui, |ui| {
                    filters_section(ui, snapshot, cache, filter);
                    response.close_shown = ui
                        .button("Close shown…")
                        .on_hover_text(
                            "Kill the loose terminals and workers the filter shows (asks first)",
                        )
                        .clicked();
                });
        });
    response.filter_changed = *filter != before;
    response
}

fn repos_section(
    ui: &mut Ui,
    snapshot: &FleetSnapshot,
    groups: &[RepoView],
    response: &mut SidebarResponse,
) {
    if groups.is_empty() {
        crate::ui::art::empty_state(
            ui,
            crate::ui::art::anthill(),
            140.0,
            "No ants",
            "The colony is quiet. New ants show up here.",
        );
        return;
    }
    for group in groups {
        let id = ui.make_persistent_id(("repo_group", &group.repo));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
            .show_header(ui, |ui| repo_header(ui, group, response))
            .body(|ui| {
                runs_section(ui, snapshot, &group.runs, response);
                loose_section(ui, &group.loose, response);
            });
    }
}

/// The repo header's "+": opens "New ant…" with the repo picked.
fn new_session_button(ui: &mut Ui, group: &RepoView, response: &mut SidebarResponse) {
    if group.repo == NO_REPO || group.repo == SLEEPING_ROOM {
        return;
    }
    let button = ui
        .small_button("+")
        .on_hover_text(format!("New ant in {}…", group.repo));
    if button.clicked() {
        response.new_session_repo = Some(group.repo.clone());
    }
}

/// A repo's header: its name (click to focus its region) and node counts per state.
fn repo_header(ui: &mut Ui, group: &RepoView, response: &mut SidebarResponse) {
    let runs = group.runs.len();
    let hover = format!(
        "{}: {runs} run{} · {} loose terminal{}\n\nClick to focus its region",
        group.repo,
        if runs == 1 { "" } else { "s" },
        group.loose.len(),
        if group.loose.len() == 1 { "" } else { "s" },
    );
    let name = ui
        .add(
            egui::Label::new(RichText::new(&group.repo).strong())
                .sense(Sense::click())
                .truncate(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover);
    if name.clicked() {
        response.focus_repo = Some(group.repo.clone());
    }
    new_session_button(ui, group, response);
    let dark = ui.visuals().dark_mode;
    for state in ALL_STATES {
        let Some(&n) = group.states.get(&state) else {
            continue;
        };
        ui.spacing_mut().item_spacing.x = 2.0;
        dot(
            ui,
            state_color(state, dark),
            format!("{n} {}", state_label(state)),
        );
        ui.label(RichText::new(n.to_string()).small());
        ui.spacing_mut().item_spacing.x = 6.0;
    }
}

/// A repo's loose terminals; clicking one selects it and pans onto it.
fn loose_section(ui: &mut Ui, loose: &[LooseRow], response: &mut SidebarResponse) {
    for row in loose {
        ui.horizontal(|ui| {
            let color = state_color(row.state, ui.visuals().dark_mode);
            dot(ui, color, format!("terminal {}", state_label(row.state)));
            let clicked = ui
                .add(
                    egui::Label::new(&row.label)
                        .sense(Sense::click())
                        .truncate(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Select this terminal")
                .clicked();
            if clicked {
                response.select = Some(row.id.clone());
                response.focus_node = Some(row.id.clone());
            }
        });
    }
}

fn dot(ui: &mut Ui, color: egui::Color32, hover: String) {
    let (rect, dot) = ui.allocate_exact_size(
        egui::vec2(DOT_RADIUS * 2.0, DOT_RADIUS * 2.0),
        Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), DOT_RADIUS, color);
    dot.on_hover_text(hover);
}

fn runs_section(
    ui: &mut Ui,
    snapshot: &FleetSnapshot,
    runs: &[RunView],
    response: &mut SidebarResponse,
) {
    for RunView {
        row,
        title,
        summary,
    } in runs
    {
        let frame = egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                state_dot(ui, row.coordinator_state);
                ui.add(egui::Label::new(RichText::new(title).strong()).truncate())
                    .on_hover_ui(|ui| {
                        ui.label(format!("{title}\n\nrun {}", row.id));
                    });
            });
            ui.add(egui::Label::new(RichText::new(summary).small()).wrap());
        });
        let clicked = frame
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Focus this run")
            .clicked();
        if clicked {
            response.focus_run = Some(row.id.clone());
            response.select = snapshot
                .run(&row.id)
                .and_then(|r| r.coordinator.clone())
                .or_else(|| snapshot.attendant(&row.id).cloned());
        }
    }
}

fn state_dot(ui: &mut Ui, state: Option<NodeState>) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(DOT_RADIUS * 2.0, DOT_RADIUS * 2.0),
        Sense::hover(),
    );
    let color = match state {
        Some(s) => state_color(s, ui.visuals().dark_mode),
        None => ui.visuals().weak_text_color(),
    };
    ui.painter().circle_filled(rect.center(), DOT_RADIUS, color);
    response.on_hover_text(match state {
        Some(s) => format!("coordinator {}", state_label(s)),
        None => "coordinator not live".to_string(),
    });
}

fn filters_section(
    ui: &mut Ui,
    snapshot: &FleetSnapshot,
    cache: &mut SidebarCache,
    filter: &mut FleetFilter,
) {
    // Right-to-left so the text box gets exactly the width the button leaves: a guessed width
    // that overflows makes the resizable panel grow a little every frame.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .add_enabled(!filter.text.is_empty(), egui::Button::new("✕").small())
            .on_hover_text("Clear search")
            .clicked()
        {
            filter.text.clear();
        }
        ui.add(
            egui::TextEdit::singleline(&mut filter.text)
                .hint_text("Search label, branch, objective, repo")
                .desired_width(ui.available_width()),
        );
    });
    ui.checkbox(&mut filter.show_loose, "Loose terminals");
    ui.checkbox(&mut filter.show_released, "Recently released");

    ui.add_space(4.0);
    crate::ui::widgets::section_label(ui, "Repo");
    let derived = cache.derived(snapshot);
    let repos = &derived.repos;
    let mut universe: Vec<String> = repos.keys().cloned().collect();
    universe.extend(
        filter
            .repos
            .iter()
            .filter(|r| !repos.contains_key(*r))
            .cloned(),
    );
    universe.sort();
    for repo in &universe {
        let count = repos.get(repo).copied().unwrap_or(0);
        let mut on = is_on(&filter.repos, repo);
        if ui.checkbox(&mut on, format!("{repo} ({count})")).changed() {
            toggle_member(&mut filter.repos, &universe, repo);
        }
    }

    ui.add_space(4.0);
    crate::ui::widgets::section_label(ui, "State")
        .on_hover_text("Pick states to show only those; none picked shows all");
    let states = &derived.states;
    ui.horizontal_wrapped(|ui| {
        for state in ALL_STATES {
            let count = states.get(&state).copied().unwrap_or(0);
            let text = crate::ui::icons::with_icon(
                ui,
                crate::ui::icons::state_icon(state),
                &format!("{} {count}", state_label(state)),
                egui::TextStyle::Button,
                Some(state_color(state, ui.visuals().dark_mode)),
            );
            if ui
                .selectable_label(filter.states.contains(&state), text)
                .clicked()
            {
                toggle_chip(&mut filter.states, state);
            }
        }
    });

    ui.add_space(4.0);
    crate::ui::widgets::section_label(ui, "Kind")
        .on_hover_text("Pick kinds to show only those; none picked shows all");
    let kinds = &derived.kinds;
    ui.horizontal_wrapped(|ui| {
        for kind in ALL_KINDS {
            let count = kinds.get(&kind).copied().unwrap_or(0);
            if count == 0 && !filter.kinds.contains(&kind) {
                continue;
            }
            let text = crate::ui::icons::with_icon(
                ui,
                crate::ui::icons::kind_icon(kind),
                &format!("{} {count}", kind_label(kind)),
                egui::TextStyle::Button,
                None,
            );
            if ui
                .selectable_label(filter.kinds.contains(&kind), text)
                .clicked()
            {
                toggle_chip(&mut filter.kinds, kind);
            }
        }
    });

    ui.add_space(4.0);
    let hidden = cache.hidden_count(snapshot, filter);
    let total = snapshot.agent_nodes().count();
    ui.horizontal(|ui| {
        ui.weak(format!("{} of {total} shown", total.saturating_sub(hidden)));
        if ui
            .add_enabled(filter.is_active(), egui::Button::new("Reset"))
            .clicked()
        {
            *filter = FleetFilter::default();
        }
    });
}

#[cfg(test)]
mod tests;
