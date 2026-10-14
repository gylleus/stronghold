use super::*;
use crate::ui::test_util::text_center;
use crate::worktrees::{PlanEntry, Verdict};

struct Harness {
    panel: WorktreePanel,
    time: f64,
    size: Vec2,
    workspace_clicks: usize,
}

impl Harness {
    fn new(stage: Stage) -> Self {
        let mut panel = WorktreePanel::new(&egui::Context::default());
        panel.stage = Some(stage);
        panel.open = true;
        Self {
            panel,
            time: 0.0,
            size: vec2(1100.0, 720.0),
            workspace_clicks: 0,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 0.1;
        let ctx = self.panel.ctx.clone();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| {
                self.panel.handle_escape(ui.ctx());
                assert!(crate::ui::keys::take_commands(ui.ctx()).is_empty());
                egui::Panel::top("top_bar").show(ui, |ui| self.panel.top_bar(ui, &[]));
                self.panel.show(ui);
                egui::CentralPanel::default().show(ui, |ui| {
                    if ui.button("Keep working").clicked() {
                        self.workspace_clicks += 1;
                    }
                });
            },
        );
        output.textures_delta.clear();
        output
    }

    fn click(&mut self, label: &str) {
        self.frame(vec![]);
        let at =
            text_center(&self.frame(vec![]), label).unwrap_or_else(|| panic!("missing {label}"));
        assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size).contains(at));
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
    }

    fn escape(&mut self) {
        self.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
    }

    fn reopen(&mut self, menu: &str) {
        self.click(menu);
        self.click("Show cleanup…");
        assert!(self.panel.open);
    }
}

fn plan() -> Plan {
    Plan {
        entries: vec![PlanEntry {
            id: "repo::branch".into(),
            repo: "repo".into(),
            name: "branch".into(),
            bytes: Some(1024),
            verdict: Verdict {
                remove: true,
                reasons: vec!["Merged into main; notes are saved before removal. ".repeat(20)],
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn running() -> (Stage, crossbeam_channel::Sender<ExecMsg>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (
        Stage::Running {
            rx,
            queue: vec!["repo::branch".into()],
            total: 1,
            done: vec![],
        },
        tx,
    )
}

#[test]
fn every_stage_can_close_and_reopen_while_the_workspace_stays_usable() {
    let (_evaluation_tx, evaluation_rx) = crossbeam_channel::bounded(1);
    let (running, _removal_tx) = running();
    for stage in [
        Stage::Evaluating(evaluation_rx),
        Stage::Failed("offline".into()),
        Stage::Review {
            plan: plan(),
            chosen: BTreeSet::from(["repo::branch".into()]),
        },
        running,
        Stage::Finished {
            removals: vec![],
            error: None,
        },
    ] {
        let menu = match &stage {
            Stage::Evaluating(_) => "0 worktrees · checking…",
            Stage::Running { .. } => "0 worktrees · clearing 0/1",
            _ => "0 worktrees",
        };
        let mut h = Harness::new(stage);
        h.click("Keep working");
        assert_eq!(h.workspace_clicks, 1);
        h.click("Close");
        assert!(!h.panel.open);
        assert!(h.panel.stage.is_some());
        h.reopen(menu);
        h.escape();
        assert!(!h.panel.open);
        assert!(text_center(&h.frame(vec![]), "Clear worktrees").is_none());
        h.reopen(menu);
    }
}

#[test]
fn review_selection_survives_dismissal_and_approval_stays_in_the_viewport() {
    let mut h = Harness::new(Stage::Failed(String::new()));
    h.panel.show_plan(plan());
    h.size = vec2(640.0, 400.0);
    h.click("repo/branch");
    h.click("Cancel");
    h.reopen("0 worktrees");
    h.click("Clear 0 worktrees (0 B)");
    assert!(matches!(&h.panel.stage, Some(Stage::Review { chosen, .. }) if chosen.is_empty()));
    h.click("repo/branch");
    assert!(matches!(&h.panel.stage, Some(Stage::Review { chosen, .. }) if chosen.len() == 1));
    let output = h.frame(vec![]);
    let approve = text_center(&output, "Clear 1 worktree (1.0 KB)").unwrap();
    assert!(approve.x < h.size.x && approve.y < h.size.y);
}

#[test]
fn evaluation_finishes_hidden_and_still_requires_review() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let mut h = Harness::new(Stage::Evaluating(rx));
    h.click("Close");
    tx.send(Ok(plan())).unwrap();
    assert!(!h.panel.poll_stage());
    assert!(!h.panel.open);
    h.reopen("0 worktrees");
    assert!(matches!(&h.panel.stage, Some(Stage::Review { chosen, .. }) if chosen.len() == 1));
}

#[test]
fn hidden_removal_keeps_progress_and_completion_without_reopening() {
    let (stage, tx) = running();
    let mut h = Harness::new(stage);
    h.escape();
    let removal = Removal {
        id: "repo::branch".into(),
        repo: "repo".into(),
        name: "branch".into(),
        path: "/unused".into(),
        branch: None,
        bytes: Some(1024),
        reason: "merged".into(),
        notes_saved_to: None,
        archive_path: None,
        outcome: Outcome::Removed,
        at_ms: 1,
    };
    tx.send(ExecMsg::Progress(Box::new(removal.clone())))
        .unwrap();
    assert!(!h.panel.poll_stage());
    assert_eq!(h.panel.progress(), Some((1, 1)));
    h.reopen("0 worktrees · clearing 1/1");
    h.click("Close");
    tx.send(ExecMsg::Done(Ok(vec![removal.clone()]))).unwrap();
    assert!(h.panel.poll_stage(), "completion requests a clan refresh");
    assert!(!h.panel.open);
    assert!(h.panel.progress().is_none());
    assert!(h.panel.last_summary.is_some());
    h.reopen("0 worktrees");
    assert!(
        matches!(&h.panel.stage, Some(Stage::Finished { removals, error: None }) if removals == &[removal])
    );
}

#[test]
fn archive_action_requires_a_separate_confirmation() {
    let mut plan = plan();
    plan.entries[0].path = "/unused/worktree".into();
    plan.entries[0].verdict = Verdict {
        archivable: true,
        reasons: vec!["unmerged work".into()],
        ..Default::default()
    };
    let mut h = Harness::new(Stage::Review {
        plan,
        chosen: BTreeSet::new(),
    });
    h.click("Keep (1)");
    h.click("Archive & remove");
    assert!(
        matches!(&h.panel.stage, Some(Stage::ArchiveConfirm { id, .. }) if id == "repo::branch")
    );
    assert!(text_center(&h.frame(vec![]), "Confirm archive & remove").is_some());
    h.escape();
    assert!(!h.panel.open);
    assert!(matches!(&h.panel.stage, Some(Stage::ArchiveConfirm { .. })));
}
