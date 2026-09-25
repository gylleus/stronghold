//! Node state derivation: [`derive_state`] is the single place that decides a node's
//! [`NodeState`] from its terminal, its dispatch and the detector/whip signals.

use crate::orca::{DispatchStatus, Terminal, Worker, WorkerState, WorkerTerminalState};

use super::NodeState;

/// Output newer than this counts as Working.
pub const WORKING_WINDOW_MS: i64 = 30_000;

/// Everything [`derive_state`] may look at for one node.
#[derive(Debug, Clone, Copy)]
pub struct StateInputs<'a> {
    /// The node's terminal as listed by `terminal list`; `None` when it is not listed.
    pub terminal: Option<&'a Terminal>,
    /// The dispatch the node represents, for worker nodes.
    pub worker: Option<&'a Worker>,
    pub now_ms: i64,
    /// Verdict of the screen detector: `Some(true)` waits for the user, `Some(false)` does
    /// not, `None` falls back to [`preview_needs_input`] on the terminal preview.
    pub needs_input: Option<bool>,
    /// Set by the whip when the node has made no progress for too long.
    pub stalled: bool,
}

impl<'a> StateInputs<'a> {
    /// Inputs with no detector verdict and not stalled.
    pub fn new(terminal: Option<&'a Terminal>, worker: Option<&'a Worker>, now_ms: i64) -> Self {
        Self {
            terminal,
            worker,
            now_ms,
            needs_input: None,
            stalled: false,
        }
    }
}

/// What one Orca worker field says about the dispatch's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Still running (or no opinion); the terminal decides.
    Open,
    /// Ended successfully or was cleaned up without a failure signal.
    Done,
    /// Ended badly.
    Failed,
}

fn worker_state_verdict(state: &WorkerState) -> Verdict {
    match state {
        WorkerState::Ready | WorkerState::Unsupervised => Verdict::Open,
        WorkerState::Succeeded => Verdict::Done,
        // Stopped by the supervisor before finishing, or a stop that was never confirmed.
        WorkerState::Failed | WorkerState::Stopped | WorkerState::StopUnknown => Verdict::Failed,
        WorkerState::Other(_) => Verdict::Open,
    }
}

fn dispatch_status_verdict(status: &DispatchStatus) -> Verdict {
    match status {
        DispatchStatus::Dispatched => Verdict::Open,
        DispatchStatus::Completed => Verdict::Done,
        DispatchStatus::Failed | DispatchStatus::CircuitBroken => Verdict::Failed,
        DispatchStatus::Other(_) => Verdict::Open,
    }
}

fn terminal_state_verdict(state: &WorkerTerminalState) -> Verdict {
    match state {
        // Resource accounting only: says nothing about how the work went.
        WorkerTerminalState::Active
        | WorkerTerminalState::Reclaimable
        | WorkerTerminalState::Retained => Verdict::Open,
        // The terminal is (being) handed back, so the dispatch is over.
        WorkerTerminalState::ReleasePending
        | WorkerTerminalState::ReleaseUnknown
        | WorkerTerminalState::Released => Verdict::Done,
        WorkerTerminalState::Other(_) => Verdict::Open,
    }
}

/// Combined verdict of a worker's fields: any failure wins, then any sign of completion.
fn worker_verdict(worker: &Worker) -> Verdict {
    let verdicts = [
        worker.worker_state.as_ref().map(worker_state_verdict),
        worker.dispatch_status.as_ref().map(dispatch_status_verdict),
        worker.terminal_state.as_ref().map(terminal_state_verdict),
    ];
    let has = |v| verdicts.contains(&Some(v));
    if has(Verdict::Failed) {
        Verdict::Failed
    } else if has(Verdict::Done) {
        Verdict::Done
    } else {
        Verdict::Open
    }
}

/// A terminal that is listed, connected and not orphaned.
fn is_live(terminal: &Terminal) -> bool {
    terminal.connected && !terminal.orphaned
}

