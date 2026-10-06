//! Headless checks that auto-fit keeps every painted node, ring and label inside the graph
//! widget, in a panel layout like the app's.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Vec2, epaint::ClippedShape};

use super::*;
use crate::model::{Edge, EdgeKind, NodeState, RunSummary};

/// Marks the start and end of the graph's shapes in the painted output.
const MARKER: Color32 = Color32::from_rgb(1, 2, 3);
/// Painted shapes may stick out of the widget by this much (anti-aliasing feathering).
const TOLERANCE: f32 = 1.0;

fn node(id: &str, kind: NodeKind, run: Option<&str>, label: &str, state: NodeState) -> Node {
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
        state,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}

/// Two runs with long-labelled workers plus a group of loose terminals.
fn fleet() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    for (run, workers) in [("run_a", 5), ("run_b", 3)] {
        let coord = format!("{run}_coord");
        s.runs.push(RunSummary {
            id: run.into(),
            objective: None,
            coordinator: Some(NodeId(coord.clone())),
            task_counts: None,
        });
        s.nodes.push(node(
            &coord,
            NodeKind::Coordinator,
            Some(run),
            &format!("{run} coordinator terminal with a long title"),
            NodeState::NeedsInput,
        ));
        for w in 0..workers {
            let id = format!("{run}_w{w}");
            s.nodes.push(node(
                &id,
                NodeKind::Worker,
                Some(run),
                &format!("myr-hrw.{w} worker fixing something important in {run}"),
                NodeState::Working,
            ));
            s.edges.push(Edge {
                from: NodeId(coord.clone()),
                to: NodeId(id),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    for t in 0..4 {
        s.nodes.push(node(
            &format!("loose{t}"),
            NodeKind::Terminal,
            None,
            &format!("zsh ~/repo/saker/myrstack-worktrees/loose-terminal-{t}"),
            NodeState::Idle,
        ));
    }
    s
}

/// The app's layout: left sidebar, optional right terminal panel, graph in the central panel.
struct Harness {
    ctx: egui::Context,
    view: GraphView,
    screen: Vec2,
    right_panel: bool,
    time: f64,
}

/// One frame's graph widget rect and the shapes the graph painted.
struct Frame {
    rect: Rect,
    shapes: Vec<ClippedShape>,
}

impl Harness {
    fn new(screen: Vec2, right_panel: bool) -> Self {
        let mut view = GraphView::new();
        view.sync(&fleet());
        Self {
            ctx: egui::Context::default(),
            view,
            screen,
            right_panel,
            time: 0.0,
        }
    }

    fn frame(&mut self) -> Frame {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.screen)),
            time: Some(self.time),
            ..Default::default()
        };
        let (view, right_panel) = (&mut self.view, self.right_panel);
        let mut rect = Rect::NOTHING;
        let mut output = self.ctx.run_ui(input, |ui| {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(260.0)
                .show(ui, fill);
            if right_panel {
                let width = ui.ctx().content_rect().width() * 0.45;
                egui::Panel::right("terminal")
                    .resizable(true)
                    .default_size(width)
                    .min_size(240.0)
                    .show(ui, fill);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                rect = ui.available_rect_before_wrap();
                let marker = Shape::circle_filled(rect.center(), 0.0, MARKER);
                ui.painter().add(marker.clone());
                view.show(ui);
                ui.painter().add(marker);
            });
        });
        output.textures_delta.clear();
        let is_marker = |s: &ClippedShape| matches!(&s.shape, Shape::Circle(c) if c.fill == MARKER);
        let mut in_graph = false;
        let mut shapes = Vec::new();
        for shape in output.shapes {
            if is_marker(&shape) {
                in_graph = !in_graph;
            } else if in_graph {
                shapes.push(shape);
            }
        }
        Frame { rect, shapes }
    }

    fn frames(&mut self, n: usize) -> Frame {
        for _ in 1..n {
            self.frame();
        }
        self.frame()
    }
}

/// Panel contents as wide as the panel, like the app's.
fn fill(ui: &mut egui::Ui) {
    ui.set_min_width(ui.available_width());
}

/// Panics unless every painted shape lies inside the graph widget.
fn assert_inside(frame: &Frame, scenario: &str) {
    assert!(!frame.shapes.is_empty(), "{scenario}: nothing painted");
    let allowed = frame.rect.expand(TOLERANCE);
    let outside: Vec<String> = frame
        .shapes
        .iter()
        .map(|s| s.shape.visual_bounding_rect())
        .filter(|r| r.is_positive() && !allowed.contains_rect(*r))
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(
        outside.is_empty(),
        "{scenario}: {} shapes outside the graph rect {:?}: {outside:?}",
        outside.len(),
        frame.rect
    );
}

