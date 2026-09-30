//! "New ant…" dialog: pick where (a worktree, a new worktree, or a run as its worker), the
//! agent and an initial prompt; see the planned `orca` commands; launch and watch each step.

use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{self, Color32, RichText};

use crate::fleet::FleetData;
use crate::orca::spawn::WorkerPlacement;
use crate::spawn::exec::JobId;
use crate::spawn::{
    Agent, Catalog, LiveAgent, ModeName, SpawnEvent, SpawnOutcome, SpawnPlan, SpawnRequest,
    SpawnTarget, StepStatus, build_plan, exec::SpawnFailure, name_conflict, new_worktree_of,
};

/// Longest run objective or terminal label shown in a picker.
const LABEL_CHARS: usize = 70;

/// Where a worker runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkerPlace {
    #[default]
    NewWorktree,
    Worktree,
    Terminal,
}

impl WorkerPlace {
    const ALL: [WorkerPlace; 3] = [
        WorkerPlace::NewWorktree,
        WorkerPlace::Worktree,
        WorkerPlace::Terminal,
    ];

    fn label(self) -> &'static str {
        match self {
            WorkerPlace::NewWorktree => "New worktree",
            WorkerPlace::Worktree => "Existing worktree",
            WorkerPlace::Terminal => "Existing terminal",
        }
    }
}

/// A run the dialog offers for worker mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunChoice {
    pub id: String,
    pub label: String,
    pub coordinator: Option<String>,
}

/// A live terminal the dialog offers for a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalChoice {
    pub handle: String,
    pub label: String,
    pub worktree_id: Option<String>,
    pub worktree_path: Option<String>,
}

/// The fleet facts the dialog needs besides Orca's repo and worktree lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnFleet {
    pub runs: Vec<RunChoice>,
    pub terminals: Vec<TerminalChoice>,
    /// Live agents, named when a new worktree's name is taken.
    pub agents: Vec<LiveAgent>,
}

impl SpawnFleet {
    pub fn from_data(data: Option<&FleetData>) -> Self {
        let Some(data) = data else {
            return Self::default();
        };
        let runs = data
            .runs
            .iter()
            .filter(|r| !r.legacy)
            .map(|r| RunChoice {
                id: r.id.clone(),
                label: shorten(&format!(
                    "{} · {}",
                    r.id,
                    r.objective.as_deref().unwrap_or("(no objective)")
                )),
                coordinator: r.coordinator_handle.clone(),
            })
            .collect();
        let terminals = data
            .terminals
            .iter()
            .filter(|t| t.connected && !t.orphaned && t.agent_identity.is_some())
            .map(|t| TerminalChoice {
                handle: t.handle.clone(),
                label: shorten(&format!(
                    "{} · {}",
                    t.title.as_deref().unwrap_or(&t.handle),
                    t.branch_name().unwrap_or("?")
                )),
                worktree_id: t.worktree_id.clone(),
                worktree_path: t.worktree_path.clone(),
            })
            .collect();
        Self {
            runs,
            terminals,
            agents: Vec::new(),
        }
    }

    /// Adds the snapshot's live agents.
    pub fn with_agents(mut self, snapshot: Option<&crate::model::FleetSnapshot>) -> Self {
        self.agents = snapshot.map(LiveAgent::from_snapshot).unwrap_or_default();
        self
    }
}

fn shorten(s: &str) -> String {
    if s.chars().count() <= LABEL_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(LABEL_CHARS - 1).collect();
    format!("{cut}…")
}

/// What the form holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnForm {
    pub mode: ModeName,
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
    pub name: String,
    pub base_branch: String,
    pub run_id: Option<String>,
    pub place: WorkerPlace,
    pub terminal: Option<String>,
    pub title: String,
    pub agent: Agent,
    pub model: String,
    pub prompt: String,
    /// Let Orca suffix a taken new worktree name instead of refusing it.
    pub allow_suffix: bool,
}

