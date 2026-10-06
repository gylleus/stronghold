use eframe::egui::{self, Color32, FontId, Pos2, Rect, Shape, pos2, vec2};

use super::zoom::*;

const RECT: Rect = Rect::from_min_max(Pos2::new(100.0, 50.0), Pos2::new(1380.0, 850.0));

fn close(a: Pos2, b: Pos2) -> bool {
    (a - b).length() < 1e-3
}

#[test]
fn fitted_camera_draws_the_farm_as_laid_out() {
    let camera = Camera::default();
    assert!(camera.is_fit());
    let p = pos2(321.0, 654.0);
    assert!(close(camera.transform(RECT) * p, p));
}

#[test]
fn zoom_keeps_the_point_under_the_pointer() {
    let mut camera = Camera::default();
    let anchor = pos2(400.0, 300.0);
    let under = camera.to_farm(RECT, anchor);
    camera.zoom_at(RECT, anchor, 2.5);
    assert_eq!(camera.zoom, 2.5);
    assert!(close(camera.to_farm(RECT, anchor), under));
    assert!(close(camera.transform(RECT) * under, anchor));

    // And again from a zoomed, panned view.
    let anchor = pos2(1000.0, 700.0);
    let under = camera.to_farm(RECT, anchor);
    camera.zoom_at(RECT, anchor, 1.5);
    assert!(close(camera.to_farm(RECT, anchor), under));
}

#[test]
fn zoom_stays_within_limits_and_out_returns_to_fit() {
    let mut camera = Camera::default();
    camera.zoom_at(RECT, RECT.center(), 0.5);
    assert_eq!(camera, Camera::default());
    camera.zoom_at(RECT, pos2(200.0, 100.0), 100.0);
    assert_eq!(camera.zoom, MAX_ZOOM);
    camera.zoom_at(RECT, pos2(1300.0, 800.0), 0.001);
    assert!(camera.is_fit());
    assert!(close(camera.transform(RECT) * RECT.min, RECT.min));
}

#[test]
fn the_view_never_leaves_the_farm() {
    let mut camera = Camera::default();
    camera.pan(RECT, vec2(500.0, 500.0));
    assert_eq!(camera, Camera::default());

    camera.zoom_at(RECT, RECT.center(), 4.0);
    camera.pan(RECT, vec2(1e6, -1e6));
    let t = camera.transform(RECT);
    // Panned as far as it goes: the farm's left edge on the view's left, its bottom on the
    // view's bottom.
    assert!((t * RECT.min).x - RECT.min.x < 1e-3);
    assert!(((t * RECT.max).y - RECT.max.y).abs() < 1e-3);
    let view = Rect::from_two_pos(
        camera.to_farm(RECT, RECT.min),
        camera.to_farm(RECT, RECT.max),
    );
    assert!(RECT.expand(1e-3).contains_rect(view));
}

#[test]
fn pan_moves_the_farm_with_the_swipe() {
    let mut camera = Camera::default();
    camera.zoom_at(RECT, RECT.center(), 2.0);
    let p = pos2(700.0, 400.0);
    let before = camera.transform(RECT) * p;
    camera.pan(RECT, vec2(30.0, -20.0));
    assert!(close(
        camera.transform(RECT) * p,
        before + vec2(30.0, -20.0)
    ));
}

