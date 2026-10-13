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
    king: KingStatus<'a>,
) -> PaneContext<'a> {
    PaneContext {
        whip_status: status,
        now_ms: 0,
        king,
        general,
        voice: VoiceStatus::default(),
        notify: NotifyStatus::default(),
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
    let king = KingStatus {
        auto_mode: true,
        running_model: Some("claude-fable-5"),
        env_model: None,
    };
    let cases: [(Section, &[&str]); 7] = [
        (
            Section::King,
            &[
                "Model",
                "Effort",
                king::CLI_DEFAULT,
                "claude-fable-5",
                "Save and restart the King",
                "Speak replies",
            ],
        ),
        (
            Section::Envoy,
            &[
                "Agent",
                "Model",
                "Poll interval (seconds)",
                "Channels (IDs, one per line)",
                "Save Envoy settings",
                "Policies and dry run ON apply immediately when saved. Other changes apply on the next Start.",
            ],
        ),
        (
            Section::Whip,
            &[
                "Dry run (log nudges, send none)",
                "Stall threshold",
                "Whip all stalled now",
            ],
        ),
        (
            Section::Notifications,
            &[
                "Quiet mode (do not disturb)",
                "macOS notifications",
                "Pop-ups in the Myrstack window",
                "Play sounds from",
                "Tink",
                "Glass",
                "Sosumi",
                "Volume",
            ],
        ),
        (
            Section::Voice,
            &[
                "Talk to the King",
                "⌘⇧E",
                "Speak all of the King's replies",
                "▶ Test voice",
                "Speech recognition (Parakeet)",
            ],
        ),
        (
            Section::Art,
            &[art::PIXELS_LABEL, art::PALETTE_LABEL, "Own colours"],
        ),
        (
            Section::General,
            &[
                general::FOCUS_NEW_LABEL,
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
        let c = cx(&status, &info, king);
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
fn king_section_shows_the_env_override() {
    let status = WhipStatus::default();
    let info = general();
    let mut pane = SettingsPane::load(None);
    pane.open_section(Section::King);
    let king = KingStatus {
        auto_mode: false,
        running_model: None,
        env_model: Some("haiku"),
    };
    let text = "Overridden by env: MYRSTACK_CHAT_MODEL=haiku. The King runs haiku; the model \
                above applies once the variable is unset.";
    assert_eq!(
        drawn(&mut pane, &cx(&status, &info, king), &[text]),
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

    // The gear opens the last section shown (King at first).
    let mut pane = SettingsPane::load(None);
    click_label("⚙", |ui| pane.gear_button(ui));
    assert!(pane.open);
    assert_eq!(pane.section, Section::King);
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
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
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
fn king_save_keeps_the_other_sections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.whip_draft.dry_run = false;
    pane.save_whip().unwrap();

    pane.king_draft.pick(Some("opus"));
    pane.king_draft.settings.effort = Some("high".into());
    let king = pane.save_king().unwrap();
    assert_eq!(king.model.as_deref(), Some("opus"));
    let on_disk = Settings::load_from(&path).settings;
    assert_eq!(on_disk.king, king);
    assert!(!on_disk.whip.dry_run, "whip section kept");

    // A custom model is trimmed; a blank one means the CLI default.
    pane.king_draft.pick_custom("  claude-opus-4-8 ");
    assert_eq!(
        pane.save_king().unwrap().model.as_deref(),
        Some("claude-opus-4-8")
    );
    assert!(
        pane.king_draft.custom,
        "an unlisted model reopens as custom"
    );
    pane.king_draft.pick_custom("   ");
    assert_eq!(pane.save_king().unwrap().model, None);

    // Invalid: nothing saved.
    pane.king_draft.pick_custom("claude opus");
    assert_eq!(pane.save_king(), None);
    assert!(pane.message.as_ref().unwrap().0);
    assert_eq!(Settings::load_from(&path).settings.king.model, None);

    // Reloading keeps everything.
    let reloaded = SettingsPane::load(Some(path));
    assert_eq!(reloaded.king(), pane.king());
    assert_eq!(reloaded.whip(), pane.whip());
}

#[test]
fn clicking_save_reports_the_king_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut pane = SettingsPane::load(Some(dir.path().join("settings.json")));
    pane.open_section(Section::King);
    pane.king_draft.pick(Some("sonnet"));
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Save and restart the King", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    assert_eq!(out.king.unwrap().model.as_deref(), Some("sonnet"));
}

#[test]
fn dwarves_default_saves_in_the_king_section_and_keeps_the_kings_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.king_draft.pick(Some("opus"));
    pane.save_king().unwrap();
    assert_eq!(
        pane.dwarves(),
        &DwarfSettings::default(),
        "off until chosen"
    );

    pane.open_section(Section::King);
    pane.dwarves_draft.pick(Some("fable"));
    pane.dwarves_draft.settings.effort = Some("high".into());
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Save for new dwarves", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    let dwarves = out.dwarves.unwrap();
    assert_eq!(dwarves.model.as_deref(), Some("fable"));
    assert_eq!(dwarves.effort.as_deref(), Some("high"));
    assert_eq!(out.king, None, "the King's own save row was not clicked");
    assert_eq!(pane.dwarves(), &dwarves);
    assert_eq!(pane.king().model.as_deref(), Some("opus"));
    let on_disk = Settings::load_from(&path).settings;
    assert_eq!(on_disk.dwarves, dwarves);
    assert_eq!(on_disk.king.model.as_deref(), Some("opus"));

    // Back to the CLI default; an invalid effort saves nothing.
    pane.dwarves_draft.pick(None);
    pane.dwarves_draft.settings.effort = None;
    assert_eq!(pane.save_dwarves().unwrap(), DwarfSettings::default());
    pane.dwarves_draft.settings.effort = Some("turbo".into());
    assert_eq!(pane.save_dwarves(), None);
    assert!(pane.message.as_ref().unwrap().0);
    assert_eq!(
        SettingsPane::load(Some(path)).dwarves(),
        &DwarfSettings::default()
    );
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

fn voices() -> Vec<crate::voice::tts::Voice> {
    ["af_heart", "ff_siwis", "bm_george", "jf_alpha", "am_adam"]
        .iter()
        .enumerate()
        .map(|(id, name)| crate::voice::tts::Voice {
            id: id as i32,
            name: (*name).into(),
        })
        .collect()
}

#[test]
fn voices_list_english_first() {
    let voices = voices();
    let names: Vec<&str> = voice::ordered_voices(&voices)
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["af_heart", "bm_george", "am_adam", "ff_siwis", "jf_alpha"]
    );
}

/// The download buttons only report the click: nothing is fetched by the pane.
#[test]
fn missing_models_offer_downloads_without_starting_them() {
    let status = WhipStatus::default();
    let info = general();
    let voices = voices();
    let mut pane = SettingsPane::load(None);
    pane.open_section(Section::Voice);
    let mut c = cx(&status, &info, KingStatus::default());
    c.voice = VoiceStatus {
        voices: &voices,
        speech_model: ModelStatus::Installed("Orca's copy".into()),
        voice_model: ModelStatus::Missing,
        hotkey_error: Some("⌘⇧E is taken by another app"),
    };
    let labels = [
        "Orca's copy",
        "⌘⇧E is taken by another app",
        "Heart (American English, female)",
    ];
    let want: Vec<&str> = labels.into_iter().collect();
    let voice_label = voice::voice_download_label();
    assert_eq!(voice_label, "Download King voice (333 MB)");
    let mut want_all = want.clone();
    want_all.push(&voice_label);
    assert_eq!(drawn(&mut pane, &c, &want_all).len(), want_all.len());

    let out = click_label(&voice_label, |ui| pane.show(ui.ctx(), &c));
    assert_eq!(out.download, Some(VoiceModel::Voice));
    assert_eq!(out.voice, None);

    // Test voice needs the model.
    let out = click_label("▶ Test voice", |ui| pane.show(ui.ctx(), &c));
    assert_eq!(out.test_voice, None, "disabled without the voice model");
    c.voice.voice_model = ModelStatus::Installed("Loaded".into());
    c.voice.speech_model = ModelStatus::Missing;
    pane.voice_draft.speed_percent = 130;
    let out = click_label("▶ Test voice", |ui| pane.show(ui.ctx(), &c));
    assert_eq!(out.test_voice.unwrap().speed_percent, 130);
    let speech_label = voice::speech_download_label();
    let out = click_label(&speech_label, |ui| pane.show(ui.ctx(), &c));
    assert_eq!(out.download, Some(VoiceModel::Speech));
}

#[test]
fn voice_settings_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.open_section(Section::Voice);
    pane.voice_draft.voice = "bm_george".into();
    pane.voice_draft.talk_hotkey = "alt+Space".into();
    pane.voice_draft.volume_percent = 35;
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Save", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    let saved = out.voice.expect("saved");
    assert_eq!(saved.voice, "bm_george");
    let reloaded = SettingsPane::load(Some(path));
    assert_eq!(reloaded.voice(), &saved);
    assert_eq!(reloaded.voice().talk_hotkey_label(), "⌥Space");
}

#[test]
fn king_speak_replies_applies_and_saves_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.open_section(Section::King);
    let status = WhipStatus::default();
    let info = general();
    let out = click_label("Speak replies", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    assert!(out.voice.expect("applied").speak_replies);
    assert!(pane.voice().speak_replies);
    assert!(SettingsPane::load(Some(path)).voice().speak_replies);
    // The palette's toggle goes through the same call.
    assert!(!pane.set_speak_replies(false).speak_replies);
}

#[test]
fn notification_test_buttons_report_the_draft() {
    let status = WhipStatus::default();
    let info = general();
    let mut pane = SettingsPane::load(None);
    pane.open_section(Section::Notifications);
    pane.notify_draft.volume_percent = 40;
    let out = click_label("Test urgent", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    let (severity, settings) = out.test_notification.expect("test notification");
    assert_eq!(severity, crate::notify::Severity::Urgent);
    assert_eq!(settings.volume_percent, 40, "the unsaved draft");
    assert_eq!(pane.notifications().volume_percent, 70, "nothing saved");

    let out = click_label("▶ Test", |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    assert_eq!(
        out.test_sound,
        Some((crate::notify::Sound::System("Tink".into()), 40))
    );
}

#[test]
fn notification_settings_save_and_quiet_toggles_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.notify_draft.popup_min = crate::notify::Severity::Warn;
    pane.notify_draft.sounds.info = crate::notify::Sound::File(PathBuf::new());
    assert_eq!(
        pane.save_notifications(),
        None,
        "an empty custom file is refused"
    );
    pane.notify_draft.sounds.info = crate::notify::Sound::None;
    let saved = pane.save_notifications().unwrap();
    assert_eq!(saved.popup_min, crate::notify::Severity::Warn);
    let quiet = pane.set_quiet(true);
    assert!(quiet.quiet);
    assert_eq!(quiet.popup_min, crate::notify::Severity::Warn);
    let loaded = crate::whip::Settings::load_from(&path)
        .settings
        .notifications;
    assert_eq!(loaded, quiet);
}

#[test]
fn envoy_save_reports_safety_on_and_explains_codex_restriction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut settings = Settings::default();
    settings.envoy.dry_run = false;
    settings.envoy.agent = "codex".into();
    settings.save_to(&path).unwrap();
    let mut pane = SettingsPane::load(Some(path.clone()));
    pane.open_section(Section::Envoy);
    let status = WhipStatus::default();
    let info = general();
    let context = cx(&status, &info, KingStatus::default());
    let warning = "Codex Envoy starts are unavailable: its MCP tool restrictions are not supported. Choose Claude.";
    assert_eq!(drawn(&mut pane, &context, &[warning]), [warning]);
    let mut safe = pane.envoy().clone();
    safe.dry_run = true;
    pane.envoy_draft = envoy::EnvoyDraft::new(&safe);
    let out = click_label("Save Envoy settings", |ui| pane.show(ui.ctx(), &context));
    assert!(out.envoy.expect("save must reach the app").dry_run);
    assert!(Settings::load_from(&path).settings.envoy.dry_run);
}

#[test]
fn focus_new_dwarves_is_off_by_default_and_saves_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"whip":{"dry_run":false}}"#).unwrap();
    let mut pane = SettingsPane::load(Some(path.clone()));
    assert!(
        !pane.spawn().focus_new,
        "a file without `spawn` loads it off"
    );

    pane.open_section(Section::General);
    let status = WhipStatus::default();
    let info = general();
    click_label(general::FOCUS_NEW_LABEL, |ui| {
        pane.show(ui.ctx(), &cx(&status, &info, KingStatus::default()))
    });
    assert!(pane.spawn().focus_new);
    let reloaded = SettingsPane::load(Some(path.clone()));
    assert!(reloaded.spawn().focus_new);
    assert!(!reloaded.whip().dry_run, "other sections kept");

    assert!(!pane.set_focus_new(false).focus_new);
    assert!(!SettingsPane::load(Some(path)).spawn().focus_new);
}
