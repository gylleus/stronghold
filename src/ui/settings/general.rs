//! The General section: whether a spawn focuses the new dwarf, where Stronghold keeps its files, the
//! control socket, the build and the playbook, with links to the docs.

use std::path::{Path, PathBuf};

use eframe::egui;

/// Facts the General section shows; the app gathers them while the window is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneralInfo {
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    /// The control API is serving on `socket_path`.
    pub socket_listening: bool,
    pub version: &'static str,
    /// Git commit the binary was built from, when the build recorded it.
    pub commit: Option<&'static str>,
    /// The playbook dir, or why none was found.
    pub playbook_dir: Result<PathBuf, String>,
    /// The playbook's current version (`1b0b685 (clean) · hash 3f9a1c2`).
    pub playbook_version: Option<String>,
}

impl GeneralInfo {
    /// Version and commit of this build.
    pub fn build() -> (&'static str, Option<&'static str>) {
        (
            env!("CARGO_PKG_VERSION"),
            option_env!("STRONGHOLD_GIT_COMMIT").filter(|c| !c.is_empty()),
        )
    }
}

pub const FOCUS_NEW_LABEL: &str = "Focus new dwarves when they're spawned";

/// Docs shipped in the source tree: `(title, path relative to the crate root)`.
pub const DOCS: &[(&str, &str)] = &[
    ("README", "README.md"),
    ("Plan / spec", "PLAN.md"),
    ("Control API", "docs/control-api.md"),
    ("MCP tools", "docs/mcp.md"),
];

/// Draws the section; returns the new "Focus new dwarves" value when it was toggled.
pub(super) fn section(
    ui: &mut egui::Ui,
    info: &GeneralInfo,
    settings_path: Option<&Path>,
    mut focus_new: bool,
) -> Option<bool> {
    let toggled = ui
        .checkbox(&mut focus_new, FOCUS_NEW_LABEL)
        .on_hover_text(
            "Select and pan to a new dwarf's node once it appears, keeping the current zoom. \
             Off: spawns leave the selection and the view alone.",
        )
        .changed();
    ui.separator();
    egui::Grid::new("general_settings")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            path_row(ui, "Data dir", &info.data_dir);
            ui.label("Control socket");
            ui.vertical(|ui| {
                path_label(ui, &info.socket_path);
                if info.socket_listening {
                    ui.colored_label(egui::Color32::from_rgb(0x4c, 0xaf, 0x50), "● listening");
                } else {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "● not listening (another Stronghold may hold it; see the log)",
                    );
                }
            });
            ui.end_row();
            ui.label("Settings file");
            match settings_path {
                Some(p) => path_label(ui, p),
                None => ui.weak("none (no config directory)"),
            };
            ui.end_row();
            ui.label("Version");
            match info.commit {
                Some(commit) => ui.label(format!("{} ({commit})", info.version)),
                None => ui.label(info.version),
            };
            ui.end_row();
            ui.label("Playbook");
            ui.vertical(|ui| {
                match &info.playbook_dir {
                    Ok(dir) => path_label(ui, dir),
                    Err(e) => ui.colored_label(ui.visuals().error_fg_color, e),
                };
                if let Some(version) = &info.playbook_version {
                    ui.weak(version);
                }
            });
            ui.end_row();
        });
    ui.separator();
    ui.label("Docs");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    ui.horizontal_wrapped(|ui| {
        for (title, rel) in DOCS {
            let path = root.join(rel);
            if path.is_file() {
                ui.hyperlink_to(*title, format!("file://{}", path.display()));
            }
        }
    });
    toggled.then_some(focus_new)
}

fn path_row(ui: &mut egui::Ui, label: &str, path: &Path) {
    ui.label(label);
    path_label(ui, path);
    ui.end_row();
}

/// A path in monospace that can be selected and copied.
fn path_label(ui: &mut egui::Ui, path: &Path) -> egui::Response {
    ui.add(
        egui::Label::new(egui::RichText::new(path.display().to_string()).monospace())
            .selectable(true),
    )
}
