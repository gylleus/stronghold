//! CommonMark blocks and inline styling shared by King and agent conversations.

use super::code_block;
use crate::model::NodeId;
use eframe::egui::{self, RichText, Ui};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag};

/// Whether `s` is an http(s) url with something after the scheme.
pub fn is_web_url(s: &str) -> bool {
    ["http://", "https://"]
        .iter()
        .any(|p| s.len() > p.len() && s.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()))
}

/// The length of a bare http(s) url starting at `text[i..]`, with trailing punctuation
/// (and unbalanced closing brackets) trimmed off.
fn url_len(text: &str, i: usize) -> Option<usize> {
    if !text.as_bytes()[i].eq_ignore_ascii_case(&b'h') {
        return None;
    }
    let boundary = text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric());
    if !boundary {
        return None;
    }
    let rest = &text[i..];
    let len = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
        .unwrap_or(rest.len());
    let url = trim_url(&rest[..len]);
    is_web_url(url).then_some(url.len())
}

/// Trims trailing punctuation off a bare url; `)` and `]` only when unbalanced.
pub fn trim_url(url: &str) -> &str {
    let mut url = url;
    while let Some(c) = url.chars().next_back() {
        let strip = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => url.matches('(').count() < url.matches(')').count(),
            ']' => url.matches('[').count() < url.matches(']').count(),
            _ => false,
        };
        if !strip {
            break;
        }
        url = &url[..url.len() - c.len_utf8()];
    }
    url
}

/// Pushes inline code, cutting out any urls in it as code-styled links.
fn push_code(spans: &mut Vec<Span>, code: &str) {
    let mut start = 0;
    let mut i = 0;
    while i < code.len() {
        if let Some(len) = url_len(code, i) {
            if start < i {
                spans.push(Span::Code(code[start..i].to_owned()));
            }
            let url = &code[i..i + len];
            spans.push(Span::Link {
                text: url.to_owned(),
                url: url.to_owned(),
                code: true,
            });
            i += len;
            start = i;
            continue;
        }
        i += code[i..].chars().next().map_or(1, char::len_utf8);
    }
    if start < code.len() || code.is_empty() {
        spans.push(Span::Code(code[start..].to_owned()));
    }
}

fn looks_like_node(s: &str) -> bool {
    ["term:", "sub:", "dispatch:"]
        .iter()
        .any(|p| s.starts_with(p) && s.len() > p.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
}

/// Pushes `text` as plain (or bold) spans, cutting out node ids and web urls.
pub(super) fn push_plain(
    spans: &mut Vec<Span>,
    text: &str,
    is_node: &dyn Fn(&NodeId) -> bool,
    bold: bool,
) {
    let wrap = |s: &str| {
        if bold {
            Span::Bold(s.to_owned())
        } else {
            Span::Plain(s.to_owned())
        }
    };
    let mut start = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < text.len() {
        if let Some(len) = url_len(text, i) {
            if start < i {
                spans.push(wrap(&text[start..i]));
            }
            let url = &text[i..i + len];
            spans.push(Span::Link {
                text: url.to_owned(),
                url: url.to_owned(),
                code: false,
            });
            i += len;
            start = i;
            continue;
        }
        let at_boundary = i == 0 || !is_id_char(bytes[i - 1] as char);
        if at_boundary
            && ["term:", "sub:", "dispatch:"]
                .iter()
                .any(|p| text[i..].starts_with(p))
        {
            let len = text[i..]
                .find(|c: char| !is_id_char(c))
                .unwrap_or(text.len() - i);
            let candidate = text[i..i + len].trim_end_matches(['.', ':', '-']);
            let id = NodeId(candidate.to_owned());
            if looks_like_node(candidate) && is_node(&id) {
                if start < i {
                    spans.push(wrap(&text[start..i]));
                }
                spans.push(Span::Node(id));
                i += candidate.len();
                start = i;
                continue;
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    if start < text.len() {
        spans.push(wrap(&text[start..]));
    }
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.')
}

/// Inline text, retaining Stronghold's clickable agent ids and bare web links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Plain(String),
    Bold(String),
    Code(String),
    Node(NodeId),
    Link {
        text: String,
        url: String,
        code: bool,
    },
    Styled {
        span: Box<Span>,
        bold: bool,
        italic: bool,
        strike: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    Quote(Vec<Block>),
    List(Vec<(String, Vec<Block>)>),
    Code(String),
    Table(Vec<Alignment>, Vec<Vec<Vec<Span>>>),
    Rule,
}

/// An event tree keeps container nesting, including loose lists and quoted code. Inline
/// events are interpreted in their original document so reference links remain resolved.
struct Element<'a> {
    event: Event<'a>,
    children: Vec<Element<'a>>,
}

fn elements<'a>(events: &mut impl Iterator<Item = Event<'a>>) -> Vec<Element<'a>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        if matches!(event, Event::End(_)) {
            break;
        }
        let children = if matches!(event, Event::Start(_)) {
            elements(events)
        } else {
            vec![]
        };
        out.push(Element { event, children });
    }
    out
}

