//! The review queue in the Queen panel ("Ready for review (N)") and its top-bar badge.

use eframe::egui::{self, Color32, RichText, Ui};

use crate::model::NodeId;
use crate::review::github::Ci;
use crate::review::view::ReviewRow;
use crate::ui::graph::pr_color;

/// What the user did with the queue this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// Open the PR in the browser.
    Open(String),
    Dismiss(String),
    Reviewed(String),
    /// Select the agent's node.
    Select(NodeId),
}

/// How the section is drawn this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionState {
    /// Force the section open (a row just arrived, or the badge or palette asked).
    pub open: bool,
    /// Scroll the section into view.
    pub scroll: bool,
}

/// The "Ready for review (N)" section; `github_note` explains a GitHub outage.
pub fn review_section(
    ui: &mut Ui,
    rows: &[ReviewRow],
    github_note: Option<&str>,
    now_ms: i64,
    state: SectionState,
) -> Option<ReviewAction> {
    let mut action = None;
    let title = RichText::new(format!("Ready for review ({})", rows.len())).strong();
    let response = egui::CollapsingHeader::new(title)
        .id_salt("queen_review")
        .default_open(!rows.is_empty())
        .open(state.open.then_some(true))
        .show(ui, |ui| {
            if let Some(note) = github_note {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "GitHub status unavailable",
                )
                .on_hover_text(note);
            }
            if rows.is_empty() {
                ui.weak("Nothing waiting. Agents call mark_pr_ready (or send merge_ready) when a PR is ready.");
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("queen_review_rows")
                .max_height(ui.ctx().content_rect().height() * 0.3)
                .show(ui, |ui| {
                    for row in rows {
                        if let Some(a) = review_row(ui, row, now_ms) {
                            action = Some(a);
                        }
                    }
                });
        });
    if state.scroll {
        response
            .header_response
            .scroll_to_me(Some(egui::Align::TOP));
    }
    action
}

fn review_row(ui: &mut Ui, row: &ReviewRow, now_ms: i64) -> Option<ReviewAction> {
    let mut action = None;
    let dark = ui.visuals().dark_mode;
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Dismiss")
                        .on_hover_text("Hide it until an agent marks it ready again")
                        .clicked()
                    {
                        action = Some(ReviewAction::Dismiss(row.url.clone()));
                    }
                    if ui
                        .small_button("Reviewed")
                        .on_hover_text("Hide it until it is pushed to or marked ready again")
                        .clicked()
                    {
                        action = Some(ReviewAction::Reviewed(row.url.clone()));
                    }
                    if ui
                        .small_button("Open")
                        .on_hover_text(format!("Open {} in the browser", row.url))
                        .clicked()
                    {
                        action = Some(ReviewAction::Open(row.url.clone()));
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&row.repo).weak().small());
                        let text = if row.title.is_empty() {
                            format!("#{}", row.number)
                        } else {
                            format!("#{} {}", row.number, row.title)
                        };
                        let link = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(text).color(pr_color(dark)).strong(),
                                )
                                .truncate()
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text(format!("{}\nclick to open in the browser", row.url));
                        if link.clicked() {
                            action = Some(ReviewAction::Open(row.url.clone()));
                        }
                    });
                });
            });
            ui.horizontal_wrapped(|ui| {
                let agent = ui.add(
                    egui::Label::new(RichText::new(row.agent_text()).small())
                        .sense(egui::Sense::click()),
                );
                match &row.agent {
                    Some(a) => {
                        if agent
                            .on_hover_text(format!("{}\nclick to select its node", a.id))
                            .clicked()
                        {
                            action = Some(ReviewAction::Select(a.id.clone()));
                        }
                    }
                    None => {
                        agent.on_hover_text("The marking terminal is not in the graph");
                    }
                }
                sep(ui);
                match (row.state, &row.error) {
                    (None, Some(e)) => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            RichText::new("GitHub error").small(),
                        )
                        .on_hover_text(e);
                    }
                    (None, None) => {
                        ui.label(RichText::new("reading GitHub…").small().weak());
                    }
                    (Some(_), _) => {
                        if let Some(note) = row.state_note() {
                            ui.label(RichText::new(note).small().strong());
                            sep(ui);
                        }
                        if let Some(ci) = row.ci {
                            ui.label(
                                RichText::new(format!("CI {}", ci.symbol()))
                                    .small()
                                    .color(ci_color(ci, ui)),
                            )
                            .on_hover_text(ci.describe());
                            sep(ui);
                        }
                        if let Some(text) = row.ankra_text() {
                            let stale = row.ankra_on_head == Some(false);
                            let label = RichText::new(text).small();
                            let label = if stale {
                                label.color(ui.visuals().warn_fg_color)
                            } else {
                                label
                            };
                            ui.label(label).on_hover_text(if stale {
                                "Ankra reviewed an older commit than the PR head"
                            } else {
                                "Ankra's verdict"
                            });
                            sep(ui);
                        }
                        if let Some(text) = row.marker_text() {
                            let stale = row.marker_on_head == Some(false);
                            let label = RichText::new(text).small();
                            let label = if stale {
                                label.color(ui.visuals().warn_fg_color)
                            } else {
                                label
                            };
                            ui.label(label).on_hover_text(if stale {
                                "The ready marker names an older commit than the PR head"
                            } else {
                                "The coordinator's ready marker (first line of the description)"
                            });
                            sep(ui);
                        }
                        if let Some(d) = &row.review_decision {
                            ui.label(RichText::new(d.to_lowercase().replace('_', " ")).small());
                            sep(ui);
                        }
                    }
                }
                if row.updated_since_review {
                    ui.label(
                        RichText::new("pushed since review")
                            .small()
                            .color(ui.visuals().warn_fg_color),
                    );
                    sep(ui);
                }
                ui.label(
                    RichText::new(crate::ui::table::format_age(now_ms - row.marked_ms))
                        .small()
                        .weak(),
                )
                .on_hover_text(format!("marked ready via {}", row.source));
            });
            if let Some(summary) = &row.summary {
                ui.add(egui::Label::new(RichText::new(summary).small().weak()).truncate())
                    .on_hover_text(summary);
            }
        });
    action
}

