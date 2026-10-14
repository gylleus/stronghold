//! The review queue in the King panel ("Ready for review (N)") and its top-bar badge.

use eframe::egui::{self, Color32, RichText, Ui};

use crate::model::NodeId;
use crate::review::github::Ci;
use crate::review::view::ReviewRow;
use crate::ui::clan_view::pr_color;

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

/// Compact links for one agent, using exactly the rows behind the clan's PR badge.
/// Excess links live in a menu so they cannot displace the header's other controls.
pub fn agent_pr_buttons(ui: &mut Ui, node: &NodeId, rows: &[ReviewRow]) -> Option<ReviewAction> {
    let rows: Vec<_> = rows
        .iter()
        .filter(|row| row.agent.as_ref().is_some_and(|agent| &agent.id == node))
        .collect();
    let mut action = None;
    ui.push_id(("agent_prs", node), |ui| {
        for (index, row) in rows.iter().enumerate() {
            let label = pr_button_text(row);
            let font = egui::TextStyle::Button.resolve(ui.style());
            let width = ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(label.clone(), font, Color32::WHITE)
                    .size()
                    .x
            }) + ui.spacing().button_padding.x * 2.0;
            let remaining = rows.len() - index;
            let reserve = if remaining > 1 { 80.0 } else { 0.0 };
            if width + reserve > ui.available_width() {
                ui.menu_button(format!("+{remaining} PRs"), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for row in &rows[index..] {
                                if pr_button(ui, row, &pr_button_text(row)) {
                                    action = Some(ReviewAction::Open(row.url.clone()));
                                    ui.close();
                                }
                            }
                        });
                });
                break;
            }
            if pr_button(ui, row, &label) {
                action = Some(ReviewAction::Open(row.url.clone()));
            }
        }
    });
    action
}

fn pr_button_text(row: &ReviewRow) -> String {
    let repo = row.repo.rsplit('/').next().unwrap_or(&row.repo);
    let cue = row.state_note().or_else(|| row.ci.map(Ci::symbol));
    match cue {
        Some(cue) => format!("{repo}#{} {cue}", row.number),
        None => format!("{repo}#{}", row.number),
    }
}