fn document(text: &str) -> Vec<Element<'_>> {
    elements(&mut Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ))
}

#[cfg(test)]
pub fn parse_blocks(text: &str) -> Vec<Block> {
    blocks(&document(text), &|_| false)
}

fn blocks(nodes: &[Element<'_>], is_node: &dyn Fn(&NodeId) -> bool) -> Vec<Block> {
    let mut out = Vec::new();
    let mut inline = Vec::new();
    for node in nodes {
        let block = match &node.event {
            Event::Start(Tag::Paragraph) => {
                Some(Block::Paragraph(inlines(&node.children, is_node)))
            }
            Event::Start(Tag::Heading { level, .. }) => Some(Block::Heading(
                *level as u8,
                inlines(&node.children, is_node),
            )),
            Event::Start(Tag::BlockQuote(_)) => Some(Block::Quote(blocks(&node.children, is_node))),
            Event::Start(Tag::List(start)) => Some(Block::List(
                node.children
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let marker = start.map_or_else(
                            || "•".into(),
                            |n| format!("{}.", n.saturating_add(i as u64)),
                        );
                        (marker, blocks(&item.children, is_node))
                    })
                    .collect(),
            )),
            Event::Start(Tag::CodeBlock(_)) => Some(Block::Code(
                literal(&node.children).trim_end_matches('\n').into(),
            )),
            Event::Start(Tag::HtmlBlock) => {
                Some(Block::Paragraph(vec![Span::Plain(literal(&node.children))]))
            }
            Event::Start(Tag::Table(alignments)) => Some(Block::Table(
                alignments.clone(),
                node.children
                    .iter()
                    .map(|row| {
                        row.children
                            .iter()
                            .map(|cell| inlines(&cell.children, is_node))
                            .collect()
                    })
                    .collect(),
            )),
            Event::Rule => Some(Block::Rule),
            _ => None,
        };
        if let Some(block) = block {
            if !inline.is_empty() {
                out.push(Block::Paragraph(inlines(&inline, is_node)));
                inline.clear();
            }
            out.push(block);
        } else {
            inline.push(node);
        }
    }
    if !inline.is_empty() {
        out.push(Block::Paragraph(inlines(&inline, is_node)));
    }
    out
}

fn literal(nodes: &[Element<'_>]) -> String {
    let mut text = String::new();
    for node in nodes {
        match &node.event {
            Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => {
                text.push_str(t)
            }
            Event::SoftBreak | Event::HardBreak => text.push('\n'),
            _ => text.push_str(&literal(&node.children)),
        }
    }
    text
}

#[cfg(test)]
pub fn parse_spans(text: &str, is_node: &dyn Fn(&NodeId) -> bool) -> Vec<Span> {
    inlines(&document(text), is_node)
}

fn inlines<'a>(nodes: &[impl AsRef<Element<'a>>], is_node: &dyn Fn(&NodeId) -> bool) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut text_run = String::new();
    for node in nodes {
        let node = node.as_ref();
        if let Event::Text(text) = &node.event {
            text_run.push_str(text);
            continue;
        }
        push_plain(&mut spans, &text_run, is_node, false);
        text_run.clear();
        match &node.event {
            Event::Html(text) | Event::InlineHtml(text) => {
                spans.push(Span::Plain(text.to_string()))
            }
            Event::Code(code) => {
                let id = NodeId(code.to_string());
                if looks_like_node(code) && is_node(&id) {
                    spans.push(Span::Node(id));
                } else {
                    push_code(&mut spans, code);
                }
            }
            Event::Start(Tag::Strong | Tag::Emphasis | Tag::Strikethrough) => {
                let bold = matches!(node.event, Event::Start(Tag::Strong));
                let italic = matches!(node.event, Event::Start(Tag::Emphasis));
                let strike = matches!(node.event, Event::Start(Tag::Strikethrough));
                spans.extend(
                    inlines(&node.children, is_node)
                        .into_iter()
                        .map(|span| match span {
                            Span::Plain(text) if bold => Span::Bold(text),
                            span => Span::Styled {
                                span: Box::new(span),
                                bold,
                                italic,
                                strike,
                            },
                        }),
                );
            }
            Event::Start(Tag::Link { dest_url, .. }) if is_web_url(dest_url) => {
                for span in inlines(&node.children, &|_| false) {
                    spans.push(link_span(span, dest_url));
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                // Unsupported schemes stay readable and inert.
                spans.push(Span::Plain(format!(
                    "[{}]({dest_url})",
                    literal(&node.children)
                )));
            }
            Event::SoftBreak => spans.push(Span::Plain(" ".into())),
            Event::HardBreak => spans.push(Span::Plain("\n".into())),
            Event::TaskListMarker(done) => {
                spans.push(Span::Plain(if *done { "☑ " } else { "☐ " }.into()))
            }
            _ => spans.extend(inlines(&node.children, is_node)),
        }
    }
    push_plain(&mut spans, &text_run, is_node, false);
    // The parser may split literal markers/entities across events; keep adjacent text as
    // one widget so words, URLs and node ids still wrap and link as a unit.
    let mut merged = Vec::new();
    for span in spans {
        match (merged.last_mut(), &span) {
            (Some(Span::Plain(a)), Span::Plain(b)) => a.push_str(b),
            _ => merged.push(span),
        }
    }
    merged
}