/// The single place that decides a node's [`NodeState`].
///
/// Precedence, first match wins:
/// 1. **Failed**: the worker's fields report a failure (`workerState` failed/stopped/
///    stop_unknown, `dispatchStatus` failed/circuit_broken).
/// 2. **Done**: the worker's fields report completion (`workerState` succeeded,
///    `dispatchStatus` completed, `terminalState` release_pending/release_unknown/released).
/// 3. Not live (terminal missing, disconnected or orphaned): **Failed** for a worker whose
///    dispatch is still open, **Idle** for anything else.
/// 4. **NeedsInput**: the detector says so, or (without a detector verdict) the preview
///    shows a prompt per [`preview_needs_input`].
/// 5. **Stalled**: the whip flagged the node.
/// 6. **Working**: output within [`WORKING_WINDOW_MS`].
/// 7. **Idle**.
pub fn derive_state(inputs: &StateInputs<'_>) -> NodeState {
    if let Some(worker) = inputs.worker {
        match worker_verdict(worker) {
            Verdict::Failed => return NodeState::Failed,
            Verdict::Done => return NodeState::Done,
            Verdict::Open => {}
        }
    }
    let Some(terminal) = inputs.terminal.filter(|t| is_live(t)) else {
        return if inputs.worker.is_some() {
            NodeState::Failed
        } else {
            NodeState::Idle
        };
    };
    let needs_input = inputs
        .needs_input
        .unwrap_or_else(|| terminal.preview.as_deref().is_some_and(preview_needs_input));
    if needs_input {
        return NodeState::NeedsInput;
    }
    if inputs.stalled {
        return NodeState::Stalled;
    }
    let last_output = terminal.last_output_at.as_ref().and_then(|t| t.epoch_ms());
    match last_output {
        Some(ms) if inputs.now_ms - ms < WORKING_WINDOW_MS => NodeState::Working,
        _ => NodeState::Idle,
    }
}

/// Lower-cased fragments that only appear while an agent waits on a choice or confirmation.
const PROMPT_MARKERS: &[&str] = &[
    // Claude Code permission prompts.
    "do you want to proceed?",
    "do you want to make this edit",
    "do you want to create",
    "do you want to allow",
    "❯ 1. yes",
    "esc to cancel · tab to amend",
    // Claude Code AskUserQuestion / selection menus.
    "enter to select · ↑/↓ to navigate",
    // Codex approval and selection prompts.
    "press enter to confirm",
    "would you like to run the following command",
    "would you like to make the following edits",
    "› 1. yes",
    "allow command?",
    // Generic shell confirmations.
    "[y/n]",
    "(y/n)",
];

