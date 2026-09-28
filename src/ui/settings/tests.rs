use std::path::PathBuf;

use eframe::egui;

use super::whip::{HOUR, MAX_ABANDON_HOURS, validate_draft};
use super::*;
use crate::ui::test_util::{click_label, text_center};

const SEC: i64 = 1_000;

fn general() -> GeneralInfo {
    GeneralInfo {
        data_dir: PathBuf::from("/data/myrstack"),
        socket_path: PathBuf::from("/data/myrstack/control.sock"),
        socket_listening: true,
        version: "0.1.0",
        commit: Some("abc1234"),
        playbook_dir: Ok(PathBuf::from("/pb")),
        playbook_version: Some("1b0b685 (clean) · hash 3f9a1c2".into()),
    }
}

fn cx<'a>(
    status: &'a WhipStatus,
    general: &'a GeneralInfo,
    queen: QueenStatus<'a>,
) -> PaneContext<'a> {
    PaneContext {
        whip_status: status,
        now_ms: 0,
        queen,
        general,
    }
}

/// Draws the pane a few frames headlessly; returns which of `labels` the last frame drew.
fn drawn(pane: &mut SettingsPane, cx: &PaneContext<'_>, labels: &[&str]) -> Vec<String> {
    let ctx = egui::Context::default();
    let mut last = None;
    for i in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            time: Some(f64::from(i) / 60.0),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            pane.show(ui.ctx(), cx);
        });
        output.textures_delta.clear();
        last = Some(output);
    }
    let output = last.unwrap();
    labels
        .iter()
        .filter(|l| text_center(&output, l).is_some())
        .map(|l| (*l).to_owned())
        .collect()
}

#[test]
fn each_section_renders_headless() {
    let status = WhipStatus::default();
    let info = general();
    let queen = QueenStatus {
        auto_mode: true,
        running_model: Some("claude-fable-5"),
        env_model: None,
    };
    let cases: [(Section, &[&str]); 4] = [
        (
            Section::Queen,
            &[
                "Model",
                "Effort",
                queen::CLI_DEFAULT,
                "claude-fable-5",
                "Save and restart the Queen",
                "Speak replies",
            ],
        ),
        (
            Section::Whip,
            &[
                "Dry run (log only, send nothing)",
                "Stall threshold",
                "Whip all stalled now",
            ],
        ),
        (Section::Voice, &[voice::PLACEHOLDER]),
        (
            Section::General,
            &[
                "/data/myrstack",
                "/data/myrstack/control.sock",
                "● listening",
                "0.1.0 (abc1234)",
                "/pb",
                "1b0b685 (clean) · hash 3f9a1c2",
                "README",
            ],
        ),
    ];
    for (section, labels) in cases {
        let mut pane = SettingsPane::load(None);
        pane.open_section(section);
        let c = cx(&status, &info, queen);
        let mut want: Vec<&str> = labels.to_vec();
        want.extend(Section::ALL.iter().map(|s| s.title()));
        let got = drawn(&mut pane, &c, &want);
        let missing: Vec<&&str> = want
            .iter()
            .filter(|l| !got.iter().any(|g| g == *l))
            .collect();
        assert!(missing.is_empty(), "{section:?}: missing {missing:?}");
    }
}

#[test]
fn queen_section_shows_the_env_override() {
    let status = WhipStatus::default();
    let info = general();
    let mut pane = SettingsPane::load(None);
    pane.open_section(Section::Queen);
    let queen = QueenStatus {
        auto_mode: false,
        running_model: None,
        env_model: Some("haiku"),
    };
    let text = "Overridden by env: MYRSTACK_CHAT_MODEL=haiku. The Queen runs haiku; the model \
                above applies once the variable is unset.";
    assert_eq!(
        drawn(&mut pane, &cx(&status, &info, queen), &[text]),
        vec![text]
    );
}

#[test]
fn old_whip_entry_points_open_the_whip_section() {
    let mut pane = SettingsPane::load(None);
    assert!(!pane.open);
    pane.open_window();
    assert!(pane.open);
    assert_eq!(pane.section, Section::Whip);

    // The top bar's "Whip…" button.
    let mut pane = SettingsPane::load(None);
    click_label("Whip…", |ui| pane.button(ui));
    assert!(pane.open);
    assert_eq!(pane.section, Section::Whip);

    // The gear opens the last section shown (Queen at first).
    let mut pane = SettingsPane::load(None);
    click_label("⚙", |ui| pane.gear_button(ui));
    assert!(pane.open);
    assert_eq!(pane.section, Section::Queen);
    pane.open = false;
    pane.section = Section::General;
    pane.open_settings();
    assert_eq!(pane.section, Section::General);
}

