//! Headless pointer tests: box selection, Shift/Cmd toggling, node drag, right-drag pan and the
//! right-click menu.

use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, Rect, Vec2};

use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};

const RUN: &str = "run_a";
const SCREEN: Vec2 = Vec2::new(1000.0, 800.0);
const PAN: Vec2 = Vec2::new(100.0, 100.0);

fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}

fn node(name: &str, kind: NodeKind, run: Option<&str>) -> Node {
    Node {
        id: id(name),
        kind,
        label: name.to_string(),
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

fn fleet() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.runs = vec![RunSummary {
        id: RUN.into(),
        objective: None,
        coordinator: Some(id("coord")),
        task_counts: None,
    }];
    s.nodes
        .push(node("coord", NodeKind::Coordinator, Some(RUN)));
    s.nodes.push(node("loose", NodeKind::Terminal, None));
    for w in ["w1", "w2", "w3"] {
        s.nodes.push(node(w, NodeKind::Worker, Some(RUN)));
        s.edges.push(Edge {
            from: id("coord"),
            to: id(w),
            kind: EdgeKind::CoordinatorWorker,
        });
    }
    s
}

/// Canvas positions: w1 and w2 side by side, w3 below them, the coordinator and the loose
/// terminal far off to the right.
const POSITIONS: [(&str, [f32; 2]); 5] = [
    ("w1", [0.0, 0.0]),
    ("w2", [150.0, 0.0]),
    ("w3", [0.0, 300.0]),
    ("coord", [600.0, 0.0]),
    ("loose", [600.0, 400.0]),
];

struct Harness {
    ctx: egui::Context,
    view: GraphView,
    time: f64,
    modifiers: Modifiers,
    last: GraphResponse,
    /// Zoom/pan after the last frame.
    meta: MetadataFrame,
}

impl Harness {
    /// A graph with the layout paused, zoom 1 and a fixed pan, so canvas positions map to
    /// known screen positions.
    fn new() -> Self {
        let mut view = GraphView::new();
        view.sync(&fleet());
        for (name, [x, y]) in POSITIONS {
            view.set_node_position(&id(name), Pos2::new(x, y));
        }
        let mut h = Self {
            ctx: egui::Context::default(),
            view,
            time: 0.0,
            modifiers: Modifiers::NONE,
            last: GraphResponse::default(),
            meta: MetadataFrame::default(),
        };
        h.frame(vec![]);
        h.view.auto_fit = false;
        h.view.wake_layout = false;
        let graph_id = Some(h.view.id.clone());
        let input = h.input(vec![]);
        h.ctx
            .run_ui(input, |ui| {
                let mut state = get_layout_state::<FleetLayoutState>(ui, graph_id.clone());
                state.set_running(false);
                set_layout_state(ui, state, graph_id.clone());
                let mut meta = MetadataFrame::new(graph_id.clone()).load(ui);
                meta.zoom = 1.0;
                meta.pan = PAN;
                meta.save(ui);
            })
            .textures_delta
            .clear();
        for (name, [x, y]) in POSITIONS {
            h.view.set_node_position(&id(name), Pos2::new(x, y));
        }
        h.view.wake_layout = false;
        h.frame(vec![]);
        h
    }

    fn input(&mut self, mut events: Vec<Event>) -> egui::RawInput {
        self.time += 1.0 / 60.0;
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            events,
            ..Default::default()
        }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let input = self.input(events);
        let view = &mut self.view;
        let mut last = GraphResponse::default();
        let mut meta = MetadataFrame::default();
        self.ctx
            .run_ui(input, |ui| {
                last = view.show(ui);
                meta = MetadataFrame::new(Some(view.id.clone())).load(ui);
            })
            .textures_delta
            .clear();
        self.last = last;
        self.meta = meta;
    }

    fn meta(&self) -> MetadataFrame {
        self.meta.clone()
    }

    /// Where canvas point `p` is on screen.
    fn to_screen(&self, p: Pos2) -> Pos2 {
        let meta = self.meta();
        let top_left = self.view.last_top_left.unwrap_or(Pos2::ZERO);
        top_left + meta.pan + p.to_vec2() * meta.zoom
    }

    fn node_screen(&self, name: &str) -> Pos2 {
        self.to_screen(self.view.node_position(&id(name)).unwrap())
    }

    fn button(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
        let modifiers = self.modifiers;
        self.frame(vec![Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        }]);
    }

    fn move_to(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
    }

    /// Presses `button` at `from`, moves to `to` in a few steps and releases there.
    fn drag(&mut self, button: PointerButton, from: Pos2, to: Pos2) {
        self.move_to(from);
        self.button(from, button, true);
        for step in 1..=4 {
            self.move_to(from + (to - from) * (step as f32 / 4.0));
        }
        self.button(to, button, false);
        self.move_to(to);
    }

    fn click(&mut self, button: PointerButton, at: Pos2) {
        self.move_to(at);
        self.button(at, button, true);
        self.button(at, button, false);
        self.frame(vec![]);
    }

    fn selection(&self) -> Vec<&str> {
        let mut out: Vec<&str> = POSITIONS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| self.view.selection().contains(&id(name)))
            .collect();
        out.sort_unstable();
        out
    }
}

