//! Shared clan selection, filters, badges and tooltip data, independent of rendering.
mod colors;
mod menu;
mod tooltip;
use crate::claude::Usage;
use crate::model::{ClanSnapshot, Node, NodeId, NodeKind, RepoIndex};
pub use colors::*;
use eframe::egui::{self, Ui};
pub use menu::MenuPick;
use std::collections::{BTreeSet, HashMap, HashSet};
pub use tooltip::{
    coordinator_gone_note, format_age, format_ago, kind_label, preview_line, state_label,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadBadge {
    pub count: u32,
    pub severity: crate::notify::Severity,
}

/// Applied by the farm once its current rooms and figures have been laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// Pan to these nodes without changing the user's zoom.
    Nodes(Vec<NodeId>),
    Run(String),
    Repo(String),
}

#[derive(Default)]
pub struct ClanView {
    nodes: HashMap<NodeId, Node>,
    objectives: HashMap<String, String>,
    repos: HashMap<NodeId, String>,
    coordinator_gone: HashSet<NodeId>,
    usage: HashMap<NodeId, Usage>,
    building: HashSet<NodeId>,
    pr_ready: HashSet<NodeId>,
    unread: HashMap<NodeId, UnreadBadge>,
    hidden: BTreeSet<NodeId>,
    selection: BTreeSet<NodeId>,
    primary: Option<NodeId>,
    focus: Option<Focus>,
}

impl ClanView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sync(&mut self, snapshot: &ClanSnapshot) {
        self.nodes = snapshot
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.clone()))
            .collect();
        self.repos = RepoIndex::new(snapshot).by_node;
        for node in snapshot.parked_nodes() {
            self.repos
                .insert(node.id.clone(), crate::park::RESTING_AREA.to_owned());
        }
        self.objectives = snapshot
            .runs
            .iter()
            .filter_map(|r| Some((r.id.clone(), r.objective.clone()?)))
            .collect();
        self.coordinator_gone = snapshot
            .nodes
            .iter()
            .filter(|n| snapshot.coordinator_gone(n))
            .map(|n| n.id.clone())
            .collect();
        self.prune_selection();
    }

    fn prune_selection(&mut self) {
        self.selection
            .retain(|id| self.nodes.contains_key(id) && !self.hidden.contains(id));
        self.primary = self.selected();
    }

    pub fn selection(&self) -> BTreeSet<NodeId> {
        self.selection.clone()
    }

    pub fn set_selection(&mut self, ids: &BTreeSet<NodeId>, primary: Option<&NodeId>) {
        self.selection = ids
            .iter()
            .filter(|id| self.is_visible(id))
            .cloned()
            .collect();
        self.primary = primary
            .filter(|id| self.selection.contains(*id))
            .cloned()
            .or_else(|| self.selected());
    }

    pub fn set_hidden(&mut self, hidden: &BTreeSet<NodeId>) {
        self.hidden = hidden.clone();
        self.prune_selection();
    }
    pub fn hidden(&self) -> &BTreeSet<NodeId> {
        &self.hidden
    }
    pub fn is_visible(&self, id: &NodeId) -> bool {
        self.nodes.contains_key(id) && !self.hidden.contains(id)
    }
    pub fn set_pr_ready(&mut self, nodes: HashSet<NodeId>) {
        self.pr_ready = nodes;
    }
    pub fn pr_ready(&self) -> &HashSet<NodeId> {
        &self.pr_ready
    }
    pub fn set_unread(&mut self, nodes: HashMap<NodeId, UnreadBadge>) {
        self.unread = nodes;
    }
    pub fn unread(&self, id: &NodeId) -> Option<UnreadBadge> {
        self.unread.get(id).copied()
    }
    pub fn focus_nodes(&mut self, ids: &[NodeId]) {
        self.focus = Some(Focus::Nodes(ids.to_vec()));
    }
    pub fn center_on(&mut self, id: &NodeId) {
        self.focus_nodes(std::slice::from_ref(id));
    }
    pub fn focus_run(&mut self, run: &str) {
        self.focus = Some(Focus::Run(run.to_owned()));
    }
    pub fn focus_repo(&mut self, repo: &str) {
        self.focus = Some(Focus::Repo(repo.to_owned()));
    }
    pub fn run_repo(&self, run: &str) -> Option<&str> {
        let node = self
            .nodes
            .values()
            .find(|n| n.run_id.as_deref() == Some(run) && !n.is_parked())?;
        self.node_repo(&node.id)
    }
    pub fn node_run(&self, id: &NodeId) -> Option<&str> {
        self.nodes.get(id)?.run_id.as_deref()
    }
    pub fn take_focus(&mut self) -> Option<Focus> {
        self.focus.take()
    }
    pub fn set_usage(&mut self, usage: HashMap<NodeId, Usage>) {
        self.usage = usage;
    }
    pub fn set_building(&mut self, nodes: HashSet<NodeId>) {
        self.building = nodes;
    }
    pub fn building(&self) -> &HashSet<NodeId> {
        &self.building
    }
    pub fn node_repo(&self, id: &NodeId) -> Option<&str> {
        self.repos.get(id).map(String::as_str)
    }
    pub fn selected(&self) -> Option<NodeId> {
        let selection = self.selection();
        self.primary
            .clone()
            .filter(|p| selection.contains(p))
            .or_else(|| selection.into_iter().next())
    }
    pub fn set_selected(&mut self, id: Option<&NodeId>) {
        let ids: BTreeSet<NodeId> = id.cloned().into_iter().collect();
        self.set_selection(&ids, id);
    }
    pub fn click_node(&mut self, id: &NodeId, additive: bool) {
        let mut selection = self.selection();
        if additive {
            if selection.remove(id) {
                let primary = self.selected().filter(|p| p != id);
                self.set_selection(&selection, primary.as_ref());
            } else {
                selection.insert(id.clone());
                self.set_selection(&selection, Some(id));
            }
        } else if selection.len() == 1 && selection.contains(id) {
            self.set_selected(None);
        } else {
            self.set_selected(Some(id));
        }
    }
    pub fn node_tooltip_ui(&self, ui: &mut Ui, id: &NodeId) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        let objective = node
            .run_id
            .as_ref()
            .and_then(|r| self.objectives.get(r))
            .map(String::as_str);
        let usage = self.usage.get(&node.id);
        let gone = self.coordinator_gone.contains(id);
        let repo = self.repos.get(&node.id).map(String::as_str);
        tooltip::node_tooltip(ui, node, objective, repo, usage, now_ms());
        if gone {
            tooltip::coordinator_gone_note(ui);
        }
    }
}
pub fn is_additive(modifiers: egui::Modifiers) -> bool {
    modifiers.shift || modifiers.command
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests;
