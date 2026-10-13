//! Broadcast one message to the coordinators of selected runs over the Orca bus.
//!
//! Addressing: each message goes to `run:<run_id>`, the Run mailbox. That is where a
//! worker's `worker_done` lands and what a coordinator drains with `orchestration check`,
//! so a broadcast reaches the coordinator the same way its workers' reports do. A bare
//! terminal handle is Orca's legacy pre-Dispatch address and bypasses the Run mailbox.
//!
//! Dedupe: a coordinator terminal that coordinates several selected runs gets one message,
//! sent to the mailbox of the first of those runs (in snapshot order); the delivery lists
//! every run it covers.
//!
//! The message type is `status` (informational, no reply expected). Text is prefixed with
//! `King: ` per `docs/heartbeat-convention.md`: broadcasts are Karl speaking through the King.

use std::collections::{BTreeSet, HashMap};
use std::thread;

use crossbeam_channel::{Receiver, unbounded};

use crate::model::{ClanSnapshot, NodeState};
use crate::orca::{BROADCAST_SENDER, OrcaClient, OutgoingMessage};

/// Every Myrstack message starts with this.
pub const PREFIX: &str = "King: ";
/// Longest subject; longer or multi-line text also goes in the body.
pub const MAX_SUBJECT_CHARS: usize = 100;

/// The runs the user ticked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BroadcastSelection {
    pub run_ids: BTreeSet<String>,
}

impl BroadcastSelection {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(run_ids: I) -> Self {
        Self {
            run_ids: run_ids.into_iter().map(Into::into).collect(),
        }
    }
}

/// One message to one coordinator.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub coordinator_handle: String,
    /// Selected runs this coordinator coordinates; the message goes to the first one's mailbox.
    pub run_ids: Vec<String>,
    pub message: OutgoingMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The run has no coordinator, or its terminal is not in `terminal list`.
    CoordinatorNotLive,
    /// The run is no longer in the snapshot.
    RunGone,
}

impl SkipReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::CoordinatorNotLive => "coordinator not live",
            Self::RunGone => "run no longer live",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub run_id: String,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BroadcastPlan {
    pub deliveries: Vec<Delivery>,
    pub skipped: Vec<Skipped>,
}

/// Adds the `King: ` prefix unless the text already has it. Trims surrounding whitespace.
pub fn prefixed(text: &str) -> String {
    let text = text.trim();
    if text.starts_with(PREFIX.trim_end()) {
        text.to_string()
    } else {
        format!("{PREFIX}{text}")
    }
}

/// The bus message for `text`, addressed to `run_id`'s mailbox, from [`BROADCAST_SENDER`].
pub fn broadcast_message(run_id: &str, text: &str) -> OutgoingMessage {
    let text = prefixed(text);
    let first_line = text.lines().next().unwrap_or_default();
    let fits = !text.contains('\n') && text.chars().count() <= MAX_SUBJECT_CHARS;
    let subject = if fits {
        text.clone()
    } else {
        let mut s: String = first_line.chars().take(MAX_SUBJECT_CHARS - 1).collect();
        s.push('…');
        s
    };
    OutgoingMessage {
        subject,
        to: Some(format!("run:{run_id}")),
        body: (!fits).then_some(text),
        message_type: Some("status".into()),
        from: Some(BROADCAST_SENDER.to_string()),
        ..OutgoingMessage::default()
    }
}

/// One delivery per live coordinator among the selected runs, in snapshot order.
///
/// Empty or whitespace-only text plans nothing.
pub fn plan_broadcast(
    snapshot: &ClanSnapshot,
    selection: &BroadcastSelection,
    text: &str,
) -> BroadcastPlan {
    let mut plan = BroadcastPlan::default();
    if text.trim().is_empty() {
        return plan;
    }
    let mut by_handle: HashMap<String, usize> = HashMap::new();
    for run in snapshot
        .runs
        .iter()
        .filter(|r| selection.run_ids.contains(&r.id))
    {
        let handle = run
            .coordinator
            .as_ref()
            .and_then(|id| snapshot.node(id))
            .filter(|n| n.live)
            .and_then(|n| n.terminal_handle.clone());
        let Some(handle) = handle else {
            plan.skipped.push(Skipped {
                run_id: run.id.clone(),
                reason: SkipReason::CoordinatorNotLive,
            });
            continue;
        };
        match by_handle.get(&handle) {
            Some(&i) => plan.deliveries[i].run_ids.push(run.id.clone()),
            None => {
                by_handle.insert(handle.clone(), plan.deliveries.len());
                plan.deliveries.push(Delivery {
                    coordinator_handle: handle,
                    run_ids: vec![run.id.clone()],
                    message: broadcast_message(&run.id, text),
                });
            }
        }
    }
    for run_id in &selection.run_ids {
        if snapshot.run(run_id).is_none() {
            plan.skipped.push(Skipped {
                run_id: run_id.clone(),
                reason: SkipReason::RunGone,
            });
        }
    }
    plan
}