impl<'a> AsRef<Element<'a>> for Element<'a> {
    fn as_ref(&self) -> &Self {
        self
    }
}

fn link_span(span: Span, url: &str) -> Span {
    match span {
        Span::Styled {
            span,
            bold,
            italic,
            strike,
        } => Span::Styled {
            span: Box::new(link_span(*span, url)),
            bold,
            italic,
            strike,
        },
        Span::Bold(text) => Span::Styled {
            span: Box::new(Span::Link {
                text,
                url: url.into(),
                code: false,
            }),
            bold: true,
            italic: false,
            strike: false,
        },
        Span::Plain(text) | Span::Link { text, .. } => Span::Link {
            text,
            url: url.into(),
            code: false,
        },
        Span::Code(text) => Span::Link {
            text,
            url: url.into(),
            code: true,
        },
        Span::Node(id) => Span::Link {
            text: id.0,
            url: url.into(),
            code: true,
        },
    }
}

pub fn markdown(ui: &mut Ui, text: &str, is_node: &dyn Fn(&NodeId) -> bool) -> Option<NodeId> {
    let mut clicked = None;
    let blocks = blocks(&document(text), is_node);
    render_blocks(ui, &blocks, &mut clicked);
    clicked
}

fn render_blocks(ui: &mut Ui, blocks: &[Block], clicked: &mut Option<NodeId>) {
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            ui.add_space(4.0);
        }
        // Equal tables/code/quotes in the same message need distinct widget identities.
        ui.push_id(index, |ui| match block {
            Block::Heading(level, spans) => {
                let size = match level {
                    1 => 22.0,
                    2 => 19.0,
                    3 => 17.0,
                    _ => 15.0,
                };
                spans_ui(ui, spans, Some(size), clicked);
            }
            Block::Paragraph(spans) => spans_ui(ui, spans, None, clicked),
            Block::Quote(children) => {
                let color = ui.visuals().widgets.noninteractive.bg_stroke.color;
                let frame = egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 4,
                        top: 4,
                        bottom: 4,
                    })
                    .show(ui, |ui| render_blocks(ui, children, clicked));
                let rect = frame.response.rect;
                ui.painter().line_segment(
                    [rect.left_top(), rect.left_bottom()],
                    egui::Stroke::new(3.0, color),
                );
            }
            Block::List(items) => {
                for (i, (marker, children)) in items.iter().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.add_space(6.0);
                            ui.label(marker);
                            ui.vertical(|ui| {
                                ui.set_max_width(ui.available_width());
                                render_blocks(ui, children, clicked);
                            });
                        });
                    });
                }
            }
            Block::Code(code) => code_block(ui, code),
            Block::Table(alignments, rows) => table(ui, alignments, rows, clicked),
            Block::Rule => {
                ui.separator();
            }
        });
    }
}

