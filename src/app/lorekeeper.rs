//! Ledger maintenance controls and scheduled runs.

use super::{StrongholdApp, now_ms, repaint_once};
use crate::lorekeeper::{
    Settings,
    report::{SavedReport, Watch},
    session::Session,
};
use crate::schedule::{Run, Schedule};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const TARGET: &str = "lorekeeper:report:";

pub struct AppLorekeeper {
    pub session: Session,
    pub reports: Watch,
    schedule: Schedule,
    last: Option<Run>,
    log: PathBuf,
    started: Instant,
    last_tick: Instant,
}

impl AppLorekeeper {
    pub fn new(data: &Path) -> Self {
        let log = data.join("lorekeeper/runs.jsonl");
        Self {
            session: Session::new(data),
            reports: Watch::new(data.join("lorekeeper")),
            schedule: Schedule::default(),
            last: Run::load_last(&log),
            log,
            started: Instant::now(),
            last_tick: Instant::now(),
        }
    }

    fn tick(&mut self, settings: &Settings, ctx: &egui::Context) -> Vec<SavedReport> {
        self.session.poll();
        if self.last_tick.elapsed() < Duration::from_secs(2) {
            return Vec::new();
        }
        self.last_tick = Instant::now();
        self.session.reload();
        let reports = self.reports.poll();
        let last = Run::load_last(&self.log);
        if last != self.last {
            if let Some(last) = &last {
                self.schedule.finished(last.at_ms);
            }
            self.last = last;
        }
        if self.started.elapsed() < Duration::from_secs(120) {
            return reports;
        }
        let now = now_ms();
        let period = settings.period_ms();
        let last = self
            .last
            .as_ref()
            .map(|r| r.at_ms)
            .or_else(|| period.map(|p| now.saturating_sub(p)));
        if self.schedule.claim(period, now, last, false, false) {
            let busy = self.session.busy();
            let run = Run {
                at_ms: now,
                summary: if busy {
                    "Lorekeeper skipped: already running"
                } else {
                    "Lorekeeper scheduled"
                }
                .into(),
            };
            match run.append(&self.log) {
                Ok(()) => {
                    self.last = Some(run);
                    if !busy && let Err(e) = self.session.start(settings, repaint_once(ctx)) {
                        self.session.error = Some(e.to_string());
                    }
                }
                Err(e) => {
                    self.session.error = Some(format!("Cannot save Lorekeeper schedule: {e}"))
                }
            }
        }
        reports
    }
}

impl StrongholdApp {
    pub(super) fn poll_lorekeeper(&mut self, ctx: &egui::Context) {
        for report in self.lorekeeper.tick(self.whip_window.lorekeeper(), ctx) {
            let mut message = report.report.counts();
            if report.report.dry_run {
                message.push_str(" (dry run)");
            }
            if report.report.flagged > 0 {
                message.push_str(&format!(" · {} flagged for you", report.report.flagged));
            }
            self.raise(
                crate::notify::Incoming::new(
                    crate::notify::Source::Lorekeeper,
                    crate::notify::Severity::Info,
                    "Ledger tended",
                    &message,
                )
                .with_target(format!("{TARGET}{}", report.run_id)),
            );
        }
    }

    pub(super) fn ledger_modal(&mut self, ctx: &egui::Context, now: i64) {
        let busy = self.lorekeeper.session.busy();
        let error = self.lorekeeper.session.error.as_deref().or(self
            .lorekeeper
            .reports
            .error
            .as_deref());
        self.ledger_panel.show_modal(
            ctx,
            self.ledger.poll(),
            now,
            crate::ui::ledger::Maintenance {
                tending: busy,
                launch_error: error,
                report: self.lorekeeper.reports.latest.as_ref(),
            },
        );
        if std::mem::take(&mut self.ledger_panel.tend_requested)
            && let Err(e) = self
                .lorekeeper
                .session
                .start(self.whip_window.lorekeeper(), repaint_once(ctx))
        {
            self.lorekeeper.session.error = Some(e.to_string());
        }
    }
}