impl SpawnForm {
    /// The request the form describes, or what is missing.
    pub fn request(&self, catalog: &Catalog, fleet: &SpawnFleet) -> Result<SpawnRequest, String> {
        let worktree = || {
            let id = self.worktree_id.as_deref().ok_or("pick a worktree")?;
            catalog
                .worktree(id)
                .ok_or_else(|| "the picked worktree is gone; pick again".to_string())
        };
        let repo = || {
            let id = self.repo_id.as_deref().ok_or("pick a repo")?;
            catalog
                .repo(id)
                .map(|r| r.id.clone())
                .ok_or_else(|| "the picked repo is gone; pick again".to_string())
        };
        let target = match self.mode {
            ModeName::Worktree => {
                let wt = worktree()?;
                SpawnTarget::Worktree {
                    worktree_id: wt.id.clone(),
                    path: wt.path.clone(),
                }
            }
            ModeName::NewWorktree => SpawnTarget::NewWorktree {
                repo_id: repo()?,
                name: self.name.clone(),
                base_branch: Some(self.base_branch.clone()).filter(|b| !b.trim().is_empty()),
            },
            ModeName::Worker => {
                let run_id = self.run_id.as_deref().ok_or("pick a run")?;
                let run = fleet
                    .runs
                    .iter()
                    .find(|r| r.id == run_id)
                    .ok_or("the picked run is gone; pick again")?;
                let (placement, path) = match self.place {
                    WorkerPlace::NewWorktree => (
                        WorkerPlacement::NewWorktree {
                            repo_id: repo()?,
                            name: self.name.clone(),
                        },
                        None,
                    ),
                    WorkerPlace::Worktree => {
                        let wt = worktree()?;
                        (
                            WorkerPlacement::Worktree {
                                worktree_id: wt.id.clone(),
                            },
                            Some(wt.path.clone()),
                        )
                    }
                    WorkerPlace::Terminal => {
                        let handle = self.terminal.as_deref().ok_or("pick a terminal")?;
                        let t = fleet
                            .terminals
                            .iter()
                            .find(|t| t.handle == handle)
                            .ok_or("the picked terminal is gone; pick again")?;
                        (
                            WorkerPlacement::Terminal {
                                worktree_id: t.worktree_id.clone().unwrap_or_default(),
                                handle: t.handle.clone(),
                            },
                            t.worktree_path.clone(),
                        )
                    }
                };
                SpawnTarget::Worker {
                    run_id: run.id.clone(),
                    coordinator: run.coordinator.clone().unwrap_or_default(),
                    placement,
                    title: Some(self.title.clone()).filter(|t| !t.trim().is_empty()),
                    path,
                }
            }
        };
        Ok(SpawnRequest {
            target,
            agent: self.agent,
            model: Some(self.model.clone()).filter(|m| !m.trim().is_empty()),
            prompt: self.prompt.clone(),
        })
    }

    /// The plan the form describes, or what is missing.
    pub fn plan(&self, catalog: &Catalog, fleet: &SpawnFleet) -> Result<SpawnPlan, String> {
        let request = self.request(catalog, fleet)?;
        let plan = build_plan(&request).map_err(|e| e.0)?;
        if !self.allow_suffix
            && let Some((repo_id, name)) = new_worktree_of(&plan.request)
            && let Some(conflict) = name_conflict(catalog, repo_id, name, &[])
        {
            return Err(conflict.message(&fleet.agents));
        }
        Ok(plan)
    }
}

/// Orca's repo and worktree lists as the dialog has them.
#[derive(Debug, Default)]
enum CatalogState {
    #[default]
    Unloaded,
    Loading(Receiver<Result<Catalog, String>>),
    Ready(Catalog),
    Failed(String),
}

/// A launched plan and how far it got.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnJobView {
    pub job: JobId,
    pub plan: SpawnPlan,
    pub statuses: Vec<StepStatus>,
    pub result: Option<Result<SpawnOutcome, SpawnFailure>>,
}

impl SpawnJobView {
    pub fn new(job: JobId, plan: SpawnPlan) -> Self {
        let statuses = vec![StepStatus::Pending; plan.steps.len()];
        Self {
            job,
            plan,
            statuses,
            result: None,
        }
    }

    pub fn running(&self) -> bool {
        self.result.is_none()
    }