fn table(
    ui: &mut Ui,
    alignments: &[Alignment],
    rows: &[Vec<Vec<Span>>],
    clicked: &mut Option<NodeId>,
) {
    egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            let columns = alignments.len().max(1);
            let gap = 14.0;
            let width =
                ((ui.available_width() - gap * (columns - 1) as f32) / columns as f32).max(100.0);
            egui::ScrollArea::horizontal()
                .id_salt("table_scroll")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (r, row) in rows.iter().enumerate() {
                        let fill = if r % 2 == 1 {
                            ui.visuals().faint_bg_color
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        egui::Frame::new().fill(fill).show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for (c, cell) in row.iter().enumerate() {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(width, ui.spacing().interact_size.y),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.set_width(width);
                                            let align = match alignments.get(c) {
                                                Some(Alignment::Center) => egui::Align::Center,
                                                Some(Alignment::Right) => egui::Align::Max,
                                                _ => egui::Align::Min,
                                            };
                                            spans_aligned(
                                                ui,
                                                cell,
                                                (r == 0).then_some(14.0),
                                                align,
                                                clicked,
                                            );
                                        },
                                    );
                                }
                            });
                        });
                    }
                });
        });
}

fn spans_ui(ui: &mut Ui, spans: &[Span], heading: Option<f32>, clicked: &mut Option<NodeId>) {
    spans_aligned(ui, spans, heading, egui::Align::Min, clicked);
}

fn spans_aligned(
    ui: &mut Ui,
    spans: &[Span],
    heading: Option<f32>,
    align: egui::Align,
    clicked: &mut Option<NodeId>,
) {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center)
            .with_main_wrap(true)
            .with_main_align(align),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            for span in spans {
                span_ui(ui, span, heading, false, false, false, clicked);
            }
        },
    );
}

fn span_ui(
    ui: &mut Ui,
    span: &Span,
    heading: Option<f32>,
    bold: bool,
    italic: bool,
    strike: bool,
    clicked: &mut Option<NodeId>,
) {
    if let Span::Styled {
        span,
        bold: b,
        italic: i,
        strike: s,
    } = span
    {
        return span_ui(
            ui,
            span,
            heading,
            bold || *b,
            italic || *i,
            strike || *s,
            clicked,
        );
    }
    let (text, code) = match span {
        Span::Plain(s) | Span::Bold(s) => (s.as_str(), false),
        Span::Code(s) => (s.as_str(), true),
        Span::Link { text, code, .. } => (text.as_str(), *code),
        Span::Node(id) => (id.as_str(), true),
        Span::Styled { .. } => unreachable!(),
    };
    let leading = text.chars().take_while(|c| *c == ' ').count();
    if leading > 0 {
        let font = egui::FontId::proportional(
            heading.unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()).size),
        );
        let space = ui.fonts_mut(|fonts| fonts.glyph_width(&font, ' '));
        ui.add_space(space * leading as f32);
    }
    let mut rich = RichText::new(text.trim_start_matches(' '));
    if let Some(size) = heading {
        rich = rich.size(size).strong();
    }
    if bold || matches!(span, Span::Bold(_)) {
        rich = rich.strong();
    }
    if italic {
        rich = rich.italics();
    }
    if strike {
        rich = rich.strikethrough();
    }
    if code {
        rich = rich.code();
    }
    match span {
        Span::Node(id) => {
            if ui.link(rich).on_hover_text("Select this node").clicked() {
                *clicked = Some(id.clone());
            }
        }
        Span::Link { url, .. } => {
            ui.add(egui::Hyperlink::from_label_and_url(rich, url))
                .on_hover_text(url);
        }
        _ => {
            ui.add(egui::Label::new(rich).wrap());
        }
    }
}

