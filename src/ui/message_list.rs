//! A scrolling list of variable-height messages that draws only the rows in view, so a long
//! conversation stays fast without being capped. It follows new messages while scrolled to
//! the bottom; once the user scrolls up it stays put as messages stream in, and a "Jump to
//! latest" button (or scrolling back down) resumes following.
//!
//! Heights of rows never drawn are estimated. Rows just above the view are measured before
//! they scroll in, and the scroll offset is corrected by any difference, so the visible rows
//! do not jump.

use std::hash::Hash;

use eframe::egui::{self, Align, Layout, Rect, Ui, UiBuilder, Vec2};

/// Space between two rows.
pub const ROW_GAP: f32 = 10.0;
/// Space below the last row.
const BOTTOM_PAD: f32 = 12.0;
/// Rows off-screen whose height is measured per frame, so the scrollbar converges.
const MEASURE_PER_FRAME: usize = 24;
/// How long a new row takes to fade in.
const FADE_IN_SECS: f64 = 0.2;
/// More rows than this arriving at once (history loading) show without fading in.
const FADE_MAX_NEW: usize = 3;
/// Within this distance of the bottom counts as at the bottom.
const BOTTOM_SLOP: f32 = 2.0;

/// Scroll and height state of one list, kept across frames.
#[derive(Debug)]
pub struct MessageList {
    /// Measured height of each row (`None`: not drawn yet).
    heights: Vec<Option<f32>>,
    /// When each row appeared (`Input::time`), for the fade-in.
    appear: Vec<f64>,
    /// Scrolled to the bottom: new rows scroll into view.
    follow: bool,
    /// Rows added since the user scrolled up.
    unseen: usize,
    /// Scroll offset at the end of the last frame.
    offset: f32,
    /// Correction for rows above the view that were measured at a height other than estimated.
    shift: f32,
}

impl Default for MessageList {
    fn default() -> Self {
        Self {
            heights: Vec::new(),
            appear: Vec::new(),
            follow: true,
            unseen: 0,
            offset: 0.0,
            shift: 0.0,
        }
    }
}

/// What happened in the list this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ListOutput {
    pub following: bool,
    /// Scroll offset from the top.
    pub offset: f32,
    /// Largest scroll offset (the bottom).
    pub max_offset: f32,
    /// Rows drawn this frame.
    pub drawn: usize,
}

impl MessageList {
    /// Whether new rows scroll into view.
    pub fn following(&self) -> bool {
        self.follow
    }

    /// Rows added since the user scrolled up.
    pub fn unseen(&self) -> usize {
        self.unseen
    }

    /// Scrolls to the bottom and follows new rows again.
    pub fn jump_to_latest(&mut self) {
        self.follow = true;
        self.unseen = 0;
    }

    /// Forgets all rows (a different conversation is shown).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Draws `len` rows with `draw` into the rest of `ui`. `estimate` guesses a row's height at
    /// a width before it is first drawn.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        id_salt: impl Hash + std::fmt::Debug,
        len: usize,
        estimate: &dyn Fn(usize, f32) -> f32,
        draw: &mut dyn FnMut(&mut Ui, usize),
    ) -> ListOutput {
        let now = ui.input(|i| i.time);
        if len < self.heights.len() {
            self.reset();
        }
        let added = len - self.heights.len();
        if !self.follow {
            self.unseen += added;
        }
        let fade_from = if added > FADE_MAX_NEW || self.heights.is_empty() {
            f64::NEG_INFINITY
        } else {
            now
        };
        self.heights.resize(len, None);
        self.appear.resize(len, fade_from);

        let width = ui.available_width();
        let view_height = ui.available_height();
        let tops = self.tops(width, estimate);
        let total = tops.last().copied().unwrap_or(0.0);
        let max_offset = (total - view_height).max(0.0);
        let set_offset = if self.follow {
            Some(max_offset)
        } else if self.shift != 0.0 {
            Some((self.offset + self.shift).clamp(0.0, max_offset))
        } else {
            None
        };
        self.shift = 0.0;

        let mut area = egui::ScrollArea::vertical()
            .id_salt(id_salt)
            .auto_shrink([false, false]);
        if let Some(offset) = set_offset {
            area = area.vertical_scroll_offset(offset);
        }
        let mut drawn = 0;
        let output = area.show_viewport(ui, |ui, viewport| {
            let origin = ui.max_rect().min;
            let rows = RowsPass {
                list: self,
                tops: &tops,
                origin,
                width,
                now,
                estimate,
            };
            drawn = rows.run(ui, viewport, draw);
            ui.advance_cursor_after_rect(Rect::from_min_size(origin, Vec2::new(width, total)));
        });

        let offset = output.state.offset.y;
        let max_after = (output.content_size.y - output.inner_rect.height()).max(0.0);
        let expected = set_offset.unwrap_or(self.offset).min(max_after);
        if offset < expected - 0.5 && offset < max_after - BOTTOM_SLOP {
            self.follow = false;
        } else if offset >= max_after - BOTTOM_SLOP && !self.follow {
            self.jump_to_latest();
        }
        self.offset = offset;

        if !self.follow && jump_button(ui, output.inner_rect, self.unseen) {
            self.jump_to_latest();
            ui.ctx().request_repaint();
        }
        ListOutput {
            following: self.follow,
            offset,
            max_offset: max_after,
            drawn,
        }
    }

    /// Top of each row, plus the total height as the last entry.
    fn tops(&self, width: f32, estimate: &dyn Fn(usize, f32) -> f32) -> Vec<f32> {
        let mut tops = Vec::with_capacity(self.heights.len() + 1);
        let mut y = 0.0;
        for (i, h) in self.heights.iter().enumerate() {
            tops.push(y);
            y += h.unwrap_or_else(|| estimate(i, width)) + ROW_GAP;
        }
        tops.push(if self.heights.is_empty() {
            0.0
        } else {
            y - ROW_GAP + BOTTOM_PAD
        });
        tops
    }

    fn height(&self, i: usize, width: f32, estimate: &dyn Fn(usize, f32) -> f32) -> f32 {
        self.heights[i].unwrap_or_else(|| estimate(i, width))
    }
}