/// Cheap heuristic: does a terminal preview show an agent waiting for the user?
pub fn preview_needs_input(preview: &str) -> bool {
    let preview = preview.to_lowercase();
    PROMPT_MARKERS.iter().any(|m| preview.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::Timestamp;

    const NOW: i64 = 1_790_000_000_000;
    const RECENT: i64 = NOW - 5_000;
    const OLD: i64 = NOW - 5 * 60_000;
    const PROMPT: &str = "Do you want to proceed?\n❯ 1. Yes\n  2. No";

    /// `(workerState, dispatchStatus, terminalState, expected)`.
    type WorkerCase = (
        Option<WorkerState>,
        Option<DispatchStatus>,
        Option<WorkerTerminalState>,
        NodeState,
    );
    /// `(worker, last output, preview, needs_input, stalled, expected)`.
    type LiveCase<'a> = (
        Option<&'a Worker>,
        Option<i64>,
        &'a str,
        Option<bool>,
        bool,
        NodeState,
    );

    fn terminal(last_output_ms: Option<i64>, preview: &str) -> Terminal {
        Terminal {
            handle: "term_x".into(),
            connected: true,
            last_output_at: last_output_ms.map(Timestamp::Millis),
            preview: Some(preview.into()),
            ..Default::default()
        }
    }

    fn worker(
        worker_state: Option<WorkerState>,
        dispatch_status: Option<DispatchStatus>,
        terminal_state: Option<WorkerTerminalState>,
    ) -> Worker {
        Worker {
            dispatch_id: "ctx_x".into(),
            task_id: None,
            run_id: None,
            worker_state,
            dispatch_status,
            agent_terminal_handle: Some("term_x".into()),
            terminal_state,
            resource: None,
        }
    }

    /// `workerState|dispatchStatus|terminalState` combinations seen in a real
    /// `orca orchestration worker-list`, plus unknown values.
    #[test]
    fn worker_fields_map_to_states() {
        use DispatchStatus as D;
        use WorkerState as W;
        use WorkerTerminalState as T;
        let live = terminal(Some(OLD), "");
        let cases: Vec<WorkerCase> = vec![
            (
                Some(W::Failed),
                Some(D::Failed),
                Some(T::Reclaimable),
                NodeState::Failed,
            ),
            (
                Some(W::Failed),
                Some(D::Failed),
                Some(T::ReleaseUnknown),
                NodeState::Failed,
            ),
            (
                Some(W::Failed),
                Some(D::Failed),
                Some(T::Released),
                NodeState::Failed,
            ),
            (
                Some(W::Failed),
                Some(D::Failed),
                Some(T::Retained),
                NodeState::Failed,
            ),
            (
                Some(W::Ready),
                Some(D::Dispatched),
                Some(T::Active),
                NodeState::Idle,
            ),
            (
                Some(W::Ready),
                Some(D::Dispatched),
                Some(T::Retained),
                NodeState::Idle,
            ),
            (
                Some(W::StopUnknown),
                Some(D::Failed),
                Some(T::Active),
                NodeState::Failed,
            ),
            (
                Some(W::StopUnknown),
                Some(D::Failed),
                Some(T::Retained),
                NodeState::Failed,
            ),
            (
                Some(W::Stopped),
                Some(D::Failed),
                Some(T::Released),
                NodeState::Failed,
            ),
            (
                Some(W::Succeeded),
                Some(D::Completed),
                Some(T::Reclaimable),
                NodeState::Done,
            ),
            (
                Some(W::Succeeded),
                Some(D::Completed),
                Some(T::ReleaseUnknown),
                NodeState::Done,
            ),
            (
                Some(W::Succeeded),
                Some(D::Completed),
                Some(T::Released),
                NodeState::Done,
            ),
            (
                Some(W::Succeeded),
                Some(D::Completed),
                Some(T::Retained),
                NodeState::Done,
            ),
            (
                Some(W::Unsupervised),
                Some(D::CircuitBroken),
                Some(T::Retained),
                NodeState::Failed,
            ),
            (
                Some(W::Unsupervised),
                Some(D::Completed),
                Some(T::Retained),
                NodeState::Done,
            ),
            (
                Some(W::Unsupervised),
                Some(D::Dispatched),
                Some(T::Retained),
                NodeState::Idle,
            ),
            (
                Some(W::Unsupervised),
                Some(D::Failed),
                Some(T::Retained),
                NodeState::Failed,
            ),
            // Single fields and unknowns.
            (Some(W::Stopped), None, None, NodeState::Failed),
            (None, None, Some(T::ReleasePending), NodeState::Done),
            (
                None,
                Some(D::Dispatched),
                Some(T::Released),
                NodeState::Done,
            ),
            (
                Some(W::Failed),
                Some(D::Completed),
                Some(T::Released),
                NodeState::Failed,
            ),
            (None, None, None, NodeState::Idle),
            (
                Some(W::Other("paused".into())),
                Some(D::Other("queued".into())),
                Some(T::Other("parked".into())),
                NodeState::Idle,
            ),
        ];
        for (ws, ds, ts, expected) in cases {
            let w = worker(ws.clone(), ds.clone(), ts.clone());
            let got = derive_state(&StateInputs::new(Some(&live), Some(&w), NOW));
            assert_eq!(got, expected, "{ws:?} | {ds:?} | {ts:?}");
        }
    }

    #[test]
    fn liveness() {
        let open = worker(
            Some(WorkerState::Ready),
            Some(DispatchStatus::Dispatched),
            None,
        );
        let done = worker(Some(WorkerState::Succeeded), None, None);
        let live = terminal(Some(RECENT), PROMPT);
        let disconnected = Terminal {
            connected: false,
            ..live.clone()
        };
        let orphaned = Terminal {
            orphaned: true,
            ..live.clone()
        };
        let cases: Vec<(Option<&Terminal>, Option<&Worker>, NodeState)> = vec![
            (None, Some(&open), NodeState::Failed),
            (Some(&disconnected), Some(&open), NodeState::Failed),
            (Some(&orphaned), Some(&open), NodeState::Failed),
            (None, Some(&done), NodeState::Done),
            (Some(&disconnected), Some(&done), NodeState::Done),
            (None, None, NodeState::Idle),
            (Some(&disconnected), None, NodeState::Idle),
            (Some(&orphaned), None, NodeState::Idle),
            (Some(&live), Some(&open), NodeState::NeedsInput),
            (Some(&live), None, NodeState::NeedsInput),
        ];
        for (i, (t, w, expected)) in cases.into_iter().enumerate() {
            let inputs = StateInputs {
                stalled: true,
                ..StateInputs::new(t, w, NOW)
            };
            assert_eq!(derive_state(&inputs), expected, "case {i}");
        }
    }

    #[test]
    fn precedence_of_live_signals() {
        let failed = worker(None, Some(DispatchStatus::Failed), None);
        let done = worker(None, Some(DispatchStatus::Completed), None);
        let open = worker(None, Some(DispatchStatus::Dispatched), None);
        #[rustfmt::skip]
        let cases: Vec<LiveCase> = vec![
            (Some(&failed), Some(RECENT), PROMPT, Some(true), true, NodeState::Failed),
            (Some(&done), Some(RECENT), PROMPT, Some(true), true, NodeState::Done),
            (Some(&open), Some(RECENT), "", Some(true), true, NodeState::NeedsInput),
            (None, Some(RECENT), PROMPT, None, true, NodeState::NeedsInput),
            (None, Some(RECENT), PROMPT, Some(false), false, NodeState::Working),
            (None, Some(OLD), "", Some(true), false, NodeState::NeedsInput),
            (None, Some(RECENT), "", None, true, NodeState::Stalled),
            (None, Some(OLD), "", Some(false), true, NodeState::Stalled),
            (Some(&open), Some(RECENT), "", None, false, NodeState::Working),
            (None, Some(NOW - WORKING_WINDOW_MS + 1), "", None, false, NodeState::Working),
            (None, Some(NOW - WORKING_WINDOW_MS), "", None, false, NodeState::Idle),
            (None, Some(OLD), "", None, false, NodeState::Idle),
            (None, None, "", None, false, NodeState::Idle),
        ];
        for (i, (w, last, preview, needs_input, stalled, expected)) in cases.into_iter().enumerate()
        {
            let t = terminal(last, preview);
            let inputs = StateInputs {
                needs_input,
                stalled,
                ..StateInputs::new(Some(&t), w, NOW)
            };
            assert_eq!(derive_state(&inputs), expected, "case {i}");
        }
    }

    /// Previews and screen tails captured from real `orca terminal list` / `terminal read`.
    #[test]
    fn preview_prompts_are_detected() {
        let prompts = [
            // Codex rate-limit model switch menu.
            "about switching models.   Press enter to confirm or esc to go back   \
             › Ask Codex to do anything   gpt-5.6-luna medium · ~/orca/workspaces/depict/x",
            // Claude AskUserQuestion menu.
            "  4. Type something.\n  5. Chat about this\n\
             Enter to select · ↑/↓ to navigate · Esc to cancel",
            // Claude permission prompt.
            " Bash command\n   cargo test\n Do you want to proceed?\n❯ 1. Yes\n  \
             2. Yes, and don't ask again for cargo test commands\n  \
             3. No, and tell Claude what to do differently (esc)",
            " Do you want to make this edit to state.rs?\n❯ 1. Yes",
            "Esc to cancel · Tab to amend · ctrl+e to explain",
            // Codex command approval.
            "Would you like to run the following command?\n  $ cargo build\n\
             › 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\n\
             Press enter to confirm or esc to cancel",
            "Overwrite existing file? [y/N]",
        ];
        for p in prompts {
            assert!(preview_needs_input(p), "missed: {p}");
        }
    }

    #[test]
    fn ordinary_previews_are_not_prompts() {
        let previews = [
            "a prompt. Replace that * with the exact value you mean, or only use * after \
             the subcommand.\n❯ Baked for 2m 4s · done 1:37 PM",
            "../../.claude/settings.json): Bash(rg --files -g '*.test.ts' src) has a wildcard \
             before the rest of the command, so it also matches any options inserted at that \
             position and approves them without a prompt.\n← for agents",
            "⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
            "────────────────────────────────\n❯",
            "new task? /clear to save 680.7k tokens",
            "direnv: error /Users/x/.envrc is blocked. Run `direnv allow` to approve its content\n\
             workspaces/realtime/x  gylleus/x ✔             7d1h",
            "⚠ MCP startup incomplete (failed: cloudflare-observability)  \
             › Ask Codex to do anything   gpt-6-astra high · ~/orca/workspaces/x",
            "Resume this session with:\nclaude --resume 742c2274-c303-4c79-bcd5-46a66143ab40",
            "3:39:55 PM [vite] (client) hmr update /src/ui/LabView.tsx",
            "",
        ];
        for p in previews {
            assert!(!preview_needs_input(p), "false positive: {p}");
        }
    }
}