/// Shared by markdown and literal user messages.
pub(super) fn link_ui(ui: &mut Ui, text: RichText, url: &str) {
    ui.add(egui::Hyperlink::from_label_and_url(text, url))
        .on_hover_text(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_quotes_lists_and_loose_continuations_keep_their_structure() {
        let blocks = parse_blocks(
            "> first\n>\n> > nested\n>\n> 3. parent\n>    continuation\n>    - child\n>\n>    second paragraph",
        );
        let Block::Quote(quote) = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!(
            quote[0],
            Block::Paragraph(vec![Span::Plain("first".into())])
        );
        assert!(matches!(&quote[1], Block::Quote(inner) if inner.len() == 1));
        let Block::List(items) = &quote[2] else {
            panic!("{quote:?}")
        };
        assert_eq!(items[0].0, "3.");
        assert_eq!(
            items[0].1[0],
            Block::Paragraph(vec![Span::Plain("parent continuation".into())])
        );
        assert!(matches!(&items[0].1[1], Block::List(children) if children[0].0 == "•"));
        assert_eq!(
            items[0].1[2],
            Block::Paragraph(vec![Span::Plain("second paragraph".into())])
        );
    }

    #[test]
    fn tables_require_delimiters_keep_escaped_pipes_and_alignment() {
        let blocks = parse_blocks("Left | Right\n:--- | ---:\n`a\\|b` | **value**");
        let Block::Table(align, rows) = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!(align, &[Alignment::Left, Alignment::Right]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], vec![Span::Code("a|b".into())]);
        assert_eq!(rows[1][1], vec![Span::Bold("value".into())]);
        assert!(matches!(
            &parse_blocks("| ordinary prose |\n\nNext")[0],
            Block::Paragraph(_)
        ));
    }

    #[test]
    fn fences_headings_rules_and_hard_breaks_follow_commonmark() {
        let blocks = parse_blocks(
            "Title\n=====\n\n###### Small\n\n#not-heading\n\n* * *\n\n~~~~rust\n> literal\n```\n~~~~\n\n    indented\n\nline  \nbreak",
        );
        assert!(matches!(&blocks[0], Block::Heading(1, _)));
        assert!(matches!(&blocks[1], Block::Heading(6, _)));
        assert_eq!(
            blocks[2],
            Block::Paragraph(vec![Span::Plain("#not-heading".into())])
        );
        assert_eq!(blocks[3], Block::Rule);
        assert_eq!(blocks[4], Block::Code("> literal\n```".into()));
        assert_eq!(blocks[5], Block::Code("indented".into()));
        assert_eq!(
            blocks[6],
            Block::Paragraph(vec![Span::Plain("line\nbreak".into())])
        );
        assert_eq!(
            parse_blocks("```rust\nunclosed"),
            vec![Block::Code("unclosed".into())]
        );
    }

    #[test]
    fn emphasis_code_escapes_and_reference_links_survive_parsing() {
        let spans = parse_spans(
            "***both*** __bold__ _italic_ ``a ` b`` \\*literal\\*",
            &|_| false,
        );
        assert!(
            matches!(&spans[0], Span::Styled { italic: true, span, .. } if matches!(span.as_ref(), Span::Bold(text) if text == "both"))
        );
        assert!(spans.contains(&Span::Bold("bold".into())));
        assert!(spans.contains(&Span::Code("a ` b".into())));
        assert!(matches!(spans.last(), Some(Span::Plain(text)) if text.ends_with("*literal*")));
        let blocks = parse_blocks("[**docs**][d]\n\n[d]: https://example.com/a_(b)");
        assert!(
            matches!(&blocks[0], Block::Paragraph(spans) if matches!(&spans[0], Span::Styled { bold: true, span, .. } if matches!(span.as_ref(), Span::Link {url, ..} if url == "https://example.com/a_(b)")))
        );
    }

    #[test]
    fn table_headers_are_visible_and_do_not_overlap_wrapped_body_rows() {
        let ctx = egui::Context::default();
        let text = "| Header | Result |\n| --- | --- |\n| Body | a very long cell that needs several lines to fit |\n| End | done |";
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    markdown(ui, text, &|_| false);
                },
            );
            let bounds = |label: &str| {
                output.shapes.iter().find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.text() == label
                    {
                        let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                        assert!(
                            shape.clip_rect.expand(1.0).contains_rect(rect),
                            "clipped {label}: {rect:?} in {:?}",
                            shape.clip_rect
                        );
                        Some(rect)
                    } else {
                        None
                    }
                })
            };
            let header = bounds("Header").unwrap();
            let body = bounds("Body").unwrap();
            let wrapped = bounds("a very long cell that needs several lines to fit").unwrap();
            let end = bounds("End").unwrap();
            assert!(header.bottom() <= body.top());
            assert!(wrapped.bottom() <= end.top());
            output.textures_delta.clear();
        }
    }

    #[test]
    fn narrow_chat_keeps_long_text_code_quotes_and_lists_inside_the_pane() {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        let text = format!(
            "# Heading\n\n> {}\n\n- **bold** {}\n  - `nested`\n\n```\n{}\n```",
            "longword".repeat(30),
            "longword".repeat(30),
            "code".repeat(60)
        );
        let mut right = 0.0_f32;
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 1600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    markdown(ui, &text, &|_| false);
                    right = ui.min_rect().right();
                },
            );
            output.textures_delta.clear();
        }
        assert!(right <= 301.0, "content expanded to {right}");
    }
}
