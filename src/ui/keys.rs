//! Keyboard navigation: key → command mapping, node/run order and the shortcut help overlay.

use std::collections::{BTreeSet, HashMap, HashSet};

use eframe::egui::{self, Event, Key};

use crate::model::{EdgeKind, FleetSnapshot, NodeId, NodeKind};
use crate::ui::sidebar::run_rows;

/// A keyboard command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCommand {
    NextNode,
    PrevNode,
    NextRun,
    PrevRun,
    /// Select the current node (the first one if none) and open its panel.
    OpenPanel,
    /// Focus the terminal panel's input box.
    FocusInput,
    /// Leave the focused text field, else clear the selection.
    Escape,
    OpenInOrca,
    Fit,
    ToggleHelp,
    /// Ask to close (kill) the selected nodes' sessions.
    CloseSession,
    /// Open (or close) the Queen's chat.
    ToggleChat,
}

/// Shortcuts listed in the help overlay.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘K  /", "Search nodes"),
    ("⌘J", "Open / close the Queen's chat"),
    ("j  k", "Next / previous node"),
    ("J  K", "Next / previous run"),
    ("Enter", "Select node, open its panel"),
    ("i", "Type into the terminal's input box"),
    (
        "Esc",
        "Leave the input box, then clear the selection (several: keep one)",
    ),
    ("o", "Open the terminal in Orca"),
    ("f", "Fit the graph to the screen"),
    ("x  ⌫  ⌦", "Close (kill) the selected sessions…"),
    ("drag", "Select the nodes in a box (⇧/⌘ adds)"),
    ("⇧/⌘ click", "Add or remove a node from the selection"),
    ("right-drag", "Pan the graph"),
    ("?", "Toggle this help"),
];

/// The command `event` triggers. Only Esc and ⌘J work while a text field has focus; the letter
/// commands come from text events so they follow the keyboard layout.
pub fn command_for_event(event: &Event, text_focused: bool) -> Option<KeyCommand> {
    match event {
        Event::Key {
            key: Key::Escape,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => Some(KeyCommand::Escape),
        Event::Key {
            key: Key::J,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command_only() => Some(KeyCommand::ToggleChat),
        _ if text_focused => None,
        Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => Some(KeyCommand::OpenPanel),
        Event::Key {
            key: Key::Delete | Key::Backspace,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() || modifiers.command_only() => Some(KeyCommand::CloseSession),
        Event::Text(text) => command_for_text(text),
        _ => None,
    }
}

fn command_for_text(text: &str) -> Option<KeyCommand> {
    Some(match text {
        "j" => KeyCommand::NextNode,
        "k" => KeyCommand::PrevNode,
        "J" => KeyCommand::NextRun,
        "K" => KeyCommand::PrevRun,
        "i" => KeyCommand::FocusInput,
        "o" => KeyCommand::OpenInOrca,
        "f" => KeyCommand::Fit,
        "?" => KeyCommand::ToggleHelp,
        "x" => KeyCommand::CloseSession,
        _ => return None,
    })
}

/// Takes this frame's command events out of the input queue (so no widget also sees them)
/// and returns their commands in order.
pub fn take_commands(ctx: &egui::Context) -> Vec<KeyCommand> {
    let text_focused = ctx.text_edit_focused();
    let mut commands = Vec::new();
    ctx.input_mut(|i| {
        i.events
            .retain(|e| match command_for_event(e, text_focused) {
                Some(command) => {
                    commands.push(command);
                    false
                }
                None => true,
            });
    });
    commands
}

/// A run's visible nodes in navigation order; `run_id` is `None` for the trailing group of
/// nodes outside any listed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavGroup {
    pub run_id: Option<String>,
    pub nodes: Vec<NodeId>,
}

/// Visible nodes grouped for navigation: the queen first (in a group of its own), then runs
/// in sidebar order, each with its coordinator
/// first, then workers by label; then nodes outside the listed runs with loose terminals
/// last. Every node is followed by its subagents (by label). Hidden nodes are skipped and
/// empty groups dropped.
pub fn nav_groups(snapshot: &FleetSnapshot, hidden: &BTreeSet<NodeId>) -> Vec<NavGroup> {
    let by_id: HashMap<&NodeId, &crate::model::Node> =
        snapshot.nodes.iter().map(|n| (&n.id, n)).collect();
    let mut children: HashMap<&NodeId, Vec<&NodeId>> = HashMap::new();
    for edge in snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Subagent)
    {
        if by_id.contains_key(&edge.to) {
            children.entry(&edge.from).or_default().push(&edge.to);
        }
    }
    for list in children.values_mut() {
        list.sort_by(|a, b| by_id[a].label.cmp(&by_id[b].label).then(a.cmp(b)));
    }

    let mut visited: HashSet<&NodeId> = HashSet::new();
    let mut visit = |id: &NodeId, out: &mut Vec<NodeId>| {
        let Some((start, _)) = by_id.get_key_value(id) else {
            return;
        };
        let mut stack = vec![*start];
        let mut order = Vec::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            order.push(id);
            if let Some(kids) = children.get(id) {
                stack.extend(kids.iter().rev());
            }
        }
        out.extend(
            order
                .into_iter()
                .filter(|id| !hidden.contains(*id))
                .cloned(),
        );
    };

    let mut groups = Vec::new();
    let mut queen = Vec::new();
    for node in snapshot.nodes.iter().filter(|n| n.kind == NodeKind::Queen) {
        visit(&node.id, &mut queen);
    }
    groups.push(NavGroup {
        run_id: None,
        nodes: queen,
    });
    for row in run_rows(snapshot) {
        let mut members: Vec<&crate::model::Node> = snapshot
            .nodes
            .iter()
            .filter(|n| n.run_id.as_deref() == Some(&row.id) && n.kind != NodeKind::Subagent)
            .collect();
        // Coordinator, then attendant, then the rest.
        let rank = |k: NodeKind| match k {
            NodeKind::Coordinator => 0,
            NodeKind::Attendant => 1,
            _ => 2,
        };
        members
            .sort_by(|a, b| (rank(a.kind), &a.label, &a.id).cmp(&(rank(b.kind), &b.label, &b.id)));
        let mut nodes = Vec::new();
        if let Some(coordinator) = snapshot.run(&row.id).and_then(|r| r.coordinator.as_ref()) {
            visit(coordinator, &mut nodes);
        }
        for node in members {
            visit(&node.id, &mut nodes);
        }
        groups.push(NavGroup {
            run_id: Some(row.id),
            nodes,
        });
    }

    let mut rest = Vec::new();
    let others = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Subagent);
    let (loose, others): (Vec<_>, Vec<_>) = others.partition(|n| n.kind == NodeKind::Terminal);
    for node in others.into_iter().chain(loose) {
        visit(&node.id, &mut rest);
    }
    for node in &snapshot.nodes {
        visit(&node.id, &mut rest);
    }
    groups.push(NavGroup {
        run_id: None,
        nodes: rest,
    });
    groups.retain(|g| !g.nodes.is_empty());
    groups
}

