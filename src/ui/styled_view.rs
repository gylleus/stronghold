//! Draws [`term_style`] lines: hanging-indent wrapping, row backgrounds, rules drawn as lines,
//! stick-to-bottom scrolling. Styling and layout are cached until the lines, width, font size
//! or theme change, so an idle frame only paints the visible rows.

use std::hash::Hash;
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, FontId, Galley, Painter, Pos2, Rect, Sense, Stroke, Vec2,
    text::{LayoutJob, TextFormat},
};

use crate::ui::term_style::{self, AgentKind, LineKind, Palette, Span, StyledLine};

/// Styled lines for one input, restyled only when the input changes, plus their last layout.
#[derive(Default)]
pub struct StyledCache {
    lines: Vec<String>,
    agent: Option<AgentKind>,
    styled: Vec<StyledLine>,
    restyles: u64,
    layout: Option<Layout>,
}

impl StyledCache {
    /// Restyles when `lines` or `agent` differ from the cached input; returns whether it did.
    pub fn update(&mut self, lines: &[String], agent: AgentKind) -> bool {
        if self.agent == Some(agent) && self.lines == lines {
            return false;
        }
        self.lines = lines.to_vec();
        self.agent = Some(agent);
        self.styled = term_style::style_lines(lines, agent);
        self.restyles += 1;
        self.layout = None;
        true
    }

    pub fn styled(&self) -> &[StyledLine] {
        &self.styled
    }

    /// How many times the lines were styled.
    pub fn restyles(&self) -> u64 {
        self.restyles
    }

