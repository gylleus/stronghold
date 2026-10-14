//! A read-only Ledger pane. One worker handles searches, file reads and opening the editor.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, Ui};

use crate::ledger::{
    Entry, Shelf,
    search::{self, Hit, Mode, Results},
};

#[derive(Debug)]
enum Job {
    Search(String, Mode),
    Newest(Shelf),
    Read(Entry),
    Open(Entry),
}

enum Reply {
    Results(Results),
    Read(String),
    Opened,
}

type Outcome = (u64, Result<Reply, String>);

struct Worker {
    tx: Sender<(u64, Job)>,
    rx: Receiver<Outcome>,
}

impl Worker {
    fn start(ctx: egui::Context, root: PathBuf) -> std::io::Result<Self> {
        let (tx, jobs) = unbounded::<(u64, Job)>();
        let (results, rx) = unbounded();
        std::thread::Builder::new()
            .name("ledger-search".into())
            .spawn(move || {
                while let Ok((mut id, mut job)) = jobs.recv() {
                    // Only the newest intent matters when a slow search is still running.
                    while let Ok(next) = jobs.try_recv() {
                        (id, job) = next;
                    }
                    let result = perform(&root, job).map_err(|e| format!("{e:#}"));
                    if results.send((id, result)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self { tx, rx })
    }
}

fn perform(root: &std::path::Path, job: Job) -> anyhow::Result<Reply> {
    match job {
        Job::Search(query, mode) => search::search(root, &query, mode).map(Reply::Results),
        Job::Newest(shelf) => Ok(Reply::Results(search::newest(root, &shelf))),
        Job::Read(entry) => search::read(root, &entry).map(Reply::Read),
        Job::Open(entry) => {
            let path = search::note_path(root, &entry.path)?;
            let status = std::process::Command::new("/usr/bin/open")
                .arg(path)
                .status()?;
            anyhow::ensure!(
                status.success(),
                "Could not open the entry in its default editor"
            );
            Ok(Reply::Opened)
        }
    }
}

#[derive(Default)]
pub struct LedgerPane {
    pub open_requested: bool,
    pub tend_requested: bool,
    query: String,
    mode: Mode,
    worker: Option<Worker>,
    generation: u64,
    busy: bool,
    shelf: Option<Shelf>,
    hits: Vec<Hit>,
    selected: Option<Entry>,
    markdown: Option<String>,
    fallback: Option<String>,
    error: Option<String>,
    searched: Option<String>,
}

impl LedgerPane {
    fn send(&mut self, job: Job) {
        self.generation += 1;
        self.busy = true;
        self.error = None;
        if self
            .worker
            .as_ref()
            .is_none_or(|w| w.tx.send((self.generation, job)).is_err())
        {
            self.busy = false;
            self.error = Some("Ledger worker is unavailable".into());
        }
    }

    fn accept(&mut self, (id, result): Outcome) {
        if id != self.generation {
            return;
        }
        self.busy = false;
        match result {
            Ok(Reply::Results(results)) => {
                self.hits = results.hits;
                self.fallback = results.fallback;
            }
            Ok(Reply::Read(text)) => self.markdown = Some(text),
            Ok(Reply::Opened) => {}
            Err(error) => self.error = Some(error),
        }
    }

    fn run_search(&mut self, shelf: &Shelf) {
        self.selected = None;
        self.markdown = None;
        self.hits.clear();
        self.fallback = None;
        let query = self.query.trim().to_owned();
        self.searched = (!query.is_empty()).then(|| query.clone());
        if query.is_empty() {
            self.shelf = Some(shelf.clone());
            self.send(Job::Newest(shelf.clone()));
        } else {
            self.send(Job::Search(query, self.mode));
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        shelf: &Shelf,
        now_ms: i64,
        tending: bool,
        launch_error: Option<&str>,
        report: Option<&crate::lorekeeper::report::SavedReport>,
    ) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!tending, egui::Button::new("Tend the Ledger"))
                .clicked()
            {
                self.tend_requested = true;
            }
            if tending {
                ui.label("Tending…");
            } else if let Some(error) = launch_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            } else if let Some(saved) = report {
                let mode = if saved.report.dry_run {
                    " (dry run)"
                } else {
                    ""
                };
                let ago = crate::quota::format_duration(now_ms.saturating_sub(saved.at_ms).max(0));
                ui.label(format!(
                    "Last tended {ago} ago{mode}: {}",
                    saved.report.counts()
                ));
            } else {
                ui.weak("Not tended yet");
            }
        });
        if let Some(saved) = report {
            ui.collapsing("Last report details", |ui| {
                ui.label(format!("Snapshot: {}", saved.snapshot.display()));
                let mut items = saved
                    .report
                    .items
                    .iter()
                    .filter(|i| i.action != crate::lorekeeper::report::Action::Keep)
                    .collect::<Vec<_>>();
                items.sort_by_key(|i| i.action != crate::lorekeeper::report::Action::Flag);
                egui::ScrollArea::vertical()
                    .id_salt("lorekeeper_report")
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for item in items {
                            ui.label(format!(
                                "{} · {}: {}",
                                item.action.name(),
                                item.path,
                                item.reason
                            ));
                            if let Some(target) = &item.target {
                                ui.weak(format!("→ {target}"));
                            }
                        }
                    });
            });
        }
        ui.separator();
        if self.worker.is_none() {
            let Some(root) = crate::ledger::default_dir() else {
                ui.label("Ledger folder is unavailable");
                return;
            };
            match Worker::start(ui.ctx().clone(), root) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    ui.label(format!("Could not start Ledger search: {error}"));
                    return;
                }
            }
        }
        while let Some(reply) = self.worker.as_ref().and_then(|w| w.rx.try_recv().ok()) {
            self.accept(reply);
        }
        let mut submit = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search the Ledger…")
                    .desired_width((ui.available_width() - 75.0).max(60.0)),
            );
            submit = (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Search").clicked();
            if edit.changed() && self.query.trim().is_empty() {
                submit = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Search by:");
            for (mode, label) in [
                (Mode::Auto, "Auto"),
                (Mode::Meaning, "Meaning"),
                (Mode::Text, "Text"),
            ] {
                if ui.selectable_value(&mut self.mode, mode, label).changed() {
                    submit = true;
                }
            }
        });
        if self.mode == Mode::Auto {
            ui.weak("Meaning for questions · text for IDs such as ABC-123 or myr-bas");
        }
        if submit || (self.query.trim().is_empty() && self.shelf.as_ref() != Some(shelf)) {
            self.run_search(shelf);
        }
        if let Some(error) = &self.fallback {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Ledger server unavailable — showing local plain text matches.",
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading…");
            });
        }
        ui.separator();
        match &self.searched {
            Some(query) => {
                ui.label(format!(
                    "{} results for “{query}” (up to 30)",
                    self.hits.len()
                ));
            }
            None => {
                ui.label(format!("Newest entries · {} in the Ledger", shelf.count));
            }
        }
        let mut pick = None;
        let height = if self.selected.is_some() {
            ui.available_height() * 0.35
        } else {
            ui.available_height()
        };
        egui::ScrollArea::vertical()
            .id_salt("ledger-results")
            .max_height(height.max(50.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.hits.is_empty() && !self.busy && self.error.is_none() {
                    ui.weak("No entries found.");
                }
                for hit in &self.hits {
                    let entry = &hit.entry;
                    let selected = self.selected.as_ref().is_some_and(|s| s.path == entry.path);
                    if ui.selectable_label(selected, &entry.title).clicked() {
                        pick = Some(entry.clone());
                    }
                    let age = if entry.modified_ms == 0 {
                        "age unknown".into()
                    } else {
                        format!(
                            "{} ago",
                            crate::quota::format_duration((now_ms - entry.modified_ms).max(0))
                        )
                    };
                    ui.weak(format!(
                        "{} · {} · {age}",
                        if entry.folder.is_empty() {
                            "Ledger"
                        } else {
                            &entry.folder
                        },
                        entry.kind.as_deref().unwrap_or("entry")
                    ));
                    ui.label(&hit.snippet);
                    ui.separator();
                }
            });
        if let Some(entry) = pick {
            self.selected = Some(entry.clone());
            self.markdown = None;
            self.send(Job::Read(entry));
        }
        if let Some(entry) = self.selected.clone() {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.heading(&entry.title);
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Open in default editor"))
                    .clicked()
                {
                    self.send(Job::Open(entry.clone()));
                }
            });
            egui::ScrollArea::vertical()
                .id_salt(("ledger-entry", &entry.path))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(markdown) = &self.markdown {
                        crate::ui::chat::markdown(ui, markdown, &|_| false);
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_search_cannot_replace_the_current_query() {
        let mut pane = LedgerPane {
            generation: 2,
            busy: true,
            ..Default::default()
        };
        pane.accept((1, Err("stale failure".into())));
        assert!(pane.busy);
        assert!(pane.error.is_none());
        pane.accept((2, Ok(Reply::Results(Results::default()))));
        assert!(!pane.busy);
    }
}

#[cfg(test)]
mod lorekeeper_tests {
    use super::*;
    use crate::ui::test_util::click_label;

    #[test]
    fn tend_button_raises_one_request_and_is_disabled_during_a_run() {
        let (tx, _jobs) = unbounded();
        let (_results, rx) = unbounded();
        let mut pane = LedgerPane {
            worker: Some(Worker { tx, rx }),
            ..Default::default()
        };
        for busy in [false, true] {
            pane.tend_requested = false;
            click_label("Tend the Ledger", |ui| {
                pane.show(ui, &Shelf::default(), 0, busy, None, None)
            });
            assert_eq!(pane.tend_requested, !busy);
        }
    }
}