    /// Applies one of this job's events.
    pub fn apply(&mut self, event: &SpawnEvent) {
        match event {
            SpawnEvent::Planned { plan, .. } => {
                if self.plan.steps != plan.steps {
                    *self = Self::new(self.job, (**plan).clone());
                }
            }
            SpawnEvent::Step { index, status, .. } => {
                if let Some(slot) = self.statuses.get_mut(*index) {
                    *slot = status.clone();
                }
            }
            SpawnEvent::Done { result, .. } => self.result = Some(result.clone()),
        }
    }
}

/// The dialog's state.
#[derive(Debug, Default)]
pub struct SpawnDialog {
    pub open: bool,
    pub form: SpawnForm,
    catalog: CatalogState,
    /// A sidebar repo name to preselect once the lists are loaded.
    wanted_repo: Option<String>,
    job: Option<SpawnJobView>,
}

impl SpawnDialog {
    /// Opens the dialog, preselecting the repo the sidebar calls `repo` (if any), and reloads
    /// Orca's lists.
    pub fn open(&mut self, repo: Option<&str>) {
        self.open = true;
        if !self.job.as_ref().is_some_and(SpawnJobView::running) {
            self.job = None;
        }
        if !matches!(self.catalog, CatalogState::Loading(_)) {
            self.catalog = CatalogState::Unloaded;
        }
        self.wanted_repo = repo.map(str::to_owned);
        if repo.is_some() && self.form.mode == ModeName::Worker {
            self.form.mode = ModeName::Worktree;
        }
    }