/// [`nav_groups`] flattened.
pub fn nav_order(snapshot: &FleetSnapshot, hidden: &BTreeSet<NodeId>) -> Vec<NodeId> {
    nav_groups(snapshot, hidden)
        .into_iter()
        .flat_map(|g| g.nodes)
        .collect()
}

/// The node `delta` steps from `current` in `order`, wrapping; from no (or an unlisted)
/// node, the first node going forward and the last going back.
pub fn step_node(order: &[NodeId], current: Option<&NodeId>, delta: isize) -> Option<NodeId> {
    if order.is_empty() {
        return None;
    }
    let len = order.len() as isize;
    let target = match current.and_then(|c| order.iter().position(|id| id == c)) {
        Some(i) => (i as isize + delta).rem_euclid(len),
        None if delta >= 0 => 0,
        None => len - 1,
    };
    Some(order[target as usize].clone())
}

/// The first node of the run group `delta` runs from `current`'s, wrapping. From a node
/// outside any run (or none), forward goes to the first run and back to the last.
pub fn step_run(groups: &[NavGroup], current: Option<&NodeId>, delta: isize) -> Option<NodeId> {
    let runs: Vec<&NavGroup> = groups.iter().filter(|g| g.run_id.is_some()).collect();
    if runs.is_empty() {
        return None;
    }
    let len = runs.len() as isize;
    let at = current.and_then(|c| runs.iter().position(|g| g.nodes.contains(c)));
    let target = match at {
        Some(i) => (i as isize + delta).rem_euclid(len),
        None if delta >= 0 => 0,
        None => len - 1,
    };
    runs[target as usize].nodes.first().cloned()
}