fn sep(ui: &mut Ui) {
    ui.label(RichText::new("·").small().weak());
}

fn ci_color(ci: Ci, ui: &Ui) -> Color32 {
    match ci {
        Ci::Passing { .. } => Color32::from_rgb(0x2d, 0xa4, 0x4e),
        Ci::Failing { .. } => ui.visuals().error_fg_color,
        Ci::Pending { .. } => ui.visuals().warn_fg_color,
        Ci::None => ui.visuals().weak_text_color(),
    }
}

/// The top bar's "Review N" badge (nothing when the queue is empty); true when clicked.
pub fn review_badge(ui: &mut Ui, count: usize) -> bool {
    if count == 0 {
        return false;
    }
    let dark = ui.visuals().dark_mode;
    let text = RichText::new(format!("Review {count}"))
        .color(Color32::WHITE)
        .strong();
    ui.add(egui::Button::new(text).fill(pr_color(dark)))
        .on_hover_text(format!(
            "{count} PR{} ready for review: open the review queue in the Queen panel",
            if count == 1 { "" } else { "s" }
        ))
        .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::github::PrState;
    use crate::review::view::AgentRef;
    use crate::ui::test_util::click_label;

    fn row() -> ReviewRow {
        ReviewRow {
            url: "https://github.com/depict-org/realtime/pull/255".into(),
            repo: "depict-org/realtime".into(),
            number: 255,
            title: "recs: gate legacy config retirement".into(),
            summary: Some("green, threads closed".into()),
            agent: Some(AgentRef {
                id: NodeId::terminal("term_a"),
                label: "worker-a".into(),
                live: true,
            }),
            agent_handle: None,
            source: "tool",
            marked_ms: 0,
            state: Some(PrState::Open),
            ci: Some(Ci::Passing { total: 9 }),
            head_sha: Some("92b1c94".into()),
            ankra_verdict: Some("comments".into()),
            ankra_on_head: Some(false),
            marker: Some("ready"),
            marker_on_head: Some(false),
            review_decision: None,
            error: None,
            updated_since_review: false,
        }
    }

    fn draw(ui: &mut Ui, rows: &[ReviewRow]) -> Option<ReviewAction> {
        review_section(
            ui,
            rows,
            None,
            3_600_000,
            SectionState {
                open: true,
                scroll: false,
            },
        )
    }

    #[test]
    fn row_actions() {
        let rows = [row()];
        let url = rows[0].url.clone();
        assert_eq!(
            click_label("#255 recs: gate legacy config retirement", |ui| draw(
                ui, &rows
            )),
            Some(ReviewAction::Open(url.clone()))
        );
        assert_eq!(
            click_label("Dismiss", |ui| draw(ui, &rows)),
            Some(ReviewAction::Dismiss(url.clone()))
        );
        assert_eq!(
            click_label("Reviewed", |ui| draw(ui, &rows)),
            Some(ReviewAction::Reviewed(url.clone()))
        );
        assert_eq!(
            click_label("worker-a", |ui| draw(ui, &rows)),
            Some(ReviewAction::Select(NodeId::terminal("term_a")))
        );
    }

    #[test]
    fn draws_headless_in_every_state() {
        let mut unknown = row();
        unknown.agent = None;
        unknown.state = None;
        unknown.ci = None;
        let mut errored = unknown.clone();
        errored.error = Some("not found".into());
        let mut merged = row();
        merged.state = Some(PrState::Merged);
        merged.updated_since_review = true;
        let rows = [row(), unknown, errored, merged];
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(
                    review_section(ui, &rows, Some("gh auth login"), 0, SectionState::default()),
                    None
                );
                assert_eq!(
                    review_section(ui, &[], None, 0, SectionState::default()),
                    None
                );
                assert!(!review_badge(ui, 0));
                assert!(!review_badge(ui, 2));
            });
            out.textures_delta.clear();
        }
        assert!(click_label("Review 2", |ui| review_badge(ui, 2)));
    }

    #[test]
    fn graph_marks_pr_nodes() {
        use crate::fleet::FleetData;
        use crate::model::FleetSnapshot;
        use crate::orca::Terminal;
        use crate::ui::graph::GraphView;
        let data = FleetData {
            terminals: vec![Terminal {
                handle: "term_a".into(),
                ..Terminal::default()
            }],
            ..FleetData::default()
        };
        let snapshot = FleetSnapshot::build(&data, 0);
        let mut graph = GraphView::new();
        graph.sync(&snapshot);
        let id = NodeId::terminal("term_a");
        let pr = |g: &GraphView| {
            let idx = g.node_index(&id).unwrap();
            g.graph().node(idx).unwrap().payload().pr_ready
        };
        assert!(!pr(&graph));
        graph.set_pr_ready([id.clone()].into());
        assert!(pr(&graph));
        graph.sync(&snapshot);
        assert!(pr(&graph), "kept across syncs");
        graph.set_pr_ready(Default::default());
        assert!(!pr(&graph));
    }
}