/// One frame's drawing of the rows in and near the viewport.
struct RowsPass<'a> {
    list: &'a mut MessageList,
    tops: &'a [f32],
    origin: egui::Pos2,
    width: f32,
    now: f64,
    estimate: &'a dyn Fn(usize, f32) -> f32,
}

impl RowsPass<'_> {
    /// Draws the visible rows and measures some off-screen ones; returns how many were drawn.
    fn run(self, ui: &mut Ui, viewport: Rect, draw: &mut dyn FnMut(&mut Ui, usize)) -> usize {
        let len = self.list.heights.len();
        if len == 0 {
            return 0;
        }
        // The first row reaching into the view keeps its place; rows below follow it.
        let first = self.tops[1..=len]
            .partition_point(|&next| next - ROW_GAP <= viewport.min.y)
            .min(len - 1);
        let mut y = self.tops[first];
        let mut i = first;
        let mut drawn = 0;
        while i < len && y <= viewport.max.y {
            let rect = Rect::from_min_size(
                self.origin + Vec2::new(0.0, y),
                Vec2::new(self.width, f32::INFINITY),
            );
            let alpha = ((self.now - self.list.appear[i]) / FADE_IN_SECS).clamp(0.0, 1.0) as f32;
            if alpha < 1.0 {
                ui.ctx().request_repaint();
            }
            let height = row(ui, rect, i, false, alpha, draw);
            self.list.heights[i] = Some(height);
            y += height + ROW_GAP;
            i += 1;
            drawn += 1;
        }

        // Rows above the view, nearest first, then any others: measured off-screen. Rows above
        // shift the offset by their error so the view stays put.
        let mut budget = MEASURE_PER_FRAME;
        let above = (0..first).rev();
        let below = i..len;
        for j in above.chain(below) {
            if budget == 0 {
                break;
            }
            if self.list.heights[j].is_some() {
                continue;
            }
            budget -= 1;
            let guess = self.list.height(j, self.width, self.estimate);
            let far = Rect::from_min_size(
                self.origin + Vec2::new(0.0, self.tops[len] + viewport.height() * 4.0),
                Vec2::new(self.width, f32::INFINITY),
            );
            let height = row(ui, far, j, true, 1.0, draw);
            self.list.heights[j] = Some(height);
            if j < first {
                self.list.shift += height - guess;
            }
        }
        drawn
    }
}

/// Draws row `i` at `rect` and returns its height.
fn row(
    ui: &mut Ui,
    rect: Rect,
    i: usize,
    invisible: bool,
    alpha: f32,
    draw: &mut dyn FnMut(&mut Ui, usize),
) -> f32 {
    let mut builder = UiBuilder::new()
        .max_rect(rect)
        .layout(Layout::top_down(Align::Min))
        .id_salt(("row", i));
    if invisible {
        builder = builder.invisible();
    }
    let mut child = ui.new_child(builder);
    child.set_clip_rect(child.clip_rect().intersect(ui.clip_rect()));
    if alpha < 1.0 {
        child.multiply_opacity(alpha);
    }
    draw(&mut child, i);
    child.min_rect().height().max(0.0)
}

/// Text of the button that scrolls back down.
pub const JUMP_LABEL: &str = "Jump to latest";

/// The floating "Jump to latest" button at the bottom of `rect`; true when clicked.
fn jump_button(ui: &mut Ui, rect: Rect, unseen: usize) -> bool {
    let text = if unseen > 0 {
        format!("↓ {JUMP_LABEL} · {unseen} new")
    } else {
        format!("↓ {JUMP_LABEL}")
    };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_width = ui.ctx().fonts_mut(|f| {
        f.layout_no_wrap(text.clone(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    let size = Vec2::new(text_width + 28.0, 28.0);
    let at = Rect::from_center_size(
        egui::pos2(rect.center().x, rect.max.y - size.y / 2.0 - 10.0),
        size,
    );
    let visuals = ui.visuals().clone();
    let button = egui::Button::new(egui::RichText::new(text).color(visuals.selection.stroke.color))
        .fill(visuals.selection.bg_fill)
        .corner_radius(14.0);
    let response = ui
        .put(at, button)
        .on_hover_text("Scroll to the newest message and follow new ones (End)");
    let end = ui.input(|i| i.key_pressed(egui::Key::End)) && ui.rect_contains_pointer(rect);
    response.clicked() || end
}

#[cfg(test)]
mod tests;