/// Draws the shortcut list while `open`; closes on its close button.
pub fn help_window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Keyboard shortcuts")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -48.0))
        .show(ctx, |ui| {
            egui::Grid::new("shortcuts")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for (keys, what) in SHORTCUTS {
                        ui.monospace(*keys);
                        ui.label(*what);
                        ui.end_row();
                    }
                });
            ui.weak("Letter keys work while no text field has focus.");
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Node, NodeState, RunSummary};
    use crate::ui::graph::center_pan;
    use eframe::egui::Modifiers;

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn text(s: &str) -> Event {
        Event::Text(s.to_string())
    }

    #[test]
    fn key_mapping() {
        use KeyCommand::*;
        let cases = [
            ("j", NextNode),
            ("k", PrevNode),
            ("J", NextRun),
            ("K", PrevRun),
            ("i", FocusInput),
            ("o", OpenInOrca),
            ("f", Fit),
            ("?", ToggleHelp),
            ("x", CloseSession),
        ];
        for (s, command) in cases {
            assert_eq!(command_for_event(&text(s), false), Some(command), "{s}");
            assert_eq!(command_for_event(&text(s), true), None, "{s}");
        }
        assert_eq!(command_for_event(&text("X"), false), None);
        assert_eq!(command_for_event(&text("jj"), false), None);
        for focused in [false, true] {
            assert_eq!(
                command_for_event(&key(Key::J, Modifiers::COMMAND), focused),
                Some(ToggleChat),
                "Cmd+J works in text fields too"
            );
        }
        assert_eq!(
            command_for_event(&key(Key::J, Modifiers::NONE), false),
            None
        );
        assert_eq!(
            command_for_event(&key(Key::Enter, Modifiers::NONE), false),
            Some(OpenPanel)
        );
        assert_eq!(
            command_for_event(&key(Key::Enter, Modifiers::NONE), true),
            None
        );
        assert_eq!(
            command_for_event(&key(Key::Enter, Modifiers::SHIFT), false),
            None
        );
        for focused in [false, true] {
            assert_eq!(
                command_for_event(&key(Key::Escape, Modifiers::NONE), focused),
                Some(Escape)
            );
        }
        for k in [Key::Delete, Key::Backspace] {
            for m in [Modifiers::NONE, Modifiers::COMMAND] {
                assert_eq!(command_for_event(&key(k, m), false), Some(CloseSession));
                // Backspace in the input box still edits text.
                assert_eq!(command_for_event(&key(k, m), true), None);
            }
            assert_eq!(command_for_event(&key(k, Modifiers::SHIFT), false), None);
        }
        // Letter keys act through their text events, not the key events.
        assert_eq!(
            command_for_event(&key(Key::J, Modifiers::NONE), false),
            None
        );
        let release = Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(command_for_event(&release, false), None);
    }

    fn node(id: &str, kind: NodeKind, run: Option<&str>, label: &str) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind,
            label: label.to_string(),
            terminal_handle: None,
            live: true,
            run_id: run.map(str::to_string),
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

    fn id(s: &str) -> NodeId {
        NodeId(s.to_string())
    }

    fn run(run_id: &str, coordinator: &str) -> RunSummary {
        RunSummary {
            id: run_id.into(),
            objective: None,
            coordinator: Some(id(coordinator)),
            task_counts: None,
        }
    }

    /// Run `old` (idle) and run `new` (recently active, listed first in the sidebar), a loose
    /// terminal listed before everything, and subagents under a worker and a loose terminal.
    fn snapshot() -> FleetSnapshot {
        let mut s = FleetSnapshot::default();
        s.runs = vec![run("old", "c_old"), run("new", "c_new")];
        let mut c_new = node("c_new", NodeKind::Coordinator, Some("new"), "coord");
        c_new.last_output_ms = Some(9_000);
        let mut c_old = node("c_old", NodeKind::Coordinator, Some("old"), "coord");
        c_old.last_output_ms = Some(1_000);
        s.nodes = vec![
            node("loose", NodeKind::Terminal, None, "shell"),
            node("w_old", NodeKind::Worker, Some("old"), "only"),
            node("w_b", NodeKind::Worker, Some("new"), "beta"),
            c_old,
            node("w_a", NodeKind::Worker, Some("new"), "alpha"),
            c_new,
            node("sub_2", NodeKind::Subagent, Some("new"), "z-sub"),
            node("sub_1", NodeKind::Subagent, Some("new"), "a-sub"),
            node("sub_nested", NodeKind::Subagent, Some("new"), "nested"),
            node("sub_loose", NodeKind::Subagent, None, "loose-sub"),
        ];
        let sub = |from: &str, to: &str| Edge {
            from: id(from),
            to: id(to),
            kind: EdgeKind::Subagent,
        };
        s.edges = vec![
            sub("w_b", "sub_2"),
            sub("w_b", "sub_1"),
            sub("sub_1", "sub_nested"),
            sub("loose", "sub_loose"),
        ];
        s
    }

    fn strs(ids: &[NodeId]) -> Vec<&str> {
        ids.iter().map(NodeId::as_str).collect()
    }

    #[test]
    fn nav_order_runs_coordinator_workers_subagents_then_loose() {
        let s = snapshot();
        let order = nav_order(&s, &BTreeSet::new());
        assert_eq!(
            strs(&order),
            [
                "c_new",
                "w_a",
                "w_b",
                "sub_1",
                "sub_nested",
                "sub_2",
                "c_old",
                "w_old",
                "loose",
                "sub_loose",
            ]
        );
        let groups = nav_groups(&s, &BTreeSet::new());
        let runs: Vec<Option<&str>> = groups.iter().map(|g| g.run_id.as_deref()).collect();
        assert_eq!(runs, [Some("new"), Some("old"), None]);
    }

    #[test]
    fn nav_order_skips_hidden_nodes_but_keeps_their_visible_subagents() {
        let s = snapshot();
        let hidden: BTreeSet<NodeId> = [id("w_b"), id("c_old"), id("w_old"), id("loose")].into();
        let groups = nav_groups(&s, &hidden);
        assert_eq!(
            strs(&nav_order(&s, &hidden)),
            ["c_new", "w_a", "sub_1", "sub_nested", "sub_2", "sub_loose"]
        );
        // Run `old` has no visible node left and drops out.
        let runs: Vec<Option<&str>> = groups.iter().map(|g| g.run_id.as_deref()).collect();
        assert_eq!(runs, [Some("new"), None]);
    }

    #[test]
    fn queen_comes_first_and_runs_still_step() {
        let mut s = snapshot();
        s.add_queen();
        let order = nav_order(&s, &BTreeSet::new());
        assert_eq!(order.first(), Some(&NodeId::queen()));
        assert_eq!(order.len(), 11);
        let groups = nav_groups(&s, &BTreeSet::new());
        let runs: Vec<Option<&str>> = groups.iter().map(|g| g.run_id.as_deref()).collect();
        assert_eq!(runs, [None, Some("new"), Some("old"), None]);
        assert_eq!(step_run(&groups, None, 1), Some(id("c_new")));
        assert_eq!(
            step_node(&order, Some(&id("sub_loose")), 1),
            Some(NodeId::queen())
        );
    }

    #[test]
    fn nav_order_of_empty_snapshot() {
        let s = FleetSnapshot::default();
        assert!(nav_order(&s, &BTreeSet::new()).is_empty());
        assert_eq!(step_node(&[], None, 1), None);
        assert_eq!(step_run(&[], None, 1), None);
    }

    #[test]
    fn step_node_wraps() {
        let order = [id("a"), id("b"), id("c")];
        assert_eq!(step_node(&order, None, 1), Some(id("a")));
        assert_eq!(step_node(&order, None, -1), Some(id("c")));
        assert_eq!(step_node(&order, Some(&id("a")), 1), Some(id("b")));
        assert_eq!(step_node(&order, Some(&id("c")), 1), Some(id("a")));
        assert_eq!(step_node(&order, Some(&id("a")), -1), Some(id("c")));
        assert_eq!(step_node(&order, Some(&id("zz")), 1), Some(id("a")));
    }

    #[test]
    fn step_run_jumps_to_first_node_of_next_run() {
        let s = snapshot();
        let groups = nav_groups(&s, &BTreeSet::new());
        assert_eq!(step_run(&groups, None, 1), Some(id("c_new")));
        assert_eq!(step_run(&groups, None, -1), Some(id("c_old")));
        assert_eq!(step_run(&groups, Some(&id("sub_2")), 1), Some(id("c_old")));
        assert_eq!(step_run(&groups, Some(&id("w_old")), 1), Some(id("c_new")));
        assert_eq!(step_run(&groups, Some(&id("w_a")), -1), Some(id("c_old")));
        assert_eq!(step_run(&groups, Some(&id("loose")), 1), Some(id("c_new")));
    }

    #[test]
    fn center_pan_puts_point_in_the_middle() {
        let view = egui::vec2(800.0, 600.0);
        let point = egui::pos2(100.0, -50.0);
        let pan = center_pan(point, view, 2.0);
        assert_eq!(point.to_vec2() * 2.0 + pan, view / 2.0);
    }
}