/// A run as the broadcast window lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunChoice {
    pub run_id: String,
    pub objective: String,
    pub repo: Option<String>,
    /// `None` when the coordinator is not live.
    pub coordinator_state: Option<NodeState>,
}

/// Runs in the snapshot, with the repo and state of their coordinator.
pub fn run_choices(snapshot: &ClanSnapshot) -> Vec<RunChoice> {
    snapshot
        .runs
        .iter()
        .map(|run| {
            let coordinator = run
                .coordinator
                .as_ref()
                .and_then(|id| snapshot.node(id))
                .filter(|n| n.live);
            let repo = coordinator
                .and_then(|n| n.worktree_path.as_deref())
                .or_else(|| {
                    snapshot
                        .nodes_in_run(&run.id)
                        .find_map(|n| n.worktree_path.as_deref())
                })
                .and_then(repo_name);
            RunChoice {
                run_id: run.id.clone(),
                objective: run.objective.clone().unwrap_or_default(),
                repo,
                coordinator_state: coordinator.map(|n| n.state),
            }
        })
        .collect()
}

/// Repository name from a worktree path: `…/workspaces/<repo>/<wt>`,
/// `…/<repo>-worktrees/<wt>`, otherwise the last path component.
pub fn repo_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if let Some(i) = parts.iter().position(|p| *p == "workspaces")
        && let Some(repo) = parts.get(i + 1)
    {
        return Some((*repo).to_string());
    }
    if parts.len() >= 2
        && let Some(repo) = parts[parts.len() - 2].strip_suffix("-worktrees")
    {
        return Some(repo.to_string());
    }
    parts.last().map(|p| (*p).to_string())
}

// ---------------------------------------------------------------------------
// Execution

/// Sends one bus message. Implemented by `OrcaClient`; tests use a fake.
pub trait BroadcastSender: Send {
    /// Returns the sent message's id, if Orca reported one.
    fn send(&mut self, message: &OutgoingMessage) -> Result<Option<String>, String>;
}

impl BroadcastSender for OrcaClient {
    fn send(&mut self, message: &OutgoingMessage) -> Result<Option<String>, String> {
        self.orchestration_send(message)
            .map(|r| r.message_id().map(str::to_string))
            .map_err(|e| e.to_string())
    }
}

/// Outcome of one delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryResult {
    /// Index into the plan's `deliveries`.
    pub index: usize,
    pub outcome: Result<Option<String>, String>,
}

/// Sends a plan's deliveries one by one on a background thread.
///
/// Local stand-in for a shared job runner; swap for one when it lands.
pub struct BroadcastJob {
    total: usize,
    results: Vec<DeliveryResult>,
    rx: Receiver<DeliveryResult>,
}

impl BroadcastJob {
    /// `on_result` runs on the worker thread after each send (e.g. to request a repaint).
    pub fn spawn(
        mut sender: Box<dyn BroadcastSender>,
        deliveries: Vec<Delivery>,
        on_result: Option<Box<dyn Fn() + Send>>,
    ) -> Self {
        let (tx, rx) = unbounded();
        let total = deliveries.len();
        thread::Builder::new()
            .name("myrstack-broadcast".into())
            .spawn(move || {
                for (index, delivery) in deliveries.iter().enumerate() {
                    let outcome = sender.send(&delivery.message);
                    if tx.send(DeliveryResult { index, outcome }).is_err() {
                        return;
                    }
                    if let Some(f) = &on_result {
                        f();
                    }
                }
            })
            .expect("spawn broadcast thread");
        Self {
            total,
            results: Vec::new(),
            rx,
        }
    }

    /// Collects results that arrived since the last call; returns all results so far.
    pub fn poll(&mut self) -> &[DeliveryResult] {
        self.results.extend(self.rx.try_iter());
        &self.results
    }