    /// Sets the lists directly (tests, or a caller that already has them).
    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = CatalogState::Ready(catalog);
        self.apply_wanted_repo();
    }

    pub fn catalog(&self) -> Option<&Catalog> {
        match &self.catalog {
            CatalogState::Ready(c) => Some(c),
            _ => None,
        }
    }

    pub fn job(&self) -> Option<&SpawnJobView> {
        self.job.as_ref()
    }

    /// The app launched `plan` as `job`.
    pub fn started(&mut self, job: JobId, plan: SpawnPlan) {
        self.job = Some(SpawnJobView::new(job, plan));
    }

    /// Applies a runner event if it belongs to the dialog's job.
    pub fn on_event(&mut self, event: &SpawnEvent) {
        if let Some(job) = &mut self.job
            && job.job == event.job()
        {
            job.apply(event);
        }
    }

    fn apply_wanted_repo(&mut self) {
        let (Some(name), CatalogState::Ready(catalog)) = (&self.wanted_repo, &self.catalog) else {
            return;
        };
        if let Some(repo) = catalog.repo_for_sidebar(name) {
            let id = repo.id.clone();
            let main = catalog.worktrees_of(&id).first().map(|w| w.id.clone());
            self.form.repo_id = Some(id);
            self.form.worktree_id = main;
        }
        self.wanted_repo = None;
    }

    fn poll_catalog(&mut self, ctx: &egui::Context) {
        match &self.catalog {
            CatalogState::Unloaded => {
                let ctx = ctx.clone();
                let rx = Catalog::fetch_async(Some(Box::new(move || ctx.request_repaint())));
                self.catalog = CatalogState::Loading(rx);
            }
            CatalogState::Loading(rx) => match rx.try_recv() {
                Ok(Ok(catalog)) => self.set_catalog(catalog),
                Ok(Err(e)) => self.catalog = CatalogState::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.catalog = CatalogState::Failed("the listing thread stopped".into());
                }
            },
            CatalogState::Ready(_) | CatalogState::Failed(_) => {}
        }
    }

    /// Draws the dialog; returns the plan to launch when Karl pressed Launch (or ⌘↩).
    pub fn show(&mut self, ctx: &egui::Context, fleet: &SpawnFleet) -> Option<SpawnPlan> {
        if !self.open {
            return None;
        }
        self.poll_catalog(ctx);
        let mut open = true;
        let mut launch = None;
        let form_height = (ctx.content_rect().height() * 0.55).max(240.0);
        egui::Window::new("New ant")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(620.0)
            .show(ctx, |ui| launch = self.contents(ui, fleet, form_height));
        if !open {
            self.open = false;
        }
        launch
    }

    /// The form and planned commands scroll within `form_height`; the launch row, progress and
    /// result stay visible below them.
    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        fleet: &SpawnFleet,
        form_height: f32,
    ) -> Option<SpawnPlan> {
        let catalog = match &self.catalog {
            CatalogState::Ready(c) => c.clone(),
            CatalogState::Failed(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Orca: {e}"));
                if ui.button("Retry").clicked() {
                    self.catalog = CatalogState::Unloaded;
                }
                return None;
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading Orca's repos and worktrees…");
                });
                return None;
            }
        };
        let running = self.job.as_ref().is_some_and(SpawnJobView::running);
        let mut launch = false;
        let mut plan = Err(String::new());
        egui::ScrollArea::vertical()
            .max_height(form_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add_enabled_ui(!running, |ui| {
                    self.form_fields(ui, &catalog, fleet);
                    launch = prompt_field(ui, &mut self.form.prompt);
                });
                plan = self.form.plan(&catalog, fleet);
                ui.add_space(4.0);
                planned_commands(ui, &plan);
            });
        ui.separator();
        ui.horizontal(|ui| {
            let ready = plan.is_ok() && !running;
            let button = ui
                .add_enabled(
                    ready,
                    crate::ui::widgets::primary_button(ui, ready, "Launch"),
                )
                .on_hover_text("⌘↩ in the prompt also launches");
            launch |= button.clicked();
            launch &= ready;
            if let Err(e) = &plan
                && !running
            {
                ui.label(RichText::new(e).weak());
            }
        });
        if let Some(job) = &self.job {
            job_view(ui, job);
        }
        if launch { plan.ok() } else { None }
    }

    fn form_fields(&mut self, ui: &mut egui::Ui, catalog: &Catalog, fleet: &SpawnFleet) {
        let form = &mut self.form;
        ui.horizontal(|ui| {
            ui.label("Mode");
            for mode in ModeName::ALL {
                ui.radio_value(&mut form.mode, mode, mode.label());
            }
        });
        egui::Grid::new("spawn_form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                match form.mode {
                    ModeName::Worktree => {
                        ui.label("Worktree");
                        worktree_picker(ui, catalog, form);
                        ui.end_row();
                    }
                    ModeName::NewWorktree => {
                        new_worktree_fields(ui, catalog, form);
                    }
                    ModeName::Worker => {
                        ui.label("Run");
                        run_picker(ui, fleet, form);
                        ui.end_row();
                        ui.label("Worker in");
                        ui.horizontal(|ui| {
                            for place in WorkerPlace::ALL {
                                ui.radio_value(&mut form.place, place, place.label());
                            }
                        });
                        ui.end_row();
                        match form.place {
                            WorkerPlace::NewWorktree => {
                                ui.label("Repo");
                                repo_picker(ui, catalog, form);
                                ui.end_row();
                                ui.label("Name");
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.name)
                                        .hint_text("new worktree name"),
                                );
                                ui.end_row();
                                ui.label("");
                                suffix_checkbox(ui, form);
                                ui.end_row();
                            }
                            WorkerPlace::Worktree => {
                                ui.label("Worktree");
                                worktree_picker(ui, catalog, form);
                                ui.end_row();
                            }
                            WorkerPlace::Terminal => {
                                ui.label("Terminal");
                                terminal_picker(ui, fleet, form);
                                ui.end_row();
                            }
                        }
                        ui.label("Task title");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.title)
                                .hint_text("defaults to the prompt's first line"),
                        );
                        ui.end_row();
                    }
                }
                ui.label("Agent");
                ui.horizontal(|ui| {
                    for agent in Agent::ALL {
                        ui.radio_value(&mut form.agent, agent, agent.label());
                    }
                    ui.label("Model");
                    let model_locked =
                        form.mode == ModeName::Worker && form.place == WorkerPlace::Terminal;
                    ui.add_enabled(
                        !model_locked,
                        egui::TextEdit::singleline(&mut form.model)
                            .hint_text("default")
                            .desired_width(140.0),
                    );
                });
                ui.end_row();
            });
    }
}

