//! The app side of the health doctor: the probe worker, the current report and its badge.

use eframe::egui;

use super::{MyrstackApp, now_ms, repaint_once};
use crate::health::{AppFacts, HealthReport, HealthWorker, Probes, QueenFact, evaluate};
use crate::ui::health::HealthAction;

pub(super) struct AppHealth {
    worker: HealthWorker,
    probes: Option<Probes>,
    /// What `report` was evaluated from.
    facts: Option<AppFacts>,
    pub(super) report: HealthReport,
    /// Bumped whenever `report` changes, for the control API's fingerprint.
    pub(super) generation: u64,
}

impl AppHealth {
    pub(super) fn start(ctx: &egui::Context) -> Self {
        Self {
            worker: HealthWorker::spawn(Some(repaint_once(ctx))),
            probes: None,
            facts: None,
            report: HealthReport::default(),
            generation: 0,
        }
    }
}

impl MyrstackApp {
    fn health_facts(&self) -> AppFacts {
        let whip = self.whip_window.whip();
        AppFacts {
            whip_enabled: whip.enabled,
            whip_dry_run: whip.dry_run,
            queen: QueenFact::from(self.chat.state()),
        }
    }

    /// Takes new probe results and re-evaluates the report when they or the app's facts
    /// changed.
    pub(super) fn poll_health(&mut self) {
        let mut changed = false;
        if let Some(probes) = self.health.worker.latest() {
            self.health.probes = Some(probes);
            changed = true;
        }
        let facts = self.health_facts();
        if self.health.facts.as_ref() != Some(&facts) {
            changed = true;
        }
        if !changed {
            return;
        }
        let report = evaluate(&facts, self.health.probes.as_ref(), now_ms());
        if report != self.health.report {
            for check in report.problems() {
                let was = self
                    .health
                    .report
                    .checks
                    .iter()
                    .find(|c| c.key == check.key);
                if was.is_none_or(|w| w.level != check.level) {
                    log::warn!("health: {}: {}", check.name, check.detail);
                }
            }
            self.health.report = report;
            self.health.generation += 1;
        }
        self.health.facts = Some(facts);
    }

    pub(super) fn health_badge(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        match crate::ui::health::health_badge(ui, &self.health.report, now_ms) {
            Some(HealthAction::CheckNow) => self.health.worker.check_now(),
            Some(HealthAction::OpenLog) => {
                std::thread::spawn(crate::logfile::open_in_console);
            }
            None => {}
        }
    }
}