    pub fn results(&self) -> &[DeliveryResult] {
        &self.results
    }

    pub fn is_done(&self) -> bool {
        self.results.len() >= self.total
    }

    pub fn total(&self) -> usize {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::clan::ClanData;
    use crate::orca::{Run, Terminal, Worker};

    fn terminal(handle: &str, path: &str) -> Terminal {
        Terminal {
            handle: handle.into(),
            worktree_path: Some(path.into()),
            connected: true,
            writable: true,
            ..Terminal::default()
        }
    }

    fn run(id: &str, coordinator: &str) -> Run {
        Run {
            id: id.into(),
            objective: Some(format!("objective {id}")),
            coordinator_handle: Some(coordinator.into()),
            ..Run::default()
        }
    }

    fn worker(dispatch: &str, run: &str, handle: &str) -> Worker {
        serde_json::from_value(serde_json::json!({
            "dispatchId": dispatch,
            "runId": run,
            "agentTerminalHandle": handle,
        }))
        .unwrap()
    }

    /// r1 → c1, r2 → c2, r3 → dead coordinator (live worker w3 keeps it in the snapshot),
    /// r4 → c1 again. Workers keep every run live even though c1's node carries only one run.
    fn snapshot() -> ClanSnapshot {
        let data = ClanData {
            terminals: vec![
                terminal("c1", "/u/orca/workspaces/depict/a"),
                terminal("c2", "/u/repo/saker/myrstack-worktrees/b"),
                terminal("w1", "/u/orca/workspaces/depict/a"),
                terminal("w3", "/u/repo/depict/realtime"),
                terminal("w4", "/u/repo/depict/realtime"),
            ],
            runs: vec![
                run("r1", "c1"),
                run("r2", "c2"),
                run("r3", "gone"),
                run("r4", "c1"),
            ],
            workers: vec![
                worker("d1", "r1", "w1"),
                worker("d3", "r3", "w3"),
                worker("d4", "r4", "w4"),
            ],
            ..ClanData::default()
        };
        ClanSnapshot::build(&data, 0)
    }

    fn all() -> BroadcastSelection {
        BroadcastSelection::new(["r1", "r2", "r3", "r4"])
    }

    #[test]
    fn one_delivery_per_live_coordinator() {
        let plan = plan_broadcast(&snapshot(), &BroadcastSelection::new(["r1", "r2"]), "hi");
        let handles: Vec<_> = plan
            .deliveries
            .iter()
            .map(|d| d.coordinator_handle.as_str())
            .collect();
        assert_eq!(handles, ["c1", "c2"]);
        assert_eq!(plan.deliveries[0].message.to.as_deref(), Some("run:r1"));
        assert_eq!(plan.deliveries[1].message.to.as_deref(), Some("run:r2"));
        assert_eq!(
            plan.deliveries[0].message.message_type.as_deref(),
            Some("status")
        );
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn dead_coordinator_is_skipped_and_reported() {
        let plan = plan_broadcast(&snapshot(), &BroadcastSelection::new(["r3"]), "hi");
        assert!(plan.deliveries.is_empty());
        assert_eq!(
            plan.skipped,
            [Skipped {
                run_id: "r3".into(),
                reason: SkipReason::CoordinatorNotLive
            }]
        );
    }

    #[test]
    fn coordinator_of_two_runs_gets_one_message() {
        let plan = plan_broadcast(&snapshot(), &all(), "hi");
        assert_eq!(plan.deliveries.len(), 2);
        let c1 = &plan.deliveries[0];
        assert_eq!(c1.coordinator_handle, "c1");
        assert_eq!(c1.run_ids, ["r1", "r4"]);
        assert_eq!(c1.message.to.as_deref(), Some("run:r1"));
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn empty_selection_or_text_plans_nothing() {
        let empty = plan_broadcast(&snapshot(), &BroadcastSelection::default(), "hi");
        assert_eq!(empty, BroadcastPlan::default());
        let blank = plan_broadcast(&snapshot(), &all(), "  \n ");
        assert_eq!(blank, BroadcastPlan::default());
    }

    #[test]
    fn vanished_run_is_reported() {
        let plan = plan_broadcast(&snapshot(), &BroadcastSelection::new(["r1", "zz"]), "hi");
        assert_eq!(plan.deliveries.len(), 1);
        assert_eq!(
            plan.skipped,
            [Skipped {
                run_id: "zz".into(),
                reason: SkipReason::RunGone
            }]
        );
    }

    #[test]
    fn prefix_added_once() {
        assert_eq!(prefixed("  status please "), "King: status please");
        assert_eq!(prefixed("King: already"), "King: already");
        assert_eq!(prefixed("King:tight"), "King:tight");
        let msg = broadcast_message("r1", "hold merges");
        assert_eq!(msg.subject, "King: hold merges");
        assert_eq!(msg.body, None);
    }

    #[test]
    fn long_or_multiline_text_goes_in_body() {
        let msg = broadcast_message("r1", "first line\nsecond line");
        assert_eq!(msg.subject, "King: first line…");
        assert_eq!(msg.body.as_deref(), Some("King: first line\nsecond line"));
        let long = "x".repeat(300);
        let msg = broadcast_message("r1", &long);
        assert_eq!(msg.subject.chars().count(), MAX_SUBJECT_CHARS);
        assert_eq!(
            msg.body.as_deref(),
            Some(format!("{PREFIX}{long}").as_str())
        );
    }

    #[test]
    fn broadcast_has_explicit_sender() {
        let msg = broadcast_message("r1", "hi");
        assert_eq!(msg.from.as_deref(), Some(BROADCAST_SENDER));
        let args = crate::orca::commands::orchestration_send_args(&msg);
        assert!(args.contains(&"--from=king".to_string()), "{args:?}");
        assert!(args.contains(&"--to=run:r1".to_string()), "{args:?}");
    }

    #[test]
    fn repo_names() {
        assert_eq!(
            repo_name("/u/orca/workspaces/depict/a").as_deref(),
            Some("depict")
        );
        assert_eq!(
            repo_name("/u/repo/saker/myrstack-worktrees/b").as_deref(),
            Some("myrstack")
        );
        assert_eq!(
            repo_name("/u/repo/depict/realtime/").as_deref(),
            Some("realtime")
        );
        assert_eq!(repo_name("/"), None);
    }

    #[test]
    fn choices_carry_repo_and_state() {
        let choices = run_choices(&snapshot());
        let ids: Vec<_> = choices.iter().map(|c| c.run_id.as_str()).collect();
        assert_eq!(ids, ["r1", "r2", "r3", "r4"]);
        assert_eq!(choices[0].repo.as_deref(), Some("depict"));
        assert!(choices[0].coordinator_state.is_some());
        assert_eq!(choices[2].repo.as_deref(), Some("realtime"));
        assert_eq!(choices[2].coordinator_state, None);
    }

    struct FakeSender {
        sent: Arc<Mutex<Vec<OutgoingMessage>>>,
        fail_to: &'static str,
    }

    impl BroadcastSender for FakeSender {
        fn send(&mut self, message: &OutgoingMessage) -> Result<Option<String>, String> {
            self.sent.lock().unwrap().push(message.clone());
            if message.to.as_deref() == Some(self.fail_to) {
                Err("boom".into())
            } else {
                Ok(Some(format!("msg-{}", self.sent.lock().unwrap().len())))
            }
        }
    }

    #[test]
    fn job_sends_each_delivery_once_in_order() {
        let plan = plan_broadcast(&snapshot(), &all(), "hi");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sender = FakeSender {
            sent: sent.clone(),
            fail_to: "run:r2",
        };
        let pings = Arc::new(Mutex::new(0));
        let pings_in = pings.clone();
        let mut job = BroadcastJob::spawn(
            Box::new(sender),
            plan.deliveries.clone(),
            Some(Box::new(move || *pings_in.lock().unwrap() += 1)),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !job.is_done() {
            assert!(Instant::now() < deadline, "job did not finish");
            job.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        let results = job.results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].index, 0);
        assert_eq!(results[0].outcome, Ok(Some("msg-1".into())));
        assert_eq!(results[1].outcome, Err("boom".into()));
        let to: Vec<_> = sent
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.to.clone().unwrap())
            .collect();
        assert_eq!(to, ["run:r1", "run:r2"]);
        assert_eq!(job.total(), 2);
        // The callback runs after the send is reported; give the thread a moment.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(*pings.lock().unwrap(), 2);
    }

    #[test]
    fn empty_job_is_done_immediately() {
        let sender = FakeSender {
            sent: Arc::default(),
            fail_to: "",
        };
        let job = BroadcastJob::spawn(Box::new(sender), Vec::new(), None);
        assert!(job.is_done());
    }
}