fn planned_commands(ui: &mut egui::Ui, plan: &Result<SpawnPlan, String>) {
    egui::CollapsingHeader::new("Planned commands")
        .default_open(true)
        .show(ui, |ui| match plan {
            Ok(plan) => {
                for (i, line) in plan.display_lines().iter().enumerate() {
                    ui.label(
                        RichText::new(format!("{}. {line}", i + 1))
                            .monospace()
                            .small(),
                    );
                }
            }
            Err(e) => {
                ui.label(RichText::new(e).italics().weak());
            }
        });
}

fn new_worktree_fields(ui: &mut egui::Ui, catalog: &Catalog, form: &mut SpawnForm) {
    ui.label("Repo");
    repo_picker(ui, catalog, form);
    ui.end_row();
    ui.label("Name");
    ui.add(egui::TextEdit::singleline(&mut form.name).hint_text("e.g. fix-checkout-flake"));
    ui.end_row();
    ui.label("");
    suffix_checkbox(ui, form);
    ui.end_row();
    let base = form
        .repo_id
        .as_deref()
        .and_then(|id| catalog.repo(id))
        .and_then(|r| r.worktree_base_ref.clone())
        .unwrap_or_else(|| "repo default".into());
    ui.label("Base ref");
    ui.add(egui::TextEdit::singleline(&mut form.base_branch).hint_text(base));
    ui.end_row();
}

fn suffix_checkbox(ui: &mut egui::Ui, form: &mut SpawnForm) {
    ui.checkbox(
        &mut form.allow_suffix,
        "Allow a suffixed name (name-2) if taken",
    )
    .on_hover_text(
        "Off: a name whose worktree or branch exists is refused, so the same work isn't \
             started twice. On: Orca picks the next free name.",
    );
}

fn repo_picker(ui: &mut egui::Ui, catalog: &Catalog, form: &mut SpawnForm) {
    let selected = form
        .repo_id
        .as_deref()
        .and_then(|id| catalog.repo(id))
        .map_or("pick a repo", |r| r.name());
    egui::ComboBox::from_id_salt("spawn_repo")
        .selected_text(selected)
        .width(300.0)
        .show_ui(ui, |ui| {
            for group in catalog.groups() {
                let id = group.repo.id.clone();
                if ui
                    .selectable_label(form.repo_id.as_ref() == Some(&id), group.repo.name())
                    .on_hover_text(&group.repo.path)
                    .clicked()
                {
                    form.repo_id = Some(id);
                }
            }
        });
}

/// Worktrees grouped by repo, the picked repo's group first.
fn worktree_picker(ui: &mut egui::Ui, catalog: &Catalog, form: &mut SpawnForm) {
    let selected = form
        .worktree_id
        .as_deref()
        .and_then(|id| catalog.worktree(id))
        .map_or_else(
            || "pick a worktree".to_string(),
            |w| {
                let repo = w
                    .repo()
                    .and_then(|r| catalog.repo(r))
                    .map_or("?", |r| r.name());
                format!("{repo} / {}", w.name())
            },
        );
    let mut groups = catalog.groups();
    if let Some(repo) = &form.repo_id {
        groups.sort_by_key(|g| &g.repo.id != repo);
    }
    egui::ComboBox::from_id_salt("spawn_worktree")
        .selected_text(selected)
        .width(360.0)
        .height(420.0)
        .show_ui(ui, |ui| {
            for group in groups.iter().filter(|g| !g.worktrees.is_empty()) {
                ui.label(RichText::new(group.repo.name()).strong());
                for wt in &group.worktrees {
                    let text = match wt.branch_name() {
                        Some(branch) if branch != wt.name() => {
                            format!("   {} · {branch}", wt.name())
                        }
                        _ => format!("   {}", wt.name()),
                    };
                    if ui
                        .selectable_label(form.worktree_id.as_ref() == Some(&wt.id), text)
                        .on_hover_text(&wt.path)
                        .clicked()
                    {
                        form.worktree_id = Some(wt.id.clone());
                        form.repo_id = Some(group.repo.id.clone());
                    }
                }
            }
        });
}

