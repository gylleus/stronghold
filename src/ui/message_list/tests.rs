use eframe::egui::{self, Event, Pos2, Rect, Vec2};

use super::*;

const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(500.0, 400.0));

/// A list of rows of fixed heights drawn headlessly, frame by frame.
struct Harness {
    ctx: egui::Context,
    list: MessageList,
    heights: Vec<f32>,
    /// What rows are estimated at before they are drawn.
    guess: f32,
    time: f64,
    /// Screen top of each row drawn in the last frame.
    drawn_at: Vec<(usize, f32)>,
    /// Rows drawn visibly (not just measured) in the last frame.
    output: ListOutput,
    full: Option<egui::FullOutput>,
}

impl Harness {
    fn new(rows: usize, height: f32) -> Self {
        Self {
            ctx: egui::Context::default(),
            list: MessageList::default(),
            heights: vec![height; rows],
            guess: height,
            time: 0.0,
            drawn_at: Vec::new(),
            output: ListOutput::default(),
            full: None,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> ListOutput {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(SCREEN),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let heights = self.heights.clone();
        let guess = self.guess;
        let list = &mut self.list;
        let mut drawn_at = Vec::new();
        let mut out = ListOutput::default();
        let mut full = self.ctx.run_ui(input, |ui| {
            out = list.show(ui, "list", heights.len(), &|_, _| guess, &mut |ui, i| {
                if ui.is_visible() {
                    drawn_at.push((i, ui.max_rect().top()));
                }
                ui.allocate_space(Vec2::new(10.0, heights[i]));
            });
        });
        full.textures_delta.clear();
        self.full = Some(full);
        self.drawn_at = drawn_at;
        self.output = out;
        out
    }

    fn frames(&mut self, n: usize) -> ListOutput {
        for _ in 0..n {
            self.frame(Vec::new());
        }
        self.output
    }

    /// Scrolls the wheel by `lines` over the list (positive: up) and lets it settle.
    fn wheel(&mut self, points: f32) -> ListOutput {
        self.frame(vec![
            Event::PointerMoved(SCREEN.center()),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, points),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            },
        ]);
        self.frames(40)
    }

    fn push(&mut self, rows: usize) {
        let h = self.heights[0];
        self.heights.extend(std::iter::repeat_n(h, rows));
    }

    /// Screen top of row `i` in the last frame.
    fn top_of(&self, i: usize) -> Option<f32> {
        self.drawn_at.iter().find(|(r, _)| *r == i).map(|(_, y)| *y)
    }

    fn first_drawn(&self) -> usize {
        self.drawn_at.iter().map(|(i, _)| *i).min().unwrap()
    }
}

#[test]
fn starts_at_the_bottom_and_follows_new_rows() {
    let mut h = Harness::new(100, 30.0);
    let out = h.frames(3);
    assert!(out.following);
    assert!(out.max_offset > 0.0);
    assert!((out.offset - out.max_offset).abs() < 1.0, "{out:?}");
    assert!(h.top_of(99).is_some(), "the newest row is in view");

    h.push(20);
    let out = h.frames(2);
    assert!(out.following);
    assert!((out.offset - out.max_offset).abs() < 1.0, "{out:?}");
    assert!(h.top_of(119).is_some(), "new rows scroll into view");
}

#[test]
fn scrolled_up_stays_put_while_rows_stream_in() {
    let mut h = Harness::new(100, 30.0);
    h.frames(3);
    let out = h.wheel(600.0);
    assert!(!out.following, "{out:?}");
    assert!(out.offset < out.max_offset - 100.0, "{out:?}");
    let offset = out.offset;
    let first = h.first_drawn();
    let first_top = h.top_of(first).unwrap();

    for _ in 0..10 {
        h.push(3);
        h.frames(2);
    }
    let out = h.output;
    assert!(!out.following);
    assert_eq!(out.offset, offset, "the view does not move");
    assert_eq!(h.top_of(first), Some(first_top));
    assert_eq!(h.list.unseen(), 30);
}

#[test]
fn jump_to_latest_button_resumes_following() {
    let mut h = Harness::new(100, 30.0);
    h.frames(3);
    h.wheel(600.0);
    h.push(5);
    h.frames(1);
    let label = format!("↓ {JUMP_LABEL} · 5 new");
    let at = crate::ui::test_util::text_center(h.full.as_ref().unwrap(), &label)
        .unwrap_or_else(|| panic!("{label} not drawn"));
    let click = |pressed| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![Event::PointerMoved(at)]);
    h.frame(vec![click(true)]);
    h.frame(vec![click(false)]);
    let out = h.frames(2);
    assert!(out.following, "{out:?}");
    assert!((out.offset - out.max_offset).abs() < 1.0, "{out:?}");
    assert!(h.top_of(104).is_some());
    assert!(
        crate::ui::test_util::text_center(h.full.as_ref().unwrap(), &format!("↓ {JUMP_LABEL}"))
            .is_none(),
        "the button hides while following"
    );
}

#[test]
fn scrolling_back_to_the_bottom_resumes_following() {
    let mut h = Harness::new(100, 30.0);
    h.frames(3);
    assert!(!h.wheel(300.0).following);
    let out = h.wheel(-2000.0);
    assert!(out.following, "{out:?}");
    h.push(4);
    let out = h.frames(2);
    assert!((out.offset - out.max_offset).abs() < 1.0, "{out:?}");
}

#[test]
fn a_long_history_draws_only_the_rows_in_view() {
    let mut h = Harness::new(20_000, 30.0);
    let out = h.frames(3);
    // 400 px of 40 px rows (30 + gap) is 10 rows; allow partial rows at both ends.
    assert!(out.drawn <= 12, "{out:?}");
    assert!(h.top_of(19_999).is_some());
    assert!(h.top_of(0).is_none());
}

/// Rows estimated too small get measured just before they scroll in, and the offset is
/// corrected so the rows in view do not jump.
#[test]
fn measuring_rows_above_keeps_the_view_steady() {
    let mut h = Harness::new(5_000, 60.0);
    h.guess = 20.0;
    h.frames(3);
    h.wheel(900.0);
    for _ in 0..5 {
        let first = h.first_drawn();
        let top = h.top_of(first).unwrap();
        h.frames(1);
        assert_eq!(h.top_of(first), Some(top), "row {first} stayed put");
    }
    assert!(!h.output.following);
}

#[test]
fn an_empty_list_is_fine_and_a_shorter_one_resets() {
    let mut h = Harness::new(0, 30.0);
    let out = h.frames(2);
    assert_eq!(out.drawn, 0);
    assert!(out.following);
    h.heights = vec![30.0; 50];
    h.frames(2);
    h.wheel(300.0);
    assert!(!h.list.following());
    h.heights.truncate(5);
    let out = h.frames(2);
    assert!(out.following, "a new conversation starts at the bottom");
}