#[test]
fn circle_rect_intersection() {
    let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(10.0, 10.0));
    assert!(circle_intersects_rect(Pos2::new(5.0, 5.0), 1.0, rect));
    assert!(circle_intersects_rect(Pos2::new(12.0, 5.0), 2.5, rect));
    assert!(!circle_intersects_rect(Pos2::new(12.0, 12.0), 2.5, rect));
}

#[test]
fn box_drag_on_empty_space_selects_the_nodes_it_covers() {
    let mut h = Harness::new();
    let (w1, w2) = (h.node_screen("w1"), h.node_screen("w2"));
    let from = w1 + Vec2::new(-40.0, -40.0);
    let to = w2 + Vec2::new(40.0, 40.0);
    h.move_to(from);
    h.button(from, PointerButton::Primary, true);
    h.move_to(from + Vec2::new(30.0, 30.0));
    h.move_to(to);
    assert!(h.view.selection_box().is_some(), "box drawn while dragging");
    h.button(to, PointerButton::Primary, false);
    h.frame(vec![]);

    assert!(h.view.selection_box().is_none());
    assert_eq!(h.selection(), vec!["w1", "w2"]);
    assert_eq!(h.last.selection.len(), 2);
    assert!(h.last.selected.is_some());
    // Nothing moved and the view did not pan.
    assert_eq!(h.view.node_position(&id("w1")), Some(Pos2::ZERO));
    assert_eq!(h.meta().pan, PAN);
}

#[test]
fn plain_box_replaces_and_shift_box_adds() {
    let mut h = Harness::new();
    let w1 = h.node_screen("w1");
    h.drag(
        PointerButton::Primary,
        w1 + Vec2::new(-30.0, -30.0),
        w1 + Vec2::new(30.0, 30.0),
    );
    assert_eq!(h.selection(), vec!["w1"]);

    let w3 = h.node_screen("w3");
    h.modifiers = Modifiers::SHIFT;
    h.drag(
        PointerButton::Primary,
        w3 + Vec2::new(-30.0, -30.0),
        w3 + Vec2::new(30.0, 30.0),
    );
    assert_eq!(h.selection(), vec!["w1", "w3"]);

    h.modifiers = Modifiers::NONE;
    let loose = h.node_screen("loose");
    h.drag(
        PointerButton::Primary,
        loose + Vec2::new(-30.0, -30.0),
        loose + Vec2::new(30.0, 30.0),
    );
    assert_eq!(h.selection(), vec!["loose"]);
}