fn run_picker(ui: &mut egui::Ui, fleet: &SpawnFleet, form: &mut SpawnForm) {
    let selected = form
        .run_id
        .as_deref()
        .and_then(|id| fleet.runs.iter().find(|r| r.id == id))
        .map_or("pick a run", |r| r.label.as_str());
    egui::ComboBox::from_id_salt("spawn_run")
        .selected_text(selected)
        .width(420.0)
        .show_ui(ui, |ui| {
            if fleet.runs.is_empty() {
                ui.label(RichText::new("no runs").weak());
            }
            for run in &fleet.runs {
                if ui
                    .selectable_label(form.run_id.as_ref() == Some(&run.id), &run.label)
                    .clicked()
                {
                    form.run_id = Some(run.id.clone());
                }
            }
        });
}

fn terminal_picker(ui: &mut egui::Ui, fleet: &SpawnFleet, form: &mut SpawnForm) {
    let selected = form
        .terminal
        .as_deref()
        .and_then(|h| fleet.terminals.iter().find(|t| t.handle == h))
        .map_or("pick an agent terminal", |t| t.label.as_str());
    egui::ComboBox::from_id_salt("spawn_terminal")
        .selected_text(selected)
        .width(420.0)
        .show_ui(ui, |ui| {
            for t in &fleet.terminals {
                if ui
                    .selectable_label(form.terminal.as_ref() == Some(&t.handle), &t.label)
                    .on_hover_text(&t.handle)
                    .clicked()
                {
                    form.terminal = Some(t.handle.clone());
                }
            }
        });
}

/// The prompt box; true when ⌘↩ was pressed in it.
fn prompt_field(ui: &mut egui::Ui, prompt: &mut String) -> bool {
    ui.label("Initial prompt");
    let id = ui.make_persistent_id("spawn_prompt");
    let focused = ui.memory(|m| m.has_focus(id));
    let submit =
        focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    let response = ui.add(
        egui::TextEdit::multiline(prompt)
            .id(id)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .hint_text("What should the ant do? (⌘↩ to launch)"),
    );
    crate::ui::widgets::focus_ring(ui, &response);
    submit
}

