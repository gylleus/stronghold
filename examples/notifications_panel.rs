//! The notifications panel on synthetic entries: unread, read, a repeat and one expanded.
//!
//! `cargo run --example notifications_panel [--shot out.png] [--light]`: with `--shot`, saves a
//! screenshot after a moment and exits.

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::notify::Severity;
use myrstack::notify::Source;
use myrstack::notify::history::Entry;
use myrstack::ui::notifications::NotificationsPanel;

const WINDOW: [f32; 2] = [420.0, 720.0];

#[allow(clippy::too_many_arguments)]
fn entry(
    id: u64,
    minutes_ago: i64,
    source: Source,
    severity: Severity,
    title: &str,
    message: &str,
    agent: Option<&str>,
    url: Option<&str>,
    count: u32,
    read: bool,
) -> Entry {
    let at_ms = now_ms() - minutes_ago * 60_000;
    Entry {
        id,
        at_ms,
        first_at_ms: at_ms - i64::from(count - 1) * 7 * 60_000,
        source,
        severity,
        title: title.into(),
        message: message.into(),
        url: url.map(Into::into),
        target: agent.map(|a| format!("term_{a}")),
        sender: agent.map(|a| format!("term_{a}")),
        agent_label: agent.map(Into::into),
        count,
        read,
    }
}

/// Oldest first, as the history keeps them.
fn entries() -> Vec<Entry> {
    vec![
        entry(
            1,
            95,
            Source::Review,
            Severity::Info,
            "PR ready for review: graph trackpad pan",
            "The branch is merged locally and the gates are green.",
            Some("graph-trackpad-pan"),
            Some("https://github.com/gylleus/myrstack/pull/12"),
            1,
            true,
        ),
        entry(
            2,
            40,
            Source::NeedsInput,
            Severity::Warn,
            "Waiting for input",
            "Allow `cargo install --path .`?",
            Some("worktree-cleanup"),
            None,
            3,
            true,
        ),
        entry(
            3,
            12,
            Source::Agent,
            Severity::Info,
            "Farm: death animation merged",
            "Removed dwarves now flip legs up and fall to the bottom of the farm. Merged into main; \
             reopen Myrstack to see it.",
            Some("dwarf-death-animation"),
            Some("https://example.com/farm"),
            1,
            false,
        ),
        entry(
            4,
            3,
            Source::Agent,
            Severity::Urgent,
            "Which voice model should the King use?",
            "The small model mishears agent names; the large one is 1.2 GB. Reply in the chat.",
            Some("voice-turn"),
            None,
            1,
            false,
        ),
        entry(
            5,
            1,
            Source::Escalation,
            Severity::Warn,
            "Stalled for 20 minutes",
            "The whip nudged it twice without progress.",
            Some("playbook-evaluation"),
            None,
            1,
            false,
        ),
    ]
}

struct Demo {
    panel: NotificationsPanel,
    entries: Vec<Entry>,
    shot: Option<String>,
    since: Instant,
    shooting: bool,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.panel.show(ui, &self.entries, false, false, now_ms());
            });
        let Some(path) = &self.shot else { return };
        if !self.shooting && self.since.elapsed() > Duration::from_millis(1500) {
            self.shooting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .expect("save screenshot");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let light = args.iter().any(|a| a == "--light");
    eframe::run_native(
        "notifications panel",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            if light {
                cc.egui_ctx.set_theme(egui::Theme::Light);
            }
            let mut panel = NotificationsPanel::default();
            panel.expand(3);
            Ok(Box::new(Demo {
                panel,
                entries: entries(),
                shot,
                since: Instant::now(),
                shooting: false,
            }))
        }),
    )
}
