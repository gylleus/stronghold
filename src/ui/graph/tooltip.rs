//! Hover tooltip for a graph node.

use eframe::egui::{RichText, TextStyle, Ui};

use crate::claude::Usage;
use crate::model::{Node, NodeKind, NodeState};

/// Longest preview line shown in the tooltip, in characters.
const PREVIEW_CHARS: usize = 100;

pub fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Coordinator => "coordinator",
        NodeKind::Attendant => "attendant",
        NodeKind::Worker => "worker",
        NodeKind::Terminal => "terminal",
        NodeKind::Envoy => "envoy",
        NodeKind::Subagent => "subagent",
        NodeKind::Queen => "queen",
    }
}

/// The kind as the tooltip names it: agent sessions of a task are ants.
fn ant_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Coordinator => "coordinator ant",
        NodeKind::Worker => "worker ant",
        NodeKind::Terminal => "ant (loose terminal)",
        other => kind_label(other),
    }
}

pub fn state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Working => "working",
        NodeState::Idle => "idle",
        NodeState::NeedsInput => "needs input",
        NodeState::Stalled => "stalled",
        NodeState::Done => "done",
        NodeState::Failed => "failed",
    }
}

/// "just now", "42s ago", "2m ago", "3h ago", "5d ago".
pub fn format_ago(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1_000;
    match secs {
        0..5 => "just now".to_string(),
        5..60 => format!("{secs}s ago"),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// First non-blank line of a preview, trimmed and capped at [`PREVIEW_CHARS`].
pub fn preview_line(preview: &str) -> Option<String> {
    let line = preview.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.chars().count() <= PREVIEW_CHARS {
        return Some(line.to_string());
    }
    let mut cut: String = line.chars().take(PREVIEW_CHARS - 1).collect();
    cut.push('…');
    Some(cut)
}

pub fn node_tooltip(
    ui: &mut Ui,
    node: &Node,
    run_objective: Option<&str>,
    repo: Option<&str>,
    usage: Option<&Usage>,
    now_ms: i64,
) {
    ui.set_max_width(420.0);
    ui.label(RichText::new(&node.label).strong());
    if node.kind == NodeKind::Queen {
        ui.label(format!("fleet · {}", state_label(node.state)));
        if let Some(counts) = &node.preview {
            ui.label(counts);
        }
        ui.label(RichText::new("Click for the fleet summary and broadcast").weak());
        return;
    }
    ui.label(crate::ui::icons::with_icon(
        ui,
        crate::ui::icons::kind_icon(node.kind),
        &format!(
            "{} · {}",
            ant_kind_label(node.kind),
            state_label(node.state)
        ),
        TextStyle::Body,
        None,
    ));
    if let Some(at) = node.parked_at_ms {
        ui.label(format!(
            "Parked {}: asleep, session and worktree kept",
            format_ago(now_ms, at)
        ));
        ui.label(RichText::new("Right-click → Wake, or send it a message, to continue").weak());
    }
    // A parked node's region is the resting area; name its own repo.
    let own_repo = node
        .is_parked()
        .then(|| {
            node.worktree_path
                .as_deref()
                .and_then(crate::model::repo_of)
        })
        .flatten();
    if let Some(repo) = own_repo.as_deref().or(repo) {
        ui.label(format!("Repo: {repo}"));
    }
    if let Some(run_id) = &node.run_id {
        ui.label(format!("Run: {}", run_objective.unwrap_or(run_id)));
    }
    if node.kind == NodeKind::Attendant {
        ui.label(RichText::new("Forwards the run's bus events to its coordinator").weak());
    }
    if let Some(agent) = &node.agent {
        ui.label(format!("Agent: {agent}"));
    }
    if let Some(branch) = &node.branch {
        ui.label(format!("Branch: {branch}"));
    }
    match node.last_output_ms {
        Some(at) => ui.label(format!("Last output {}", format_ago(now_ms, at))),
        None => ui.label("No output yet"),
    };
    if let Some(usage) = usage {
        ui.label(format!("Tokens: {}", usage.label()));
        ui.label(RichText::new(usage.tokens.breakdown()).weak());
    }
    if let Some(line) = node.preview.as_deref().and_then(preview_line) {
        ui.label(RichText::new(line).monospace().weak());
    }
}

/// The "coordinator gone" line of an attendant whose run has no live coordinator.
pub fn coordinator_gone_note(ui: &mut Ui) {
    ui.colored_label(
        ui.visuals().warn_fg_color,
        "Coordinator gone: no live terminal coordinates this run; events wait at the attendant",
    );
}