#[test]
fn zoomed_text_is_laid_out_at_the_zoomed_size() {
    let ctx = egui::Context::default();
    let mut output = ctx.clone().run_ui(egui::RawInput::default(), |_| {
        let layer = egui::LayerId::background();
        let painter = egui::Painter::new(ctx.clone(), layer, RECT);
        let start = ctx.graphics(|g| g.get(layer).map(|l| l.next_idx()));
        let start = start.unwrap_or(egui::layers::ShapeIdx(0));
        painter.text(
            pos2(200.0, 100.0),
            egui::Align2::LEFT_TOP,
            "ant",
            FontId::monospace(10.0),
            Color32::WHITE,
        );
        let fitted = ctx.graphics(|g| g.get(layer).unwrap().all_entries().last().cloned());
        let Some(Shape::Text(fitted)) = fitted.map(|c| c.shape) else {
            panic!("expected text");
        };
        let mut camera = Camera::default();
        camera.zoom_at(RECT, RECT.min, 3.0);
        let transform = camera.transform(RECT);
        apply(&ctx, layer, start, transform, RECT);
        let zoomed = ctx.graphics(|g| g.get(layer).unwrap().all_entries().last().cloned());
        let zoomed = zoomed.unwrap();
        assert_eq!(zoomed.clip_rect, RECT);
        let Shape::Text(zoomed) = zoomed.shape else {
            panic!("expected text");
        };
        assert!(close(zoomed.pos, transform * fitted.pos));
        assert_eq!(zoomed.galley.job.sections[0].format.font_id.size, 30.0);
        let ratio = zoomed.galley.size().x / fitted.galley.size().x;
        assert!((ratio - 3.0).abs() < 0.2, "ratio {ratio}");
    });
    output.textures_delta.clear();
}

/// The farm on a full-window view, fed raw input frame by frame as eframe would.
struct Harness {
    ctx: egui::Context,
    time: f64,
    output: Option<egui::FullOutput>,
    snapshot: crate::model::FleetSnapshot,
    graph: crate::ui::graph::GraphView,
    farm: super::AntFarmView,
}

impl Harness {
    fn new() -> Self {
        let snapshot = super::layout_tests::snapshot();
        let mut graph = crate::ui::graph::GraphView::new();
        graph.sync(&snapshot);
        let mut harness = Self {
            ctx: egui::Context::default(),
            time: 0.0,
            output: None,
            snapshot,
            graph,
            farm: super::AntFarmView::new(),
        };
        let rect = harness.frame(vec![]).0;
        assert_eq!(rect, FULL, "the farm fills the window");
        harness
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (Rect, super::AntFarmResponse) {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(FULL),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let mut out = None;
        let (farm, snapshot, graph) = (&mut self.farm, &self.snapshot, &self.graph);
        let mut output = self.ctx.run_ui(input, |ui| {
            let rect = ui.available_rect_before_wrap();
            out = Some((rect, farm.show(ui, snapshot, graph)));
        });
        output.textures_delta.clear();
        self.output = Some(output);
        out.expect("a frame ran")
    }

    /// Runs frames until egui's smoothed scroll has played out.
    fn settle(&mut self) -> super::AntFarmResponse {
        let mut last = self.frame(vec![]).1;
        for _ in 0..120 {
            last = self.frame(vec![]).1;
        }
        last
    }

    /// Where the ant `name` is drawn on screen.
    fn on_screen(&self, name: &str) -> Pos2 {
        let at = super::layout_tests::ant(&super::layout_tests::scene(&self.snapshot), name).pos;
        self.farm.camera.transform(FULL) * at
    }

    fn click(&mut self, at: Pos2) -> super::AntFarmResponse {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![egui::Event::PointerMoved(at)]);
        self.frame(vec![button(true)]);
        self.frame(vec![button(false)]).1
    }
}

/// The test window; matches the layout tests' rect so their scene is the farm's.
const FULL: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 800.0));

fn wheel(unit: egui::MouseWheelUnit, delta: egui::Vec2) -> egui::Event {
    egui::Event::MouseWheel {
        unit,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    }
}