fn job_view(ui: &mut egui::Ui, job: &SpawnJobView) {
    ui.separator();
    let dark = ui.visuals().dark_mode;
    let green = if dark {
        Color32::from_rgb(120, 200, 120)
    } else {
        Color32::from_rgb(30, 130, 50)
    };
    let amber = Color32::from_rgb(215, 160, 40);
    let error = ui.visuals().error_fg_color;
    for (step, status) in job.plan.steps.iter().zip(&job.statuses) {
        ui.horizontal_wrapped(|ui| {
            match status {
                StepStatus::Pending => {
                    ui.label(RichText::new("○").weak());
                }
                StepStatus::Running => {
                    ui.spinner();
                }
                StepStatus::Done(_) => {
                    ui.label(RichText::new("✔").color(green));
                }
                StepStatus::Warned(_) => {
                    ui.label(RichText::new("⚠").color(amber));
                }
                StepStatus::Skipped(_) => {
                    ui.label(RichText::new("–").weak());
                }
                StepStatus::Failed(_) => {
                    ui.label(RichText::new("✖").color(error));
                }
            }
            ui.label(step.title());
            let note = match status {
                StepStatus::Done(Some(n)) | StepStatus::Skipped(n) => Some(n),
                StepStatus::Warned(n) | StepStatus::Failed(n) => Some(n),
                _ => None,
            };
            if let Some(note) = note {
                let text = RichText::new(note).small();
                ui.label(match status {
                    StepStatus::Failed(_) => text.color(error),
                    StepStatus::Warned(_) => text.color(amber),
                    _ => text.weak(),
                });
            }
        });
    }
    match &job.result {
        None => {}
        Some(Ok(outcome)) => {
            let text = match (&outcome.context.handle, outcome.verified) {
                (Some(h), true) => format!("Started in {h}; it will be selected when it appears."),
                (Some(h), false) => format!("Launched in {h}, but the start is unconfirmed."),
                (None, _) => "Launched.".to_string(),
            };
            ui.label(RichText::new(text).color(if outcome.verified { green } else { amber }));
        }
        Some(Err(failure)) => {
            ui.colored_label(error, failure.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;
    use crate::orca::spawn::{parse_repo_list, parse_worktree_list};
    use crate::spawn::exec::SpawnOutcome;
    use crate::spawn::{SpawnContext, Step};

    const WT_ID: &str = "f2c8fcb1-8fa7-4763-8986-a597cba57cdd::/Users/gylleus/orca/workspaces/commerce-platform/hack-77-admin-speed";
    const REPO_ID: &str = "f2c8fcb1-8fa7-4763-8986-a597cba57cdd";

    fn catalog() -> Catalog {
        let repos = include_bytes!("../../tests/fixtures/orca/repo_list.json");
        let worktrees = include_bytes!("../../tests/fixtures/orca/worktree_list.json");
        Catalog {
            repos: parse_repo_list(repos).unwrap().repos,
            worktrees: parse_worktree_list(worktrees).unwrap().worktrees,
        }
    }

    fn fleet() -> SpawnFleet {
        SpawnFleet {
            runs: vec![RunChoice {
                id: "run_1".into(),
                label: "run_1 · ship it".into(),
                coordinator: Some("term_coord".into()),
            }],
            terminals: vec![TerminalChoice {
                handle: "term_w".into(),
                label: "worker · main".into(),
                worktree_id: Some(WT_ID.into()),
                worktree_path: Some("/wt".into()),
            }],
            agents: vec![LiveAgent {
                node: "term:term_w".into(),
                state: "working".into(),
                worktree_path: Some(
                    "/Users/gylleus/orca/workspaces/commerce-platform/hack-77-admin-speed".into(),
                ),
            }],
        }
    }

    fn form(mode: ModeName) -> SpawnForm {
        SpawnForm {
            mode,
            prompt: "Fix the flaky test".into(),
            ..SpawnForm::default()
        }
    }

    #[test]
    fn form_builds_requests_per_mode() {
        let (catalog, fleet) = (catalog(), fleet());
        let mut f = form(ModeName::Worktree);
        assert_eq!(f.request(&catalog, &fleet).unwrap_err(), "pick a worktree");
        f.worktree_id = Some(WT_ID.into());
        let req = f.request(&catalog, &fleet).unwrap();
        assert!(
            matches!(req.target, SpawnTarget::Worktree { ref worktree_id, .. } if worktree_id == WT_ID)
        );

        let mut f = form(ModeName::NewWorktree);
        f.repo_id = Some(REPO_ID.into());
        f.name = "fix-flake".into();
        f.model = " ".into();
        let req = f.request(&catalog, &fleet).unwrap();
        assert_eq!(req.model, None);
        assert_eq!(
            req.target,
            SpawnTarget::NewWorktree {
                repo_id: REPO_ID.into(),
                name: "fix-flake".into(),
                base_branch: None
            }
        );

        let mut f = form(ModeName::Worker);
        assert_eq!(f.request(&catalog, &fleet).unwrap_err(), "pick a run");
        f.run_id = Some("run_1".into());
        f.place = WorkerPlace::Terminal;
        f.terminal = Some("term_w".into());
        let plan = f.plan(&catalog, &fleet).unwrap();
        assert!(matches!(
            &plan.request.target,
            SpawnTarget::Worker { coordinator, placement: WorkerPlacement::Terminal { handle, .. }, .. }
                if coordinator == "term_coord" && handle == "term_w"
        ));
        f.model = "opus".into();
        assert!(
            f.plan(&catalog, &fleet)
                .unwrap_err()
                .contains("existing terminal")
        );
    }

    #[test]
    fn form_refuses_a_taken_name_unless_a_suffix_is_allowed() {
        let (catalog, fleet) = (catalog(), fleet());
        let mut f = form(ModeName::NewWorktree);
        f.repo_id = Some(REPO_ID.into());
        f.name = "hack-77-admin-speed".into();
        let e = f.plan(&catalog, &fleet).unwrap_err();
        assert!(e.contains("term:term_w (working)"), "{e}");
        f.allow_suffix = true;
        assert!(f.plan(&catalog, &fleet).is_ok());

        let mut f = form(ModeName::Worker);
        f.run_id = Some("run_1".into());
        f.repo_id = Some(REPO_ID.into());
        f.name = "hack-77-admin-speed".into();
        assert!(
            f.plan(&catalog, &fleet)
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn new_worktree_plans_show_the_startup_shell_close() {
        let mut f = form(ModeName::NewWorktree);
        f.repo_id = Some(REPO_ID.into());
        f.name = "fresh".into();
        let plan = f.plan(&catalog(), &fleet()).unwrap();
        assert_eq!(plan.steps.last(), Some(&Step::CloseStartupShell));
        assert!(plan.display_lines().last().unwrap().contains("--tab"));
    }

    #[test]
    fn opening_from_a_sidebar_repo_picks_it_and_its_main_worktree() {
        let mut dialog = SpawnDialog::default();
        dialog.open(Some("commerce-platform"));
        dialog.set_catalog(catalog());
        assert_eq!(dialog.form.repo_id.as_deref(), Some(REPO_ID));
        assert_eq!(
            dialog.form.worktree_id.as_deref(),
            Some(
                "f2c8fcb1-8fa7-4763-8986-a597cba57cdd::/Users/gylleus/repo/hackathon/commerce-platform"
            )
        );
        assert!(dialog.open);
    }

    fn draw(dialog: &mut SpawnDialog, frames: usize) -> (Option<SpawnPlan>, Vec<String>) {
        let ctx = egui::Context::default();
        let mut out = None;
        let mut texts = Vec::new();
        for _ in 0..frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 2400.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                out = dialog.show(ui.ctx(), &fleet()).or(out.take());
            });
            output.textures_delta.clear();
            texts = shape_texts(&output);
        }
        (out, texts)
    }

    fn shape_texts(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn dialog_draws_headless_with_planned_commands_and_launches() {
        let mut dialog = SpawnDialog::default();
        dialog.open(None);
        dialog.set_catalog(catalog());
        dialog.form.worktree_id = Some(WT_ID.into());
        dialog.form.prompt = "Fix the flaky test".into();
        let (launched, texts) = draw(&mut dialog, 3);
        assert!(launched.is_none());
        assert!(texts.iter().any(|t| t == "New ant"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Planned commands"));
        assert!(
            texts.iter().any(|t| t.contains("orca terminal create")
                && t.contains(r#"--command="claude 'Fix the flaky test'""#)),
            "{texts:?}"
        );

        let plan =
            crate::ui::test_util::click_label("Launch", |ui| dialog.show(ui.ctx(), &fleet()))
                .expect("Launch returns the plan");
        assert!(matches!(plan.steps[0], Step::CreateTerminal { .. }));
    }

    #[test]
    fn dialog_shows_progress_and_result_of_its_job() {
        let mut dialog = SpawnDialog::default();
        dialog.open(None);
        dialog.set_catalog(catalog());
        dialog.form.worktree_id = Some(WT_ID.into());
        let plan = dialog.form.plan(&catalog(), &fleet()).unwrap();
        dialog.started(4, plan);
        dialog.on_event(&SpawnEvent::Step {
            job: 4,
            index: 0,
            status: StepStatus::Done(Some("term_new".into())),
        });
        dialog.on_event(&SpawnEvent::Step {
            job: 99,
            index: 1,
            status: StepStatus::Failed("other job".into()),
        });
        assert!(dialog.job().unwrap().running());
        let (_, texts) = draw(&mut dialog, 3);
        assert!(texts.iter().any(|t| t == "term_new"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == "other job"));
        dialog.on_event(&SpawnEvent::Done {
            job: 4,
            result: Ok(SpawnOutcome {
                context: SpawnContext {
                    handle: Some("term_new".into()),
                    ..SpawnContext::default()
                },
                verified: true,
                notes: vec![],
            }),
        });
        assert!(!dialog.job().unwrap().running());
        let (_, texts) = draw(&mut dialog, 2);
        assert!(
            texts.iter().any(|t| t.starts_with("Started in term_new")),
            "{texts:?}"
        );
        // Reopening after a finished job starts fresh.
        dialog.open(None);
        assert!(dialog.job().is_none());
    }
}