    /// The layout for `key`, rebuilt only when the key or the lines changed.
    fn layout(&mut self, painter: &Painter, palette: &Palette, key: LayoutKey) -> &Layout {
        if self.layout.as_ref().is_none_or(|l| l.key != key) {
            self.layout = Some(build_layout(painter, palette, &self.styled, key));
        }
        self.layout.as_ref().expect("just built")
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutKey {
    width: f32,
    font_size: f32,
    dark: bool,
}

struct Layout {
    key: LayoutKey,
    rows: Vec<Row>,
    size: Vec2,
}

struct Row {
    y: f32,
    height: f32,
    kind: LineKind,
    prefix: Option<Arc<Galley>>,
    body: Option<Arc<Galley>>,
    body_x: f32,
}

/// Draws `lines` (styled as `agent`) in a stick-to-bottom scroll area. The cache lives in egui
/// memory under `id_salt`.
pub fn styled_view(
    ui: &mut egui::Ui,
    id_salt: impl Hash + std::fmt::Debug,
    lines: &[String],
    agent: AgentKind,
    font_size: f32,
) {
    let id = ui.make_persistent_id(id_salt);
    let cache = ui.data_mut(|d| {
        d.get_temp_mut_or_default::<Arc<Mutex<StyledCache>>>(id)
            .clone()
    });
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    cache.update(lines, agent);
    let palette = Palette::new(ui.visuals());
    egui::ScrollArea::both()
        .id_salt(id)
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show_viewport(ui, |ui, viewport| {
            let key = LayoutKey {
                width: viewport.width().round(),
                font_size,
                dark: palette.dark,
            };
            let layout = cache.layout(ui.painter(), &palette, key);
            let size = Vec2::new(layout.size.x.max(key.width), layout.size.y);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            paint_rows(ui, layout, &palette, rect, viewport);
        });
}

fn paint_rows(ui: &mut egui::Ui, layout: &Layout, palette: &Palette, rect: Rect, viewport: Rect) {
    let full_width = rect.width();
    let first = layout
        .rows
        .partition_point(|r| r.y + r.height < viewport.min.y);
    for row in layout.rows[first..]
        .iter()
        .take_while(|r| r.y <= viewport.max.y)
    {
        let top = rect.min + Vec2::new(0.0, row.y);
        let row_rect = Rect::from_min_size(top, Vec2::new(full_width, row.height));
        if let Some(bg) = palette.row_bg(row.kind) {
            ui.painter().rect_filled(row_rect, 2.0, bg);
        }
        if row.kind == LineKind::Rule {
            paint_rule(ui.painter(), palette, row, row_rect);
            continue;
        }
        if let Some(prefix) = &row.prefix {
            ui.painter().galley(top, prefix.clone(), palette.text);
        }
        if let Some(body) = &row.body {
            let at = top + Vec2::new(row.body_x, 0.0);
            ui.put(
                Rect::from_min_size(at, body.size()),
                egui::Label::new(body.clone()).selectable(true),
            );
        }
    }
}

fn paint_rule(painter: &Painter, palette: &Palette, row: &Row, rect: Rect) {
    let y = rect.center().y;
    let stroke = Stroke::new(1.0, palette.faint.gamma_multiply(0.7));
    let Some(label) = &row.body else {
        painter.hline(rect.x_range(), y, stroke);
        return;
    };
    let inset = 18.0;
    let pad = 6.0;
    painter.hline(rect.min.x..=rect.min.x + inset - pad, y, stroke);
    let at = Pos2::new(rect.min.x + inset, rect.min.y);
    painter.galley(at, label.clone(), palette.faint);
    let after = at.x + label.size().x + pad;
    if after < rect.max.x {
        painter.hline(after..=rect.max.x, y, stroke);
    }
}

fn build_layout(
    painter: &Painter,
    palette: &Palette,
    styled: &[StyledLine],
    key: LayoutKey,
) -> Layout {
    let mono = FontId::monospace(key.font_size);
    let prose = FontId::proportional(key.font_size + 0.5);
    let row_height = painter.fonts_mut(|f| f.row_height(&mono));
    let gap = (key.font_size * 0.45).round();
    let mut rows = Vec::with_capacity(styled.len());
    let mut y = 0.0;
    let mut width = key.width;
    for (i, line) in styled.iter().enumerate() {
        if i > 0 && line.starts_block() {
            y += gap;
        }
        if line.kind == LineKind::Rule {
            let body = (!line.body.is_empty()).then(|| {
                painter.layout_job(job(&line.body, line, palette, &mono, &prose, f32::INFINITY))
            });
            let height = body.as_ref().map_or(row_height * 0.8, |g| g.size().y);
            rows.push(Row {
                y,
                height,
                kind: line.kind,
                prefix: None,
                body,
                body_x: 0.0,
            });
            y += height;
            continue;
        }
        let prefix = (!line.prefix.is_empty()).then(|| {
            painter.layout_job(job(
                &line.prefix,
                line,
                palette,
                &mono,
                &mono,
                f32::INFINITY,
            ))
        });
        let body_x = prefix.as_ref().map_or(0.0, |g| g.size().x);
        let wrap = if line.kind.wraps() {
            (key.width - body_x).max(key.font_size * 12.0)
        } else {
            f32::INFINITY
        };
        let body = (!line.body.is_empty())
            .then(|| painter.layout_job(job(&line.body, line, palette, &mono, &prose, wrap)));
        let height = [
            row_height,
            prefix.as_ref().map_or(0.0, |g| g.size().y),
            body.as_ref().map_or(0.0, |g| g.size().y),
        ]
        .into_iter()
        .fold(0.0, f32::max);
        width = width.max(body_x + body.as_ref().map_or(0.0, |g| g.size().x));
        rows.push(Row {
            y,
            height,
            kind: line.kind,
            prefix,
            body,
            body_x,
        });
        y += height;
    }
    Layout {
        key,
        rows,
        size: Vec2::new(width, y),
    }
}

/// A layout job for `spans` of `line`, wrapped at `wrap` points.
fn job(
    spans: &[Span],
    line: &StyledLine,
    palette: &Palette,
    mono: &FontId,
    prose: &FontId,
    wrap: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    for span in spans {
        let font_id = if line.prose && !span.style.monospace() {
            prose.clone()
        } else {
            mono.clone()
        };
        job.append(
            &span.text,
            0.0,
            TextFormat {
                font_id,
                color: palette.color(span.style),
                background: palette.span_bg(span.style, line.kind),
                italics: span.style.italics(),
                ..Default::default()
            },
        );
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn cache_restyles_only_on_new_lines() {
        let mut cache = StyledCache::default();
        let a = lines("⏺ Hello\n  ⎿  Read 3 lines");
        assert!(cache.update(&a, AgentKind::Claude));
        let first = cache.styled().to_vec();
        assert!(!cache.update(&a.clone(), AgentKind::Claude));
        assert_eq!(cache.restyles(), 1);
        assert_eq!(cache.styled(), &first[..]);

        let b = lines("⏺ Hello\n  ⎿  Read 4 lines");
        assert!(cache.update(&b, AgentKind::Claude));
        assert_eq!(cache.restyles(), 2);
        assert_ne!(cache.styled(), &first[..]);
        // A different agent kind restyles the same lines.
        assert!(cache.update(&b, AgentKind::Unknown));
        assert_eq!(cache.restyles(), 3);
    }

    const DIFF: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/screens/claude_update_diff_synthetic.txt"
    ));

    struct Frame {
        texts: Vec<String>,
        /// Row count of each text's galley.
        rows: Vec<usize>,
        fills: Vec<egui::Color32>,
        restyles: u64,
    }

    fn render(ctx: &egui::Context, lines: &[String], frames: usize) -> Frame {
        let mut last = None;
        for i in 0..frames {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 900.0))),
                time: Some(i as f64 / 60.0),
                ..Default::default()
            };
            let mut restyles = 0;
            let mut output = ctx.run_ui(input, |ui| {
                styled_view(ui, "t", lines, AgentKind::Claude, 12.0);
                let id = ui.make_persistent_id("t");
                restyles = ui
                    .data(|d| d.get_temp::<Arc<Mutex<StyledCache>>>(id))
                    .map_or(0, |c| c.lock().unwrap().restyles());
            });
            output.textures_delta.clear();
            let mut frame = Frame {
                texts: Vec::new(),
                rows: Vec::new(),
                fills: Vec::new(),
                restyles,
            };
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut frame);
            }
            last = Some(frame);
        }
        last.expect("at least one frame")
    }

    fn collect(shape: &egui::Shape, frame: &mut Frame) {
        match shape {
            egui::Shape::Text(t) => {
                frame.texts.push(t.galley.text().to_string());
                frame.rows.push(t.galley.rows.len());
            }
            egui::Shape::Rect(r) => frame.fills.push(r.fill),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, frame)),
            _ => {}
        }
    }

    #[test]
    fn renders_headless_in_dark_and_light() {
        let lines = lines(DIFF);
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals.clone());
            let palette = Palette::new(&visuals);
            let frame = render(&ctx, &lines, 3);
            assert!(
                frame
                    .texts
                    .iter()
                    .any(|t| t == "Update(src/ui/terminal.rs)")
            );
            assert!(frame.texts.iter().any(|t| t == "⏺ "));
            assert!(frame.texts.iter().any(|t| t.starts_with("+      let rows")));
            // Diff rows get their backgrounds.
            assert!(frame.fills.contains(&palette.add_bg));
            assert!(frame.fills.contains(&palette.del_bg));
            assert!(frame.fills.contains(&palette.input_bg));
            // Three frames of the same screen are styled once.
            assert_eq!(frame.restyles, 1);
        }
    }

    #[test]
    fn new_screen_restyles_and_long_prose_wraps() {
        let ctx = egui::Context::default();
        let mut screen = lines("⏺ Read(src/lib.rs)\n  ⎿  Read 3 lines");
        assert_eq!(render(&ctx, &screen, 2).restyles, 1);
        screen.push(format!("⏺ {}", "word ".repeat(200)));
        let frame = render(&ctx, &screen, 2);
        assert_eq!(frame.restyles, 2);
        // The long line is one galley wrapped over several rows, not scrolled sideways.
        let at = frame
            .texts
            .iter()
            .position(|t| t.starts_with("word word"))
            .expect("long line drawn");
        assert!(frame.rows[at] > 3, "{} rows", frame.rows[at]);
    }
}