/// A mouse wheel zooms around the pointer: the ant under it stays under it and is still the
/// one hovered, and clicks land on the ants where they are now drawn.
#[test]
fn the_wheel_zooms_around_the_pointer() {
    let mut h = Harness::new();
    let target = h.on_screen("working");
    h.frame(vec![egui::Event::PointerMoved(target)]);
    for _ in 0..8 {
        h.frame(vec![wheel(egui::MouseWheelUnit::Line, vec2(0.0, 1.0))]);
    }
    let response = h.settle();
    assert!(h.farm.camera.zoom > 1.5, "zoomed to {}", h.farm.camera.zoom);
    assert!(close(h.on_screen("working"), target));
    assert_eq!(response.hovered, Some(super::layout_tests::id("working")));

    let other = ["idle", "stalled", "failed", "r1-coord", "loose"]
        .into_iter()
        .find(|name| FULL.shrink(20.0).contains(h.on_screen(name)))
        .expect("another ant is on screen");
    let response = h.click(h.on_screen(other));
    assert_eq!(response.clicked, Some(super::layout_tests::id(other)));

    // Wheeling back out stops at the fitted farm.
    for _ in 0..20 {
        h.frame(vec![wheel(egui::MouseWheelUnit::Line, vec2(0.0, -3.0))]);
    }
    h.settle();
    assert_eq!(h.farm.camera, Camera::default());
}

/// A trackpad pinch (winit's PinchGesture, which egui-winit passes on as `Event::Zoom`)
/// zooms around the pointer.
#[test]
fn a_pinch_zooms_around_the_pointer() {
    let mut h = Harness::new();
    let target = h.on_screen("stalled");
    h.frame(vec![egui::Event::PointerMoved(target)]);
    h.frame(vec![egui::Event::Zoom(1.25)]);
    let response = h.frame(vec![egui::Event::Zoom(1.25)]).1;
    assert!((h.farm.camera.zoom - 1.5625).abs() < 1e-3);
    assert!(close(h.on_screen("stalled"), target));
    assert_eq!(response.hovered, Some(super::layout_tests::id("stalled")));
    h.frame(vec![egui::Event::Zoom(0.1)]);
    assert!(h.farm.camera.is_fit());
}

/// A two-finger trackpad swipe pans and never zooms; at the fitted farm it does nothing.
#[test]
fn a_trackpad_swipe_pans_without_zooming() {
    let mut h = Harness::new();
    let center = FULL.center();
    h.frame(vec![egui::Event::PointerMoved(center)]);
    for _ in 0..5 {
        h.frame(vec![wheel(egui::MouseWheelUnit::Point, vec2(0.0, -40.0))]);
    }
    h.settle();
    assert_eq!(h.farm.camera, Camera::default());

    h.frame(vec![egui::Event::Zoom(3.0)]);
    let before = h.farm.camera;
    for _ in 0..5 {
        h.frame(vec![wheel(egui::MouseWheelUnit::Point, vec2(-30.0, -20.0))]);
    }
    h.settle();
    assert_eq!(h.farm.camera.zoom, before.zoom);
    assert!(h.farm.camera.center.x > before.center.x);
    assert!(h.farm.camera.center.y > before.center.y);
}

/// Double-clicking empty ground fits the farm again.
#[test]
fn double_click_on_empty_ground_fits_the_farm() {
    let mut h = Harness::new();
    h.frame(vec![egui::Event::PointerMoved(FULL.center())]);
    h.frame(vec![egui::Event::Zoom(4.0)]);
    let soil = pos2(FULL.center().x, FULL.bottom() - 4.0);
    h.click(soil);
    assert!(!h.farm.camera.is_fit());
    h.click(soil);
    assert!(h.farm.camera.is_fit());
}

impl Harness {
    fn with_envoy() -> Self {
        let mut h = Self::new();
        let mut envoy = h.snapshot.nodes[0].clone();
        envoy.id = super::layout_tests::id("envoy");
        envoy.kind = crate::model::NodeKind::Envoy;
        h.snapshot.nodes.push(envoy);
        h.graph.sync(&h.snapshot);
        h.frame(vec![]);
        h
    }

    fn tray_center(&self) -> Pos2 {
        let scene = super::layout_tests::scene(&self.snapshot);
        self.farm.camera.transform(FULL) * scene.comms.unwrap().tray_rect().center()
    }
}