const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);
/// Enough frames for the layout to settle and auto-fit to finish.
const SETTLE_FRAMES: usize = 600;

#[test]
fn auto_fit_keeps_graph_inside_with_side_panels() {
    let mut h = Harness::new(SCREEN, true);
    let frame = h.frames(SETTLE_FRAMES);
    assert_inside(&frame, "sidebar + terminal panel");
}

#[test]
fn auto_fit_keeps_graph_inside_during_settling() {
    let mut h = Harness::new(SCREEN, true);
    for i in 0..60 {
        let frame = h.frame();
        if i >= 2 {
            assert_inside(&frame, &format!("settling frame {i}"));
        }
    }
}

#[test]
fn refits_when_terminal_panel_opens_after_settling() {
    let mut h = Harness::new(SCREEN, false);
    assert_inside(&h.frames(SETTLE_FRAMES), "sidebar only");
    h.right_panel = true;
    assert_inside(&h.frames(3), "terminal panel opened");
    h.right_panel = false;
    assert_inside(&h.frames(3), "terminal panel closed");
}

#[test]
fn refits_when_terminal_panel_opens_while_settling() {
    let mut h = Harness::new(SCREEN, false);
    h.frames(20);
    h.right_panel = true;
    assert_inside(&h.frames(SETTLE_FRAMES), "terminal panel opened mid-layout");
}

#[test]
fn refits_when_window_resizes() {
    let mut h = Harness::new(SCREEN, true);
    h.frames(SETTLE_FRAMES);
    h.screen = Vec2::new(1000.0, 700.0);
    assert_inside(&h.frames(3), "window shrunk");
    h.screen = Vec2::new(1800.0, 1100.0);
    assert_inside(&h.frames(3), "window grown");
}

fn zoom(h: &Harness) -> f32 {
    let mut meta = None;
    h.ctx
        .run_ui(egui::RawInput::default(), |ui| {
            meta = Some(MetadataFrame::new(Some(DEFAULT_GRAPH_ID.into())).load(ui));
        })
        .drop_without_applying_deltas();
    meta.unwrap().zoom
}

#[test]
fn no_refit_on_resize_after_manual_navigation() {
    let mut h = Harness::new(SCREEN, true);
    h.frames(SETTLE_FRAMES);
    h.view.center_on(&NodeId("run_a_coord".into()));
    h.frame();
    let before = zoom(&h);
    h.screen = Vec2::new(1000.0, 700.0);
    h.frames(3);
    assert_eq!(zoom(&h), before);
}

#[test]
fn fit_button_refits_after_manual_navigation() {
    let mut h = Harness::new(SCREEN, false);
    h.frames(SETTLE_FRAMES);
    h.view.center_on(&NodeId("run_a_coord".into()));
    h.frame();
    h.right_panel = true;
    h.view.fit_to_screen();
    assert_inside(&h.frames(2), "fit after opening the panel");
    h.right_panel = false;
    assert_inside(&h.frames(2), "panel closed after the fit");
}

/// A square of fixed screen size plus a label-like box that grows with the zoom.
struct Item(Pos2);

impl fit::FitItem for Item {
    fn center(&self) -> Pos2 {
        self.0
    }

    fn extent(&self, zoom: f32) -> Rect {
        Rect::from_min_max(Pos2::new(-5.0, -5.0), Pos2::new(5.0 + 100.0 * zoom, 5.0))
    }
}

#[test]
fn fit_transform_fits_screen_extents() {
    let items = [Item(Pos2::new(0.0, 0.0)), Item(Pos2::new(200.0, 100.0))];
    let view = Vec2::new(600.0, 600.0);
    let (zoom, pan) = fit::fit_transform(&items, view, 10.0, 10.0).unwrap();
    // Width at zoom z: 200z + 100z + 10 = 580.
    assert!((zoom - 570.0 / 300.0).abs() < 1e-3, "{zoom}");
    let left = -5.0 + pan.x;
    let right = 200.0 * zoom + 5.0 + 100.0 * zoom + pan.x;
    assert!(
        (left - 10.0).abs() < 0.1 && (right - 590.0).abs() < 0.1,
        "{left} {right}"
    );
    let top = -5.0 + pan.y;
    let bottom = 100.0 * zoom + 5.0 + pan.y;
    assert!(
        (top + bottom - 600.0).abs() < 0.1,
        "centered vertically: {top} {bottom}"
    );
}

#[test]
fn fit_transform_caps_zoom_and_handles_empty() {
    let items = [Item(Pos2::new(50.0, 50.0))];
    let (zoom, _) = fit::fit_transform(&items, Vec2::splat(800.0), 24.0, 2.0).unwrap();
    assert_eq!(zoom, 2.0);
    assert!(fit::fit_transform::<Item>(&[], Vec2::splat(800.0), 24.0, 2.0).is_none());
}