#[test]
fn shift_or_cmd_click_toggles_and_plain_click_selects_one() {
    let mut h = Harness::new();
    h.click(PointerButton::Primary, h.node_screen("w1"));
    assert_eq!(h.selection(), vec!["w1"]);
    assert_eq!(h.last.selected, Some(id("w1")));

    h.modifiers = Modifiers::SHIFT;
    h.click(PointerButton::Primary, h.node_screen("w2"));
    assert_eq!(h.selection(), vec!["w1", "w2"]);
    assert_eq!(h.last.selected, Some(id("w2")), "the added node is primary");

    h.modifiers = Modifiers::COMMAND;
    h.click(PointerButton::Primary, h.node_screen("coord"));
    assert_eq!(h.selection(), vec!["coord", "w1", "w2"]);
    h.click(PointerButton::Primary, h.node_screen("w2"));
    assert_eq!(h.selection(), vec!["coord", "w1"]);

    // A plain click keeps just the clicked node.
    h.modifiers = Modifiers::NONE;
    h.click(PointerButton::Primary, h.node_screen("w1"));
    assert_eq!(h.selection(), vec!["w1"]);
    // Clicking the only selected node deselects it, as before.
    h.click(PointerButton::Primary, h.node_screen("w1"));
    assert!(h.selection().is_empty());
}

#[test]
fn plain_click_on_empty_space_clears_but_shift_click_keeps() {
    let mut h = Harness::new();
    let mut ids = BTreeSet::from([id("w1"), id("w2")]);
    h.view.set_selection(&ids, Some(&id("w2")));
    let empty = h.to_screen(Pos2::new(300.0, 200.0));
    h.modifiers = Modifiers::SHIFT;
    h.click(PointerButton::Primary, empty);
    assert_eq!(h.selection(), vec!["w1", "w2"]);
    h.modifiers = Modifiers::NONE;
    h.click(PointerButton::Primary, empty);
    assert!(h.selection().is_empty());
    ids.clear();
    assert_eq!(h.view.selection(), ids);
}

#[test]
fn left_drag_on_a_node_moves_it_without_box_selecting() {
    let mut h = Harness::new();
    let w1 = h.node_screen("w1");
    // Ends over w2: a box from w1 would have selected both.
    let to = h.node_screen("w2") + Vec2::new(0.0, 60.0);
    h.move_to(w1);
    h.button(w1, PointerButton::Primary, true);
    for step in 1..=4 {
        h.move_to(w1 + (to - w1) * (step as f32 / 4.0));
        assert!(h.view.selection_box().is_none());
    }
    h.button(to, PointerButton::Primary, false);
    h.frame(vec![]);

    // The layout, woken by the drag, nudges it a little after the release.
    let moved = h.view.node_position(&id("w1")).unwrap();
    assert!(
        (moved - Pos2::new(150.0, 60.0)).length() < 40.0,
        "w1 followed the pointer: {moved:?}"
    );
    assert!(h.selection().is_empty());
    assert_eq!(h.meta().pan, PAN, "no pan");
}

#[test]
fn left_drag_from_empty_space_over_a_node_never_grabs_it() {
    let mut h = Harness::new();
    let w1 = h.node_screen("w1");
    let from = w1 + Vec2::new(-60.0, 0.0);
    h.drag(PointerButton::Primary, from, w1 + Vec2::new(10.0, 10.0));
    assert_eq!(h.view.node_position(&id("w1")), Some(Pos2::ZERO));
    assert_eq!(h.selection(), vec!["w1"]);
}

#[test]
fn right_drag_pans_and_counts_as_manual_navigation() {
    let mut h = Harness::new();
    h.view.set_selected(Some(&id("w1")));
    h.view.fitted_size = Some(SCREEN);
    let from = h.to_screen(Pos2::new(300.0, 200.0));
    // Starting on a node pans too; it does not move the node.
    h.drag(
        PointerButton::Secondary,
        from,
        from + Vec2::new(80.0, -40.0),
    );
    let pan = h.meta().pan;
    assert!(
        (pan - (PAN + Vec2::new(80.0, -40.0))).length() < 1.0,
        "pan {pan:?}"
    );
    assert_eq!(h.selection(), vec!["w1"]);
    assert!(h.view.fitted_size.is_none(), "auto-fit on resize stops");
    assert!(!h.view.auto_fit);
    assert!(h.view.export_layout().manual_view);
    assert!(!egui::Popup::is_any_open(&h.ctx), "a drag opens no menu");

    let w2 = h.node_screen("w2");
    h.drag(PointerButton::Secondary, w2, w2 + Vec2::new(50.0, 0.0));
    assert_eq!(h.view.node_position(&id("w2")), Some(Pos2::new(150.0, 0.0)));
    assert!((h.meta().pan - (pan + Vec2::new(50.0, 0.0))).length() < 1.0);
}