fn pr_button(ui: &mut Ui, row: &ReviewRow, text: &str) -> bool {
    let mut tooltip = format!(
        "{}\n{}\n{}",
        row.title,
        row.url,
        row.state.map_or("Reading GitHub…", |state| state.name())
    );
    if let Some(ci) = row.ci {
        tooltip.push_str(&format!("\n{}", ci.describe()));
    }
    if let Some(marker) = row.marker_text() {
        tooltip.push_str(&format!("\n{marker}"));
    }
    if let Some(error) = &row.error {
        tooltip.push_str(&format!("\n{error}"));
    }
    ui.button(
        RichText::new(text)
            .small()
            .color(pr_color(ui.visuals().dark_mode)),
    )
    .on_hover_text(tooltip)
    .clicked()
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
        .id_salt("king_review")
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
                ui.horizontal_top(|ui| {
                    ui.add(crate::ui::art::portrait(ui.ctx(), crate::ui::art::Portrait::Still, 28.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new("Nothing waiting. Coordinators and standalone agents call mark_pr_ready when a PR is ready.").weak(),
                        )
                        .wrap(),
                    );
                });
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("king_review_rows")
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
            // Title and buttons on their own lines, so a narrow column cannot overlap them.
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.repo).weak().small());
                let text = if row.title.is_empty() {
                    format!("#{}", row.number)
                } else {
                    format!("#{} {}", row.number, row.title)
                };
                let link = ui
                    .add(
                        egui::Label::new(RichText::new(text).color(pr_color(dark)).strong())
                            .truncate()
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text(format!("{}\nclick to open in the browser", row.url));
                if link.clicked() {
                    action = Some(ReviewAction::Open(row.url.clone()));
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .small_button("Open")
                    .on_hover_text(format!("Open {} in the browser", row.url))
                    .clicked()
                {
                    action = Some(ReviewAction::Open(row.url.clone()));
                }
                if ui
                    .small_button("Reviewed")
                    .on_hover_text("Hide it until it is pushed to or marked ready again")
                    .clicked()
                {
                    action = Some(ReviewAction::Reviewed(row.url.clone()));
                }
                if ui
                    .small_button("Dismiss")
                    .on_hover_text("Hide it until an agent marks it ready again")
                    .clicked()
                {
                    action = Some(ReviewAction::Dismiss(row.url.clone()));
                }
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
                        agent.on_hover_text("The marking terminal is not in the clan");
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
                    RichText::new(crate::ui::clan_view::format_age(now_ms - row.marked_ms))
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
            "{count} PR{} ready for review: open the review queue in the King panel",
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
            url: "https://github.com/acme/api/pull/255".into(),
            repo: "acme/api".into(),
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
    fn agent_buttons_open_each_pr_and_only_show_the_selected_agents_rows() {
        let first = row();
        let mut second = row();
        second.number = 256;
        second.url = "https://github.com/acme/api/pull/256".into();
        second.ci = Some(Ci::Pending {
            pending: 1,
            total: 9,
        });
        let rows = [first, second];
        for row in &rows {
            assert_eq!(
                click_label(&pr_button_text(row), |ui| {
                    ui.horizontal(|ui| agent_pr_buttons(ui, &NodeId::terminal("term_a"), &rows))
                        .inner
                }),
                Some(ReviewAction::Open(row.url.clone()))
            );
        }
        let ctx = egui::Context::default();
        // Reuse the context as selection and queue contents change: no stale links.
        for (node, rows, expected) in [
            (NodeId::terminal("term_a"), &rows[..1], true),
            (NodeId::terminal("term_b"), &rows[..], false),
            (NodeId::terminal("term_a"), &rows[..], true),
            (NodeId::terminal("term_a"), &[][..], false),
        ] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.horizontal(|ui| agent_pr_buttons(ui, &node, rows));
            });
            output.textures_delta.clear();
            assert_eq!(
                crate::ui::test_util::text_center(&output, "api#255 ✓").is_some(),
                expected
            );
        }
    }

    #[test]
    fn agent_button_overflow_keeps_every_pr_clickable() {
        use egui::{Event, PointerButton, Pos2, Rect};
        let rows = [row(), {
            let mut r = row();
            r.number = 256;
            r.url.push_str("/files");
            r
        }];
        let ctx = egui::Context::default();
        let mut action = None;
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_max_width(100.0);
                    action = ui
                        .horizontal(|ui| agent_pr_buttons(ui, &NodeId::terminal("term_a"), &rows))
                        .inner;
                },
            );
            output.textures_delta.clear();
            output
        };
        for label in ["+2 PRs", "api#256 ✓"] {
            let output = frame(vec![]);
            let at = crate::ui::test_util::text_center(&output, label).expect(label);
            frame(vec![Event::PointerMoved(at)]);
            for pressed in [true, false] {
                frame(vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
        }
        assert_eq!(action, Some(ReviewAction::Open(rows[1].url.clone())));
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
    fn clan_view_marks_pr_nodes() {
        use crate::clan::ClanData;
        use crate::model::ClanSnapshot;
        use crate::orca::Terminal;
        use crate::ui::clan_view::ClanView;
        let data = ClanData {
            terminals: vec![Terminal {
                handle: "term_a".into(),
                ..Terminal::default()
            }],
            ..ClanData::default()
        };
        let snapshot = ClanSnapshot::build(&data, 0);
        let mut clan = ClanView::new();
        clan.sync(&snapshot);
        let id = NodeId::terminal("term_a");
        let pr = |g: &ClanView| g.pr_ready().contains(&id);
        assert!(!pr(&clan));
        clan.set_pr_ready([id.clone()].into());
        assert!(pr(&clan));
        clan.sync(&snapshot);
        assert!(pr(&clan), "kept across syncs");
        clan.set_pr_ready(Default::default());
        assert!(!pr(&clan));
    }
}
