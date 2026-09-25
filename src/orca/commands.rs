//! Typed wrappers for the `orca` commands Myrstack uses.
//!
//! Each command has a pure argument builder (`*_args`) and a pure parser
//! (`parse_*`) over the raw stdout bytes; the [`OrcaClient`] methods glue the two
//! together around a process spawn.

use serde::Deserialize;

use super::client::{OrcaClient, OrcaError, parse_result};
use super::types::{
    InboxMessage, OutgoingMessage, Run, SendResult, Task, TerminalFocus, TerminalInput,
    TerminalList, TerminalRead, TerminalSend, WorkerList,
};

/// Formats a value flag as `--name=value`. Orca's parser treats a separate
/// token starting with `--` as a new flag, so the `=` form is the only one that
/// passes arbitrary text (e.g. `--text=--help`) through intact.
fn flag(name: &str, value: impl std::fmt::Display) -> String {
    format!("--{name}={value}")
}

fn push_opt(args: &mut Vec<String>, name: &str, value: Option<impl std::fmt::Display>) {
    if let Some(value) = value {
        args.push(flag(name, value));
    }
}

fn base(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[derive(Deserialize)]
struct TerminalReadResult {
    terminal: TerminalRead,
}

#[derive(Deserialize)]
struct TerminalSendResult {
    #[serde(default)]
    send: TerminalSend,
}

#[derive(Deserialize)]
struct TerminalFocusResult {
    #[serde(default)]
    focus: TerminalFocus,
}

#[derive(Deserialize)]
struct RunListResult {
    #[serde(default)]
    runs: Option<Vec<Run>>,
}

#[derive(Deserialize)]
struct TaskListResult {
    #[serde(default)]
    tasks: Option<Vec<Task>>,
}

#[derive(Deserialize)]
struct InboxResult {
    #[serde(default)]
    messages: Option<Vec<InboxMessage>>,
}

// ---------------------------------------------------------------------------
// Argument builders

pub fn terminal_list_args() -> Vec<String> {
    base(&["terminal", "list"])
}

pub fn terminal_read_screen_args(handle: &str) -> Vec<String> {
    let mut args = base(&["terminal", "read"]);
    args.push(flag("terminal", handle));
    args.push("--screen".into());
    args
}

pub fn terminal_read_args(handle: &str, cursor: Option<u64>, limit: Option<u64>) -> Vec<String> {
    let mut args = base(&["terminal", "read"]);
    args.push(flag("terminal", handle));
    push_opt(&mut args, "cursor", cursor);
    push_opt(&mut args, "limit", limit);
    args
}

pub fn terminal_send_args(handle: &str, input: &TerminalInput) -> Vec<String> {
    let mut args = base(&["terminal", "send"]);
    args.push(flag("terminal", handle));
    push_opt(&mut args, "text", input.text.as_deref());
    if input.enter {
        args.push("--enter".into());
    }
    if input.interrupt {
        args.push("--interrupt".into());
    }
    args
}

pub fn terminal_switch_args(handle: &str) -> Vec<String> {
    let mut args = base(&["terminal", "switch"]);
    args.push(flag("terminal", handle));
    args
}

pub fn run_list_args() -> Vec<String> {
    base(&["orchestration", "run-list"])
}

pub fn worker_list_args(run: Option<&str>) -> Vec<String> {
    let mut args = base(&["orchestration", "worker-list"]);
    push_opt(&mut args, "run", run);
    args
}

pub fn task_list_args(run: &str) -> Vec<String> {
    let mut args = base(&["orchestration", "task-list"]);
    args.push(flag("run", run));
    args
}

pub fn inbox_args(limit: Option<u64>, terminal: Option<&str>) -> Vec<String> {
    let mut args = base(&["orchestration", "inbox"]);
    push_opt(&mut args, "limit", limit);
    push_opt(&mut args, "terminal", terminal);
    args
}

pub fn orchestration_send_args(msg: &OutgoingMessage) -> Vec<String> {
    let mut args = base(&["orchestration", "send"]);
    args.push(flag("subject", &msg.subject));
    push_opt(&mut args, "to", msg.to.as_deref());
    push_opt(&mut args, "run", msg.run.as_deref());
    push_opt(&mut args, "from", msg.from.as_deref());
    push_opt(&mut args, "body", msg.body.as_deref());
    push_opt(&mut args, "type", msg.message_type.as_deref());
    push_opt(&mut args, "priority", msg.priority.as_deref());
    push_opt(&mut args, "thread-id", msg.thread_id.as_deref());
    push_opt(&mut args, "payload", msg.payload.as_deref());
    push_opt(&mut args, "task-id", msg.task_id.as_deref());
    push_opt(&mut args, "dispatch-id", msg.dispatch_id.as_deref());
    push_opt(&mut args, "outcome", msg.outcome.as_deref());
    if !msg.files_modified.is_empty() {
        args.push(flag("files-modified", msg.files_modified.join(",")));
    }
    push_opt(&mut args, "report-path", msg.report_path.as_deref());
    push_opt(&mut args, "phase", msg.phase.as_deref());
    push_opt(&mut args, "retry-request", msg.retry_request.as_deref());
    args
}

// ---------------------------------------------------------------------------
// Parsers (pure, over the raw stdout of `orca … --json`)

pub fn parse_terminal_list(stdout: &[u8]) -> Result<TerminalList, OrcaError> {
    parse_result(stdout)
}

pub fn parse_terminal_read(stdout: &[u8]) -> Result<TerminalRead, OrcaError> {
    parse_result::<TerminalReadResult>(stdout).map(|r| r.terminal)
}

pub fn parse_terminal_send(stdout: &[u8]) -> Result<TerminalSend, OrcaError> {
    parse_result::<TerminalSendResult>(stdout).map(|r| r.send)
}

pub fn parse_terminal_switch(stdout: &[u8]) -> Result<TerminalFocus, OrcaError> {
    parse_result::<TerminalFocusResult>(stdout).map(|r| r.focus)
}

pub fn parse_run_list(stdout: &[u8]) -> Result<Vec<Run>, OrcaError> {
    parse_result::<RunListResult>(stdout).map(|r| r.runs.unwrap_or_default())
}

pub fn parse_worker_list(stdout: &[u8]) -> Result<WorkerList, OrcaError> {
    parse_result(stdout)
}

pub fn parse_task_list(stdout: &[u8]) -> Result<Vec<Task>, OrcaError> {
    parse_result::<TaskListResult>(stdout).map(|r| r.tasks.unwrap_or_default())
}

pub fn parse_inbox(stdout: &[u8]) -> Result<Vec<InboxMessage>, OrcaError> {
    parse_result::<InboxResult>(stdout).map(|r| r.messages.unwrap_or_default())
}

pub fn parse_orchestration_send(stdout: &[u8]) -> Result<SendResult, OrcaError> {
    parse_result(stdout)
}

// ---------------------------------------------------------------------------
// Client methods

impl OrcaClient {
    /// `orca terminal list`.
    pub fn terminal_list(&self) -> Result<TerminalList, OrcaError> {
        self.call(&terminal_list_args())
    }

    /// `orca terminal read --screen`: the currently rendered frame.
    pub fn terminal_read_screen(&self, handle: &str) -> Result<TerminalRead, OrcaError> {
        self.call::<TerminalReadResult, _>(&terminal_read_screen_args(handle))
            .map(|r| r.terminal)
    }

    /// `orca terminal read` of accumulated output, optionally after `cursor`.
    pub fn terminal_read(
        &self,
        handle: &str,
        cursor: Option<u64>,
        limit: Option<u64>,
    ) -> Result<TerminalRead, OrcaError> {
        self.call::<TerminalReadResult, _>(&terminal_read_args(handle, cursor, limit))
            .map(|r| r.terminal)
    }

    /// `orca terminal send`.
    pub fn terminal_send(
        &self,
        handle: &str,
        input: &TerminalInput,
    ) -> Result<TerminalSend, OrcaError> {
        self.call::<TerminalSendResult, _>(&terminal_send_args(handle, input))
            .map(|r| r.send)
    }

    /// `orca terminal switch`: bring the terminal's tab to the front in Orca.
    pub fn terminal_switch(&self, handle: &str) -> Result<TerminalFocus, OrcaError> {
        self.call::<TerminalFocusResult, _>(&terminal_switch_args(handle))
            .map(|r| r.focus)
    }

    /// `orca orchestration run-list`.
    pub fn run_list(&self) -> Result<Vec<Run>, OrcaError> {
        self.call::<RunListResult, _>(&run_list_args())
            .map(|r| r.runs.unwrap_or_default())
    }

    /// `orca orchestration worker-list`, optionally for one run.
    pub fn worker_list(&self, run: Option<&str>) -> Result<WorkerList, OrcaError> {
        self.call(&worker_list_args(run))
    }

    /// `orca orchestration task-list --run <id>`.
    pub fn task_list(&self, run: &str) -> Result<Vec<Task>, OrcaError> {
        self.call::<TaskListResult, _>(&task_list_args(run))
            .map(|r| r.tasks.unwrap_or_default())
    }

    /// `orca orchestration inbox`, newest first.
    pub fn inbox(
        &self,
        limit: Option<u64>,
        terminal: Option<&str>,
    ) -> Result<Vec<InboxMessage>, OrcaError> {
        self.call::<InboxResult, _>(&inbox_args(limit, terminal))
            .map(|r| r.messages.unwrap_or_default())
    }

    /// `orca orchestration send`.
    pub fn orchestration_send(&self, msg: &OutgoingMessage) -> Result<SendResult, OrcaError> {
        self.call(&orchestration_send_args(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orca::types::{
        AgentIdentity, DispatchStatus, MessageType, ReadSource, TaskStatus, TerminalStatus,
        WorkerState, WorkerTerminalState,
    };

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../../tests/fixtures/orca/", $name))
        };
    }

    #[test]
    fn terminal_list_fixture() {
        let list = parse_terminal_list(fixture!("terminal_list.json")).unwrap();
        assert_eq!(list.terminals.len(), 3);
        assert_eq!(list.total_count, Some(3));
        assert!(!list.truncated);

        let shell = &list.terminals[0];
        assert_eq!(shell.handle, "term_08fe0725-96a1-4355-a4fd-6c8a07aa6c72");
        assert_eq!(shell.agent_identity, None);
        assert!(shell.connected && shell.writable && !shell.orphaned);
        assert_eq!(
            shell.last_output_at.as_ref().and_then(|t| t.epoch_ms()),
            Some(1_789_598_758_208)
        );
        assert_eq!(
            shell.branch_name(),
            Some("gylleus/tinybird-replay-sessions")
        );
        assert_eq!(shell.execution_host_id.as_deref(), Some("local"));
        assert!(shell.worktree_path.is_some() && shell.tab_id.is_some());
        assert!(shell.leaf_id.is_some() && shell.pty_id.is_some());

        let agents: Vec<_> = list
            .terminals
            .iter()
            .map(|t| t.agent_identity.clone())
            .collect();
        assert_eq!(
            agents,
            vec![
                None,
                Some(AgentIdentity::Claude),
                Some(AgentIdentity::Codex)
            ]
        );
    }

    #[test]
    fn terminal_read_screen_fixture() {
        let read = parse_terminal_read(fixture!("terminal_read_screen.json")).unwrap();
        assert_eq!(read.status, Some(TerminalStatus::Running));
        assert_eq!(read.source, Some(ReadSource::Screen));
        assert_eq!(read.tail.len(), 2);
        assert_eq!(read.next_cursor, Some(8));
        assert_eq!(read.oldest_cursor, Some(0));
        assert_eq!(read.draft, None);
    }

    #[test]
    fn terminal_read_stream_fixture() {
        let read = parse_terminal_read(fixture!("terminal_read_stream.json")).unwrap();
        assert_eq!(read.source, Some(ReadSource::Stream));
        assert_eq!(read.tail.len(), 5);
        assert_eq!(read.returned_line_count, Some(5));
        assert_eq!(read.latest_cursor, Some(8));
    }

    #[test]
    fn terminal_read_stale_handle_is_api_error() {
        let err = parse_terminal_read(fixture!("error_terminal_handle_stale.json")).unwrap_err();
        assert_eq!(err.code(), Some("terminal_handle_stale"));
    }

    #[test]
    fn terminal_send_and_switch_results() {
        // Shapes from Orca's CLI formatter; not captured live to avoid side effects.
        let send = parse_terminal_send(
            br#"{"id":"1","ok":true,"result":{"send":{"handle":"term_1","bytesWritten":6}}}"#,
        )
        .unwrap();
        assert_eq!(send.bytes_written, Some(6));
        assert_eq!(send.accepted, None);
        let refused = parse_terminal_send(
            br#"{"id":"1","ok":true,"result":{"send":{"handle":"term_1","accepted":false,"bytesWritten":0,"refusedReason":"permission"}}}"#,
        )
        .unwrap();
        assert_eq!(refused.accepted, Some(false));
        assert_eq!(refused.refused_reason.as_deref(), Some("permission"));
        let focus = parse_terminal_switch(
            br#"{"id":"1","ok":true,"result":{"focus":{"handle":"term_1","tabId":"tab","navigated":false}}}"#,
        )
        .unwrap();
        assert_eq!(focus.tab_id.as_deref(), Some("tab"));
        assert_eq!(focus.navigated, Some(false));
    }

    #[test]
    fn run_list_fixture() {
        let runs = parse_run_list(fixture!("run_list.json")).unwrap();
        assert_eq!(runs.len(), 3);
        let run = &runs[0];
        assert_eq!(run.id, "run_7c25cdcf46e0");
        assert!(
            run.objective
                .as_deref()
                .unwrap()
                .starts_with("commerce-platform")
        );
        assert_eq!(
            run.coordinator_handle.as_deref(),
            Some("term_df9f48c9-c8e8-4ceb-afca-932028245fb1")
        );
        assert!(run.coordinator_pane_key.is_some());
        assert!(!run.legacy);
        let created = run.created_at.as_ref().unwrap().epoch_ms().unwrap();
        let updated = run.updated_at.as_ref().unwrap().epoch_ms().unwrap();
        assert!(updated > created);
    }

    #[test]
    fn worker_list_fixture() {
        let list = parse_worker_list(fixture!("worker_list.json")).unwrap();
        assert_eq!(list.workers.len(), 5);
        assert_eq!(list.counts.get("active"), Some(&9));

        let first = &list.workers[0];
        assert_eq!(first.worker_state, Some(WorkerState::Succeeded));
        assert_eq!(first.dispatch_status, Some(DispatchStatus::Completed));
        assert_eq!(first.terminal_state, Some(WorkerTerminalState::Released));
        let resource = first.resource.as_ref().unwrap();
        assert_eq!(resource.release_state.as_deref(), Some("released"));
        assert!(
            resource
                .release_completed_at
                .as_ref()
                .unwrap()
                .epoch_ms()
                .is_some()
        );

        assert!(list.workers[1].resource.is_none());
        assert_eq!(
            list.workers[2].dispatch_status,
            Some(DispatchStatus::CircuitBroken)
        );
        assert_eq!(
            list.workers[3].worker_state,
            Some(WorkerState::Unsupervised)
        );
        let active = &list.workers[4];
        assert_eq!(active.terminal_state, Some(WorkerTerminalState::Active));
        assert!(active.agent_terminal_handle.is_some());
    }

    #[test]
    fn task_list_fixture() {
        let tasks = parse_task_list(fixture!("task_list.json")).unwrap();
        assert_eq!(tasks.len(), 3);

        let done = &tasks[0];
        assert_eq!(done.status, TaskStatus::Completed);
        assert_eq!(done.run_id.as_deref(), Some("run_7c25cdcf46e0"));
        assert!(done.deps.is_empty());
        assert!(done.created_by_terminal_handle.is_some());
        assert!(done.completed_at.as_ref().unwrap().epoch_ms().is_some());
        let result = done.parsed_result().unwrap();
        assert_eq!(result["outcome"], "succeeded");
        assert!(done.label().starts_with("hack-76"));

        let dispatched = &tasks[1];
        assert_eq!(dispatched.status, TaskStatus::Dispatched);
        assert!(dispatched.assignee_handle.is_some());
        assert_eq!(dispatched.result, None);
        assert_eq!(dispatched.completed_at, None);

        assert_eq!(tasks[2].status, TaskStatus::Failed);
        assert_eq!(tasks[2].parsed_result(), None);
    }

    #[test]
    fn task_list_run_required_is_api_error() {
        let err = parse_task_list(fixture!("error_run_required.json")).unwrap_err();
        assert_eq!(err.code(), Some("run_required"));
    }

    #[test]
    fn inbox_fixture() {
        let msgs = parse_inbox(fixture!("inbox.json")).unwrap();
        assert_eq!(msgs.len(), 4);

        let heartbeat = &msgs[0];
        assert_eq!(heartbeat.message_type, Some(MessageType::Heartbeat));
        let payload = heartbeat.parsed_payload().unwrap();
        assert!(payload.task_id.is_some() && payload.dispatch_id.is_some());
        assert_eq!(payload.phase.as_deref(), Some("reviewing"));
        assert!(!heartbeat.read);
        assert!(heartbeat.created_at.as_ref().unwrap().epoch_ms().is_some());

        let done = &msgs[1];
        assert_eq!(done.message_type, Some(MessageType::WorkerDone));
        assert!(done.read);
        assert_eq!(
            done.parsed_payload().unwrap().outcome.as_deref(),
            Some("succeeded")
        );

        let delivered = &msgs[2];
        assert_eq!(delivered.message_type, Some(MessageType::Status));
        assert_eq!(delivered.payload, None);
        assert!(delivered.delivered_at.is_some());

        let threaded = &msgs[3];
        assert!(threaded.thread_id.is_some());
        assert!(
            threaded
                .to_handle
                .as_deref()
                .unwrap()
                .starts_with("dispatch:")
        );
    }

    #[test]
    fn value_flags_use_equals_form() {
        let input = TerminalInput {
            text: Some("--help me".into()),
            enter: true,
            interrupt: false,
        };
        assert_eq!(
            terminal_send_args("term_1", &input),
            vec![
                "terminal",
                "send",
                "--terminal=term_1",
                "--text=--help me",
                "--enter"
            ]
        );
        assert_eq!(
            terminal_read_args("term_1", Some(8), None),
            vec!["terminal", "read", "--terminal=term_1", "--cursor=8"]
        );
        assert_eq!(
            terminal_read_screen_args("term_1"),
            vec!["terminal", "read", "--terminal=term_1", "--screen"]
        );
        assert_eq!(
            inbox_args(Some(50), None),
            vec!["orchestration", "inbox", "--limit=50"]
        );
        assert_eq!(
            task_list_args("run_1"),
            vec!["orchestration", "task-list", "--run=run_1"]
        );
    }

    #[test]
    fn orchestration_send_args_cover_flags() {
        let msg = OutgoingMessage {
            subject: "Myrstack nudge".into(),
            to: Some("run:run_1".into()),
            body: Some("Please send a heartbeat".into()),
            message_type: Some("status".into()),
            task_id: Some("task_1".into()),
            files_modified: vec!["a.rs".into(), "b.rs".into()],
            ..Default::default()
        };
        assert_eq!(
            orchestration_send_args(&msg),
            vec![
                "orchestration",
                "send",
                "--subject=Myrstack nudge",
                "--to=run:run_1",
                "--body=Please send a heartbeat",
                "--type=status",
                "--task-id=task_1",
                "--files-modified=a.rs,b.rs",
            ]
        );
        let result = parse_orchestration_send(
            br#"{"id":"1","ok":true,"result":{"message":{"id":"msg_1"},"warnings":[]}}"#,
        )
        .unwrap();
        assert_eq!(result.message_id(), Some("msg_1"));
    }

    /// Calls the live Orca runtime with read-only commands.
    #[test]
    #[ignore = "needs a running Orca"]
    fn live_smoke() {
        let client = OrcaClient::new();
        let terminals = client.terminal_list().expect("terminal list");
        let runs = client.run_list().expect("run-list");
        let workers = client.worker_list(None).expect("worker-list");
        let inbox = client.inbox(Some(20), None).expect("inbox");
        for run in &runs {
            client.task_list(&run.id).expect("task-list");
        }
        if let Some(t) = terminals.terminals.first() {
            let screen = client
                .terminal_read_screen(&t.handle)
                .expect("read --screen");
            assert_eq!(screen.handle, t.handle);
            client
                .terminal_read(&t.handle, screen.next_cursor, Some(10))
                .expect("read --cursor");
        }
        let err = client
            .terminal_read_screen("term_does_not_exist")
            .unwrap_err();
        assert!(err.code().is_some(), "{err:?}");
        eprintln!(
            "live: {} terminals, {} runs, {} workers, {} messages",
            terminals.terminals.len(),
            runs.len(),
            workers.workers.len(),
            inbox.len()
        );
    }
}