#[test]
fn whip_all_stalled_now_is_taken_once() {
    let mut pane = SettingsPane::load(None);
    pane.open_window();
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Whip all stalled now", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, QueenStatus::default()))
    });
    assert!(out.whip_stalled);
    assert!(pane.take_whip_stalled());
    assert!(!pane.take_whip_stalled());
}

#[test]
fn save_writes_the_file_and_applies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    assert!(pane.whip().dry_run);
    assert_eq!(pane.message, None);

    pane.whip_draft.dry_run = false;
    pane.whip_draft.threshold_ms = 60 * SEC;
    let saved = pane.save_whip().unwrap();
    assert!(!saved.dry_run);
    assert_eq!(pane.whip(), &saved);
    assert_eq!(Settings::load_from(&path).settings.whip, saved);
}

#[test]
fn queen_save_keeps_the_other_sections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.whip_draft.dry_run = false;
    pane.save_whip().unwrap();

    pane.queen_draft.pick(Some("opus"));
    pane.queen_draft.settings.effort = Some("high".into());
    let queen = pane.save_queen().unwrap();
    assert_eq!(queen.model.as_deref(), Some("opus"));
    let on_disk = Settings::load_from(&path).settings;
    assert_eq!(on_disk.queen, queen);
    assert!(!on_disk.whip.dry_run, "whip section kept");

    // A custom model is trimmed; a blank one means the CLI default.
    pane.queen_draft.pick_custom("  claude-opus-4-8 ");
    assert_eq!(
        pane.save_queen().unwrap().model.as_deref(),
        Some("claude-opus-4-8")
    );
    assert!(
        pane.queen_draft.custom,
        "an unlisted model reopens as custom"
    );
    pane.queen_draft.pick_custom("   ");
    assert_eq!(pane.save_queen().unwrap().model, None);

    // Invalid: nothing saved.
    pane.queen_draft.pick_custom("claude opus");
    assert_eq!(pane.save_queen(), None);
    assert!(pane.message.as_ref().unwrap().0);
    assert_eq!(Settings::load_from(&path).settings.queen.model, None);

    // Reloading keeps everything.
    let reloaded = SettingsPane::load(Some(path));
    assert_eq!(reloaded.queen(), pane.queen());
    assert_eq!(reloaded.whip(), pane.whip());
}

#[test]
fn clicking_save_reports_the_queen_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut pane = SettingsPane::load(Some(dir.path().join("settings.json")));
    pane.open_section(Section::Queen);
    pane.queen_draft.pick(Some("sonnet"));
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Save and restart the Queen", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, QueenStatus::default()))
    });
    assert_eq!(out.queen.unwrap().model.as_deref(), Some("sonnet"));
}

#[test]
fn abandon_window_validation() {
    let ok = WhipSettings::default();
    let cases: Vec<(&str, WhipSettings, bool)> = vec![
        ("defaults", ok.clone(), true),
        (
            "one hour",
            WhipSettings {
                abandon_after_ms: HOUR,
                ..ok.clone()
            },
            true,
        ),
        (
            "zero",
            WhipSettings {
                abandon_after_ms: 0,
                ..ok.clone()
            },
            false,
        ),
        (
            "not longer than the threshold",
            WhipSettings {
                abandon_after_ms: HOUR,
                threshold_ms: HOUR,
                ..ok.clone()
            },
            false,
        ),
        (
            "over 30 days",
            WhipSettings {
                abandon_after_ms: (MAX_ABANDON_HOURS + 1) * HOUR,
                ..ok.clone()
            },
            false,
        ),
    ];
    for (name, draft, valid) in cases {
        assert_eq!(validate_draft(&draft).is_ok(), valid, "case: {name}");
    }
}

#[test]
fn saved_abandon_window_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.whip_draft.abandon_after_ms = 6 * HOUR;
    assert_eq!(pane.save_whip().unwrap().abandon_after_ms, 6 * HOUR);
    assert_eq!(
        Settings::load_from(&path).settings.whip.abandon_after_ms,
        6 * HOUR
    );

    pane.whip_draft.abandon_after_ms = pane.whip_draft.threshold_ms;
    assert_eq!(pane.save_whip(), None);
    assert_eq!(pane.whip().abandon_after_ms, 6 * HOUR);
}

#[test]
fn invalid_draft_is_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.whip_draft.tick_ms = 0;
    assert_eq!(pane.save_whip(), None);
    assert!(pane.message.as_ref().unwrap().0);
    assert!(!path.exists());
    assert_eq!(pane.whip(), &WhipSettings::default());
}