#[test]
fn left_drag_does_not_pan() {
    let mut h = Harness::new();
    let from = h.to_screen(Pos2::new(300.0, 200.0));
    h.drag(PointerButton::Primary, from, from + Vec2::new(100.0, 100.0));
    assert_eq!(h.meta().pan, PAN);
}

#[test]
fn right_click_opens_the_node_menu() {
    let mut h = Harness::new();
    h.click(PointerButton::Secondary, h.node_screen("w1"));
    assert_eq!(h.view.context_node, Some(id("w1")));
    assert!(egui::Popup::is_any_open(&h.ctx));
    assert!(h.selection().is_empty(), "a right-click does not select");
    assert!(h.last.close_requested.is_empty());
}

#[test]
fn right_click_on_a_multi_selection_opens_its_menu() {
    let mut h = Harness::new();
    let ids = BTreeSet::from([id("w1"), id("w2"), id("coord")]);
    h.view.set_selection(&ids, Some(&id("w1")));
    h.click(PointerButton::Secondary, h.node_screen("w2"));
    assert_eq!(h.view.context_node, Some(id("w2")));
    assert!(egui::Popup::is_any_open(&h.ctx));
    assert_eq!(h.view.selection(), ids, "selection kept");
    assert_eq!(select::close_selected_text(3), "Close 3 selected…");
}

#[test]
fn set_selection_keeps_visible_nodes_and_picks_a_primary() {
    let mut h = Harness::new();
    let ids = BTreeSet::from([id("w1"), id("w2"), id("gone")]);
    h.view.set_selection(&ids, Some(&id("gone")));
    assert_eq!(h.selection(), vec!["w1", "w2"]);
    assert_eq!(h.view.selected(), Some(id("w1")));
    h.view.set_selection(&ids, Some(&id("w2")));
    assert_eq!(h.view.selected(), Some(id("w2")));
    // Hiding the primary drops it from the selection; another node becomes primary.
    h.view.set_hidden(&BTreeSet::from([id("w2")]));
    assert_eq!(h.selection(), vec!["w1"]);
    assert_eq!(h.view.selected(), Some(id("w1")));
    h.view.set_selected(None);
    assert!(h.view.selection().is_empty());
    assert_eq!(h.view.selected(), None);
}

/// Screen centre of the text `label` drawn in one more frame, if any.
fn find_text(h: &mut Harness, label: &str) -> Option<Pos2> {
    fn walk(shape: &egui::Shape, label: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == label => {
                Some(t.pos + t.galley.rect.center().to_vec2())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| walk(s, label)),
            _ => None,
        }
    }
    let input = h.input(vec![]);
    let view = &mut h.view;
    let mut output = h.ctx.run_ui(input, |ui| {
        view.show(ui);
    });
    output.textures_delta.clear();
    output.shapes.iter().find_map(|c| walk(&c.shape, label))
}

/// Clicks the open menu's item `label`; returns the graph response of the release frame.
fn click_menu_item(h: &mut Harness, label: &str) -> GraphResponse {
    let at = find_text(h, label).unwrap_or_else(|| panic!("no {label:?} item"));
    h.move_to(at);
    h.button(at, PointerButton::Primary, true);
    h.button(at, PointerButton::Primary, false);
    h.last.clone()
}

#[test]
fn context_menu_whip_requests_the_node() {
    let mut h = Harness::new();
    h.click(PointerButton::Secondary, h.node_screen("w1"));
    let last = click_menu_item(&mut h, "Whip");
    assert_eq!(last.whip_requested, vec![id("w1")]);
    assert!(last.close_requested.is_empty());
}

#[test]
fn context_menu_on_a_multi_selection_whips_all_of_it() {
    let mut h = Harness::new();
    let ids = BTreeSet::from([id("w1"), id("w2"), id("coord")]);
    h.view.set_selection(&ids, Some(&id("w1")));
    h.click(PointerButton::Secondary, h.node_screen("w2"));
    let last = click_menu_item(&mut h, "Whip 3 selected");
    assert_eq!(last.whip_requested, ids.into_iter().collect::<Vec<_>>());
}
