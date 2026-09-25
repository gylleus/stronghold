//! Sidebar: run list and fleet filters.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use eframe::egui::{self, RichText, Sense, Ui};

use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState, TaskCounts};
use crate::ui::graph::{kind_label, state_color, state_label};

/// Repo bucket for nodes without a worktree.
pub const NO_REPO: &str = "(no repo)";

const ALL_STATES: [NodeState; 6] = [
    NodeState::Working,
    NodeState::NeedsInput,
    NodeState::Stalled,
    NodeState::Idle,
    NodeState::Done,
    NodeState::Failed,
];
const ALL_KINDS: [NodeKind; 4] = [
    NodeKind::Coordinator,
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
    /// Case-insensitive substring of a node's label, branch or run objective.
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
    pub fn matches(&self, node: &Node, snapshot: &FleetSnapshot) -> bool {
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
        [Some(node.label.as_str()), node.branch.as_deref(), objective]
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

/// The repo of a worktree path: the component after `workspaces/` in Orca paths
/// (`…/orca/workspaces/<repo>/<worktree>`), else the parent directory's name without a
/// `-worktrees` suffix.
pub fn repo_of(path: &str) -> Option<String> {
    let path = Path::new(path.trim_end_matches('/'));
    let components: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    if let Some(pos) = components.iter().position(|c| *c == "workspaces")
        && let Some(repo) = components.get(pos + 1)
    {
        return Some((*repo).to_string());
    }
    let parent = path.parent()?.file_name()?.to_str()?;
    let repo = parent.strip_suffix("-worktrees").unwrap_or(parent);
    (!repo.is_empty()).then(|| repo.to_string())
}

/// A node's repo; workers without a worktree inherit their run coordinator's.
pub fn node_repo(node: &Node, snapshot: &FleetSnapshot) -> String {
    let own = node.worktree_path.as_deref().and_then(repo_of);
    own.or_else(|| {
        let coordinator = snapshot
            .run(node.run_id.as_deref()?)?
            .coordinator
            .as_ref()?;
        repo_of(find_node(snapshot, coordinator)?.worktree_path.as_deref()?)
    })
    .unwrap_or_else(|| NO_REPO.to_string())
}

/// Like [`FleetSnapshot::node`], but also works on snapshots assembled by hand.
fn find_node<'a>(snapshot: &'a FleetSnapshot, id: &NodeId) -> Option<&'a Node> {
    snapshot
        .node(id)
        .or_else(|| snapshot.nodes.iter().find(|n| &n.id == id))
}

pub fn repo_counts(snapshot: &FleetSnapshot) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for node in &snapshot.nodes {
        *counts.entry(node_repo(node, snapshot)).or_insert(0) += 1;
    }
    counts
}

pub fn state_counts(snapshot: &FleetSnapshot) -> BTreeMap<NodeState, usize> {
    let mut counts = BTreeMap::new();
    for node in &snapshot.nodes {
        *counts.entry(node.state).or_insert(0) += 1;
    }
    counts
}

pub fn kind_counts(snapshot: &FleetSnapshot) -> BTreeMap<NodeKind, usize> {
    let mut counts = BTreeMap::new();
    for node in &snapshot.nodes {
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
    /// A node to select (the clicked run's coordinator).
    pub select: Option<NodeId>,
    /// The filter changed this frame.
    pub filter_changed: bool,
}

/// Draws the run list and filter controls; edits `filter` in place.
pub fn sidebar(ui: &mut Ui, snapshot: &FleetSnapshot, filter: &mut FleetFilter) -> SidebarResponse {
    let before = filter.clone();
    let mut response = SidebarResponse::default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Runs ({})", snapshot.runs.len()))
                .default_open(true)
                .show(ui, |ui| runs_section(ui, snapshot, &mut response));
            egui::CollapsingHeader::new("Filters")
                .default_open(true)
                .show(ui, |ui| filters_section(ui, snapshot, filter));
        });
    response.filter_changed = *filter != before;
    response
}

fn runs_section(ui: &mut Ui, snapshot: &FleetSnapshot, response: &mut SidebarResponse) {
    let rows = run_rows(snapshot);
    if rows.is_empty() {
        ui.weak("No active runs");
        return;
    }
    for row in rows {
        let frame = egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                state_dot(ui, row.coordinator_state);
                let objective = row.objective.as_deref().unwrap_or(&row.id);
                ui.add(egui::Label::new(RichText::new(objective).strong()).truncate())
                    .on_hover_text(format!("{objective}\n\nrun {}", row.id));
            });
            let tasks = row
                .tasks
                .as_ref()
                .map_or_else(|| "tasks unknown".to_string(), task_summary);
            let workers = match row.workers {
                1 => "1 worker".to_string(),
                n => format!("{n} workers"),
            };
            ui.add(egui::Label::new(RichText::new(format!("{tasks} · {workers}")).small()).wrap());
        });
        let clicked = frame
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Focus this run")
            .clicked();
        if clicked {
            response.focus_run = Some(row.id.clone());
            response.select = snapshot.run(&row.id).and_then(|r| r.coordinator.clone());
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

fn filters_section(ui: &mut Ui, snapshot: &FleetSnapshot, filter: &mut FleetFilter) {
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
                .hint_text("Search label, branch, objective")
                .desired_width(ui.available_width()),
        );
    });
    ui.checkbox(&mut filter.show_loose, "Loose terminals");
    ui.checkbox(&mut filter.show_released, "Recently released");

    ui.add_space(4.0);
    ui.label(RichText::new("Repo").strong());
    let repos = repo_counts(snapshot);
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
    ui.label(RichText::new("State").strong())
        .on_hover_text("Pick states to show only those; none picked shows all");
    let states = state_counts(snapshot);
    ui.horizontal_wrapped(|ui| {
        for state in ALL_STATES {
            let count = states.get(&state).copied().unwrap_or(0);
            let text = RichText::new(format!("{} {count}", state_label(state)))
                .color(state_color(state, ui.visuals().dark_mode));
            if ui
                .selectable_label(filter.states.contains(&state), text)
                .clicked()
            {
                toggle_chip(&mut filter.states, state);
            }
        }
    });

    ui.add_space(4.0);
    ui.label(RichText::new("Kind").strong())
        .on_hover_text("Pick kinds to show only those; none picked shows all");
    let kinds = kind_counts(snapshot);
    ui.horizontal_wrapped(|ui| {
        for kind in ALL_KINDS {
            let count = kinds.get(&kind).copied().unwrap_or(0);
            if count == 0 && !filter.kinds.contains(&kind) {
                continue;
            }
            let text = format!("{} {count}", kind_label(kind));
            if ui
                .selectable_label(filter.kinds.contains(&kind), text)
                .clicked()
            {
                toggle_chip(&mut filter.kinds, kind);
            }
        }
    });

    ui.add_space(4.0);
    let hidden = filter.hidden(snapshot).len();
    ui.horizontal(|ui| {
        ui.weak(format!(
            "{} of {} shown",
            snapshot.nodes.len() - hidden,
            snapshot.nodes.len()
        ));
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