#[test]
fn tray_click_opens_without_selecting_or_clearing_at_fit_and_zoomed() {
    for (zoomed, receipt_ms) in [(false, 500), (true, 500), (false, 31), (true, 31)] {
        let mut h = Harness::with_envoy();
        h.farm
            .set_envoy_receipt(crate::ui::envoy_receipt::ReceiptCue::at(
                Some(0),
                receipt_ms,
                false,
            ));
        if zoomed {
            let target = h.tray_center();
            h.farm.camera.zoom_at(FULL, target, 2.0);
            h.farm.camera.pan(FULL, vec2(-15.0, -12.0));
        }
        let at = h.tray_center();
        h.frame(vec![egui::Event::PointerMoved(at)]);
        h.settle();
        assert!(
            crate::ui::test_util::text_center(
                h.output.as_ref().unwrap(),
                "Envoy inbox: 0 waiting · outbox"
            )
            .is_some()
        );
        let response = h.click(at);
        assert!(response.open_envoy);
        assert!(response.clicked.is_none());
        assert!(!response.clicked_empty);
        assert!(response.hovered.is_none());
        assert_eq!(
            h.output.as_ref().unwrap().platform_output.cursor_icon,
            egui::CursorIcon::PointingHand
        );
        let camera = h.farm.camera;
        h.click(at);
        assert_eq!(
            h.farm.camera, camera,
            "double-clicking the tray must not reset zoom"
        );
        let outside = h.click(FULL.right_bottom() - vec2(5.0, 5.0));
        assert!(!outside.open_envoy);
        assert!(outside.clicked_empty);
    }
    let mut absent = Harness::new();
    assert!(!absent.click(pos2(150.0, 200.0)).open_envoy);
}

#[test]
fn tray_has_priority_over_a_walking_ant_and_cannot_start_a_selection_box() {
    let mut h = Harness::with_envoy();
    let at = h.tray_center();
    let id = super::layout_tests::id("working");
    let track = h.farm.tracks.get_mut(&id).unwrap();
    let mut walk = super::motion::Walk::new(&[at, at], FULL, h.time);
    walk.duration_s = 1000.0;
    track.walk = Some(walk);
    let response = h.click(at);
    assert!(response.open_envoy);
    assert!(response.clicked.is_none());
    assert!(!response.clicked_empty);
    let button = |pressed, pos| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![button(true, at)]);
    let end = at + vec2(80.0, 50.0);
    h.frame(vec![egui::Event::PointerMoved(end)]);
    let response = h.frame(vec![button(false, end)]).1;
    assert!(response.boxed.is_none());
}

#[test]
fn badge_counts_pending_replies_and_tasks_only() {
    use crate::envoy::drafts::{Draft, DraftQueue, ReplyTo};
    use crate::ui::test_util::text_center;
    let dir = tempfile::tempdir().unwrap();
    let mut queue = DraftQueue::load(dir.path().join("drafts.json")).unwrap();
    let reply = queue
        .create(Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1".into(),
            },
            None,
            "Reply".into(),
            "Why".into(),
            0,
        ))
        .unwrap();
    queue
        .create(Draft::proposal(
            "Task".into(),
            None,
            "Why".into(),
            String::new(),
            0,
        ))
        .unwrap();
    let mut h = Harness::with_envoy();
    assert_eq!(queue.pending_count(), 2);
    h.farm.set_envoy_pending(queue.pending_count());
    h.frame(vec![]);
    assert!(text_center(h.output.as_ref().unwrap(), "2").is_some());
    queue.reject(&reply.id, 1).unwrap();
    assert_eq!(queue.pending_count(), 1);
    h.farm.set_envoy_pending(queue.pending_count());
    h.frame(vec![]);
    assert!(text_center(h.output.as_ref().unwrap(), "1").is_some());
    h.farm.set_envoy_pending(0);
    h.frame(vec![]);
    assert!(text_center(h.output.as_ref().unwrap(), "0").is_none());
}
