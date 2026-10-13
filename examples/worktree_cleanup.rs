//! The worktree cleanup window on a synthetic plan, with the hammer running along the bottom
//! (driven by a sweep marker in a temporary data dir, as when the King runs `clean_worktrees`).
//! `cargo run --example farm -- --hammer` shows it breaking heaps in the farm.
//!
//! `cargo run --example worktree_cleanup [--shot out.png] [--light]`: with `--shot`, saves a
//! screenshot after a moment and exits.

use std::time::{Duration, Instant};

use eframe::egui;
use stronghold::ui::worktrees::WorktreePanel;
use stronghold::worktrees::{BuildDir, Plan, PlanEntry, SweepMarker, Verdict};

const WINDOW: [f32; 2] = [1100.0, 720.0];
const GB: u64 = 1024 * 1024 * 1024;

fn entry(repo: &str, name: &str, bytes: Option<u64>, remove: bool, reason: &str) -> PlanEntry {
    PlanEntry {
        id: format!("{repo}::{name}"),
        name: name.into(),
        repo: repo.into(),
        path: format!("~/orca/workspaces/{repo}/{name}"),
        branch: Some(format!("gylleus/{name}")),
        bytes,
        // Kept worktrees show what their build output costs.
        build: bytes
            .filter(|_| !remove)
            .map(|b| {
                vec![
                    BuildDir {
                        path: "target".into(),
                        bytes: b * 3 / 5,
                    },
                    BuildDir {
                        path: ".venv".into(),
                        bytes: b / 5,
                    },
                ]
            })
            .unwrap_or_default(),
        verdict: Verdict {
            remove,
            reasons: vec![reason.into()],
            ..Verdict::default()
        },
    }
}

fn plan() -> Plan {
    Plan {
        entries: vec![
            entry(
                "stronghold",
                "farm-view",
                Some(12 * GB),
                true,
                "everything is in origin/main",
            ),
            entry(
                "stronghold",
                "graph-trackpad-pan",
                Some(6 * GB),
                true,
                "merged into main",
            ),
            entry(
                "recommendations",
                "recs-pool-dedupe",
                Some(5 * GB),
                true,
                "PR #135 merged; HEAD is on origin/gylleus/recs-pool-dedupe",
            ),
            entry(
                "depict",
                "docs-recs-custom-css",
                Some(GB / 3),
                true,
                "PR #8291 closed; HEAD is on origin/gylleus/docs-recs-custom-css",
            ),
            entry(
                "recommendations",
                "rperf",
                Some(4 * GB),
                true,
                "HEAD is in merged PR #150's head",
            ),
            entry(
                "recommendations",
                "serving-field",
                Some(GB * 12 / 5),
                true,
                "PR #137 closed; HEAD is on origin/gylleus/live-static-baseline; first saves 19 \
                 notes (ANSWER-1.md, ANSWER-10.md, ANSWER-2.md, +16 more)",
            ),
            entry(
                "recommendations",
                "demo-check-shopify-13374187",
                Some(3 * GB),
                false,
                "untracked files need a decision: demo-check/ (139 MB)",
            ),
            entry(
                "depict",
                "sra-storefront",
                None,
                false,
                "live terminal: sra-storefront; PR #8118 is open",
            ),
            entry(
                "realtime",
                "median-per-pricelist",
                Some(2 * GB),
                false,
                "1 uncommitted change (?? MEDIAN-PRICELIST-REPORT.md)",
            ),
        ],
        orphans: vec!["~/orca/workspaces/platform/upload-flow-fix".into()],
        evaluated_at_ms: 0,
    }
}

struct Demo {
    panel: WorktreePanel,
    shot: Option<String>,
    since: Instant,
    shooting: bool,
    /// Holds the sweep marker.
    _data: tempfile::TempDir,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Farm goes here");
        });
        self.panel.tick(None, now_ms());
        self.panel.window(&ctx);
        self.panel.hammer(&ctx);
        let Some(path) = &self.shot else { return };
        if !self.shooting && self.since.elapsed() > Duration::from_millis(2500) {
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
    let data = tempfile::tempdir().expect("temp dir");
    // SAFETY: set before any thread starts.
    unsafe { std::env::set_var("STRONGHOLD_DATA_DIR", data.path()) };
    let marker = SweepMarker {
        started_at_ms: now_ms(),
        updated_at_ms: now_ms() + 3_600_000,
        total: 4,
        done: 1,
        current: Some("graph-trackpad-pan".into()),
        current_id: Some("stronghold::graph-trackpad-pan".into()),
    };
    std::fs::write(
        stronghold::worktrees::default_marker_path(),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    eframe::run_native(
        "worktree cleanup",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            stronghold::ui::theme::install(&cc.egui_ctx);
            if light {
                cc.egui_ctx.set_theme(egui::Theme::Light);
            }
            let mut panel = WorktreePanel::new(&cc.egui_ctx);
            panel.show_plan(plan());
            Ok(Box::new(Demo {
                panel,
                shot,
                since: Instant::now(),
                shooting: false,
                _data: data,
            }))
        }),
    )
}
