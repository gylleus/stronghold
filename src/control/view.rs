//! `StatusView`: the serde read model of the fleet that the control API serves.
//!
//! The app builds one after each change ([`StatusView::build`]) and publishes it into a
//! [`SharedStatus`]; the server threads only ever read a published `Arc`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::app::View;
use crate::claude::ClaudeData;
use crate::claude::usage::TokenCounts;
use crate::detect::PromptKind;
use crate::fleet::FleetData;
use crate::model::{EdgeKind, FleetSnapshot, Node, NodeKind, NodeState, TaskCounts, fleet_state};
use crate::notify::{self, history::Entry};
use crate::playbook::fetches::PlaybookView;
use crate::playbook::fill::FleetFacts;
use crate::quota::{self, Quotas, Severity};
use crate::ui::sidebar::{FleetFilter, node_repo};
use crate::whip::{WhipLogItem, WhipSettings, WhipStatus};

/// Whip log entries a published view keeps (newest).
pub const WHIP_LOG_KEPT: usize = 200;
/// Longest preview kept per agent.
const PREVIEW_CHARS: usize = 200;

/// Everything the read methods serve, as of one publish.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusView {
    /// When the app built this view, epoch ms. 0 means the app has not published yet.
    pub published_at_ms: i64,
    /// When Orca was last polled; `None` before the first poll.
    pub polled_at_ms: Option<i64>,
    /// Errors of the last poll.
    pub errors: Vec<String>,
    pub fleet: FleetSummary,
    pub agents: Vec<AgentView>,
    pub whip: WhipView,
    pub quotas: QuotasView,
    /// The notification inbox, newest first.
    pub alerts: Vec<AlertView>,
    pub dashboard: DashboardView,
    /// Current playbook version and who fetched which; set by the app after [`StatusView::build`].
    #[serde(default)]
    pub playbook: PlaybookView,
    /// The Queen's chat is in auto mode (her actions run without Karl's confirmation).
    /// Set by the app after [`StatusView::build`]; read-only for API clients.
    #[serde(default)]
    pub queen_auto: bool,
    /// The health doctor's checks; set by the app after [`StatusView::build`].
    #[serde(default)]
    pub health: crate::health::HealthReport,
    /// The raw poll data the `fleet_data` method serves (for filled playbook templates).
    #[serde(skip)]
    pub fleet_data: Option<Arc<FleetFacts>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetSummary {
    /// The Queen's state: the fleet's most urgent one (`needs_input` > `stalled` > `working` >
    /// `idle`).
    pub state: String,
    /// Agent nodes of every kind (the Queen is not one).
    pub agents: usize,
    /// Nodes with a live terminal.
    pub live: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub by_state: BTreeMap<String, usize>,
    /// Ids of nodes waiting on Karl.
    pub needs_input: Vec<String>,
    /// Ids of stalled nodes (escalated by the whip).
    pub stalled: Vec<String>,
    /// Ids of agents Karl parked in the sleeping room ([`crate::park`]). They count in
    /// `agents`, `live` and `by_kind` but not in `by_state`, `needs_input` or `stalled`.
    #[serde(default)]
    pub parked: Vec<String>,
    pub runs: Vec<RunView>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub id: String,
    pub objective: Option<String>,
    pub repo: Option<String>,
    /// Node id of the live coordinator.
    pub coordinator: Option<String>,
    pub coordinator_state: Option<String>,
    /// Node id of the attendant: the terminal Orca records as coordinator, which forwards the
    /// run's bus events to the coordinator. Messages to the run's mailbox land there.
    #[serde(default)]
    pub attendant: Option<String>,
    /// `None` when the last poll fetched no tasks for the run.
    pub tasks: Option<TaskCountsView>,
    /// Worker nodes, live or not.
    pub workers: usize,
    pub by_state: BTreeMap<String, usize>,
    /// Some node of the run waits on Karl.
    pub needs_input: bool,
    /// Some node of the run is stalled.
    pub stalled: bool,
    /// Newest terminal output in the run, epoch ms.
    pub last_activity_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCountsView {
    pub total: usize,
    pub pending: usize,
    pub ready: usize,
    pub dispatched: usize,
    pub completed: usize,
    pub failed: usize,
    pub blocked: usize,
    pub other: usize,
}

impl From<&TaskCounts> for TaskCountsView {
    fn from(c: &TaskCounts) -> Self {
        Self {
            total: c.total(),
            pending: c.pending,
            ready: c.ready,
            dispatched: c.dispatched,
            completed: c.completed,
            failed: c.failed,
            blocked: c.blocked,
            other: c.other,
        }
    }
}

/// One node of the graph: a coordinator, worker, loose terminal or Claude subagent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentView {
    /// Stable node id: `term:<handle>`, `dispatch:<run>:<dispatch>` or `sub:<session>:<agent>`.
    pub id: String,
    /// `coordinator`, `attendant`, `worker`, `terminal` or `subagent`.
    pub kind: String,
    pub label: String,
    /// `working`, `idle`, `needs_input`, `stalled`, `done` or `failed`.
    pub state: String,
    /// Whether the node's terminal is in `terminal list`.
    pub live: bool,
    pub run_id: Option<String>,
    pub run_objective: Option<String>,
    pub task_id: Option<String>,
    /// `claude` or `codex`.
    pub agent: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub repo: String,
    pub terminal_handle: Option<String>,
    /// Parent node: the coordinator of a worker or attendant, the terminal (or subagent) of a
    /// subagent.
    pub parent: Option<String>,
    pub last_output_ms: Option<i64>,
    /// Seconds since the last output, as of the response.
    pub idle_secs: Option<i64>,
    /// Set while the node waits on Karl.
    pub needs_input: Option<PromptView>,
    pub stalled: bool,
    /// The whip escalated this node's terminal.
    pub escalated: bool,
    pub tokens: Option<TokensView>,
    /// Released worker / finished subagent, fading out.
    pub released: bool,
    /// Parked by Karl ([`crate::park`]): asleep, its session kept; never `needs_input` or
    /// `stalled`. Sending it text wakes it.
    #[serde(default)]
    pub parked: bool,
    /// When it was parked, epoch ms.
    #[serde(default)]
    pub parked_at_ms: Option<i64>,
    pub worker_state: Option<String>,
    pub dispatch_status: Option<String>,
    /// Last line Orca previews for the terminal (truncated).
    pub preview: Option<String>,
}

/// What a waiting node asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptView {
    /// `permission`, `choice`, `confirm`, `freeform_question`, or `unknown` when only the
    /// preview hinted at a prompt.
    pub kind: String,
    /// Menu options of a `choice` prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

impl From<&PromptKind> for PromptView {
    fn from(kind: &PromptKind) -> Self {
        let (kind, options) = match kind {
            PromptKind::Permission => ("permission", Vec::new()),
            PromptKind::Choice { options } => ("choice", options.clone()),
            PromptKind::Confirm => ("confirm", Vec::new()),
            PromptKind::FreeformQuestion => ("freeform_question", Vec::new()),
        };
        Self {
            kind: kind.into(),
            options,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokensView {
    pub total: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl From<&TokenCounts> for TokensView {
    fn from(t: &TokenCounts) -> Self {
        Self {
            total: t.total(),
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_creation: t.cache_creation,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WhipView {
    pub enabled: bool,
    pub dry_run: bool,
    pub notify: bool,
    pub threshold_ms: i64,
    pub cooldown_ms: i64,
    pub max_nudges: u32,
    pub tick_ms: i64,
    pub abandon_after_ms: i64,
    pub last_tick_ms: Option<i64>,
    /// Stall findings of the last tick.
    pub findings: usize,
    pub actions_logged: u64,
    /// Terminal handles with an escalated stall.
    pub escalated: Vec<String>,
    /// Newest entries last.
    pub log: Vec<WhipLogView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhipLogView {
    pub seq: u64,
    pub at_ms: i64,
    pub action: String,
    pub target: String,
    pub reason: String,
    pub dry_run: bool,
    pub outcome: String,
    /// Sent because Karl (or an agent through the API) asked, not by a tick.
    #[serde(default)]
    pub manual: bool,
    pub handle: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
}

impl From<&WhipLogItem> for WhipLogView {
    fn from(item: &WhipLogItem) -> Self {
        let e = &item.entry;
        Self {
            seq: item.seq,
            at_ms: e.at_ms,
            action: e.action.clone(),
            target: e.target.clone(),
            reason: e.reason.clone(),
            dry_run: e.dry_run,
            outcome: e.outcome.clone(),
            manual: e.manual,
            handle: item.handle.clone(),
            run_id: item.run_id.clone(),
            task_id: item.task_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotasView {
    /// 0 before the first fetch.
    pub fetched_at_ms: i64,
    pub error: Option<String>,
    pub providers: Vec<ProviderView>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderView {
    pub key: String,
    pub name: String,
    pub status: Option<String>,
    pub error: Option<String>,
    pub windows: Vec<WindowView>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowView {
    pub key: String,
    /// Short name: `5h`, `wk`, `Fable wk`, `mo`.
    pub label: String,
    pub used_percent: f64,
    pub resets_at_ms: Option<i64>,
    pub reset_description: Option<String>,
    /// `normal`, `warn` or `critical`.
    pub severity: String,
}

/// One notification in the inbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AlertView {
    #[serde(default)]
    pub id: u64,
    /// When it last arrived.
    pub at_ms: i64,
    /// `info`, `warn` or `urgent`.
    #[serde(default)]
    pub severity: String,
    /// `agent`, `needs_input`, `escalation`, `review` or `test`.
    #[serde(default)]
    pub source: String,
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub url: Option<String>,
    /// The agent's name when it was raised.
    #[serde(default)]
    pub agent: Option<String>,
    /// Node the alert is about, if still in the fleet.
    pub node: Option<String>,
    /// Identical notifications collapsed into this one.
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub read: bool,
}

impl AlertView {
    pub fn build(entry: &Entry, snapshot: &FleetSnapshot) -> Self {
        Self {
            id: entry.id,
            at_ms: entry.at_ms,
            severity: entry.severity.name().into(),
            source: entry.source.name().into(),
            title: entry.title.clone(),
            message: entry.message.clone(),
            url: entry.url.clone(),
            agent: entry.agent_label.clone(),
            node: entry
                .target
                .as_deref()
                .and_then(|t| notify::resolve_target(snapshot, t))
                .map(|id| id.0),
            count: entry.count,
            read: entry.read,
        }
    }
}

/// What the dashboard currently shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DashboardView {
    /// `graph` or `table`.
    pub view: String,
    pub selected: Option<String>,
    /// Every selected node (several after a box select or Shift/Cmd-click); includes
    /// `selected` when it is shown in the graph.
    #[serde(default)]
    pub selection: Vec<String>,
    pub filter: FilterView,
    pub whip_settings_open: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterView {
    pub repos: Vec<String>,
    pub states: Vec<String>,
    pub kinds: Vec<String>,
    pub text: String,
    pub show_loose: bool,
    pub show_released: bool,
    /// Nodes the filter hides.
    pub hidden: usize,
}

// ---------------------------------------------------------------------------
// Names

pub fn state_name(state: NodeState) -> &'static str {
    match state {
        NodeState::Working => "working",
        NodeState::Idle => "idle",
        NodeState::NeedsInput => "needs_input",
        NodeState::Stalled => "stalled",
        NodeState::Done => "done",
        NodeState::Failed => "failed",
    }
}

/// Accepts `needs_input`, `NeedsInput`, `needs-input`, …
pub fn parse_state(s: &str) -> Option<NodeState> {
    const ALL: [NodeState; 6] = [
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Stalled,
        NodeState::Done,
        NodeState::Failed,
    ];
    ALL.into_iter()
        .find(|st| squash(state_name(*st)) == squash(s))
}

pub fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Coordinator => "coordinator",
        NodeKind::Attendant => "attendant",
        NodeKind::Worker => "worker",
        NodeKind::Terminal => "terminal",
        NodeKind::Subagent => "subagent",
        NodeKind::Queen => "queen",
    }
}

pub fn parse_kind(s: &str) -> Option<NodeKind> {
    const ALL: [NodeKind; 6] = [
        NodeKind::Coordinator,
        NodeKind::Attendant,
        NodeKind::Worker,
        NodeKind::Terminal,
        NodeKind::Subagent,
        NodeKind::Queen,
    ];
    ALL.into_iter().find(|k| squash(kind_name(*k)) == squash(s))
}

pub fn view_name(view: View) -> &'static str {
    match view {
        View::Graph => "graph",
        View::Table => "table",
        View::AntFarm => "ant_farm",
    }
}

pub fn parse_view(s: &str) -> Option<View> {
    match squash(s).as_str() {
        "graph" => Some(View::Graph),
        "table" => Some(View::Table),
        "antfarm" | "farm" | "ants" => Some(View::AntFarm),
        _ => None,
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Normal => "normal",
        Severity::Warn => "warn",
        Severity::Critical => "critical",
    }
}

// ---------------------------------------------------------------------------
// Building

/// The app state a view is built from.
pub struct ViewInputs<'a> {
    pub snapshot: Option<&'a FleetSnapshot>,
    pub data: Option<&'a FleetData>,
    pub claude: Option<&'a ClaudeData>,
    pub whip_status: &'a WhipStatus,
    pub whip_settings: &'a WhipSettings,
    /// Whip log, oldest first.
    pub whip_log: Vec<&'a WhipLogItem>,
    pub quotas: &'a Quotas,
    /// The notification inbox, oldest first.
    pub alerts: &'a [Entry],
    pub view: View,
    pub selected: Option<&'a str>,
    pub selection: Vec<&'a str>,
    pub filter: &'a FleetFilter,
    pub whip_settings_open: bool,
    pub now_ms: i64,
}

impl StatusView {
    pub fn build(inputs: &ViewInputs<'_>) -> Self {
        let empty = FleetSnapshot::default();
        let snapshot = inputs.snapshot.unwrap_or(&empty);
        let agents: Vec<AgentView> = snapshot
            .agent_nodes()
            .map(|n| agent_view(n, snapshot, inputs))
            .collect();
        let fleet = fleet_summary(snapshot, &agents);
        Self {
            published_at_ms: inputs.now_ms,
            polled_at_ms: inputs.data.map(|d| d.polled_at_ms),
            errors: inputs.data.map(|d| d.errors.clone()).unwrap_or_default(),
            fleet,
            agents,
            whip: whip_view(inputs),
            quotas: quotas_view(inputs.quotas),
            alerts: inputs
                .alerts
                .iter()
                .rev()
                .map(|a| AlertView::build(a, snapshot))
                .collect(),
            dashboard: DashboardView {
                view: view_name(inputs.view).into(),
                selected: inputs.selected.map(str::to_owned),
                selection: inputs.selection.iter().map(|s| (*s).to_owned()).collect(),
                filter: filter_view(inputs.filter, snapshot),
                whip_settings_open: inputs.whip_settings_open,
            },
            playbook: PlaybookView::default(),
            queen_auto: false,
            health: Default::default(),
            fleet_data: inputs.data.map(|d| Arc::new(FleetFacts::from_fleet(d))),
        }
    }

    /// An agent by node id (`term:…`), terminal handle, or case-insensitive label.
    pub fn find_agent(&self, key: &str) -> Option<&AgentView> {
        let key = key.trim();
        self.agents
            .iter()
            .find(|a| a.id == key)
            .or_else(|| {
                self.agents
                    .iter()
                    .find(|a| a.live && a.terminal_handle.as_deref() == Some(key))
            })
            .or_else(|| {
                self.agents
                    .iter()
                    .find(|a| a.terminal_handle.as_deref() == Some(key))
            })
            .or_else(|| {
                self.agents
                    .iter()
                    .find(|a| a.label.eq_ignore_ascii_case(key))
            })
    }
}

impl AgentView {
    /// A copy with `idle_secs` computed for `now_ms`.
    pub fn at(&self, now_ms: i64) -> Self {
        let mut a = self.clone();
        a.idle_secs = a.last_output_ms.map(|t| ((now_ms - t) / 1000).max(0));
        a
    }
}

fn agent_view(node: &Node, snapshot: &FleetSnapshot, inputs: &ViewInputs<'_>) -> AgentView {
    let handle = node.terminal_handle.as_deref();
    let prompt = handle
        .and_then(|h| inputs.data?.needs_input.get(h))
        .and_then(|v| v.as_ref());
    let needs_input = match (node.state, prompt) {
        _ if node.is_parked() => None,
        (NodeState::NeedsInput, Some(kind)) => Some(PromptView::from(kind)),
        (NodeState::NeedsInput, None) => Some(PromptView {
            kind: "unknown".into(),
            options: Vec::new(),
        }),
        _ => None,
    };
    let escalated = handle.is_some_and(|h| inputs.whip_status.escalated.contains(h));
    let parent = snapshot
        .edges
        .iter()
        .find(|e| {
            e.to == node.id
                && matches!(
                    e.kind,
                    EdgeKind::CoordinatorWorker | EdgeKind::Subagent | EdgeKind::Attendant
                )
        })
        .map(|e| e.from.0.clone());
    let run_objective = node
        .run_id
        .as_deref()
        .and_then(|r| snapshot.run(r))
        .and_then(|r| r.objective.clone());
    let preview = node.preview.as_deref().map(|p| {
        let p = p.trim();
        if p.chars().count() > PREVIEW_CHARS {
            let mut s: String = p.chars().take(PREVIEW_CHARS - 1).collect();
            s.push('…');
            s
        } else {
            p.to_owned()
        }
    });
    AgentView {
        id: node.id.0.clone(),
        kind: kind_name(node.kind).into(),
        label: node.label.clone(),
        state: state_name(node.state).into(),
        live: node.live,
        run_id: node.run_id.clone(),
        run_objective,
        task_id: node.task_id.clone(),
        agent: node.agent.as_ref().map(|a| a.to_string()),
        branch: node.branch.clone(),
        worktree: node.worktree_path.clone(),
        repo: node_repo(node, snapshot),
        terminal_handle: node.terminal_handle.clone(),
        parent,
        last_output_ms: node.last_output_ms,
        idle_secs: node
            .last_output_ms
            .map(|t| ((inputs.now_ms - t) / 1000).max(0)),
        needs_input,
        stalled: node.state == NodeState::Stalled && !node.is_parked(),
        escalated,
        tokens: inputs
            .claude
            .and_then(|c| c.node_usage(node))
            .map(|u| TokensView::from(&u.tokens)),
        released: node.released_at_ms.is_some(),
        parked: node.is_parked(),
        parked_at_ms: node.parked_at_ms,
        worker_state: node.worker_state.as_ref().map(|s| s.to_string()),
        dispatch_status: node.dispatch_status.as_ref().map(|s| s.to_string()),
        preview,
    }
}

fn fleet_summary(snapshot: &FleetSnapshot, agents: &[AgentView]) -> FleetSummary {
    let mut by_kind = BTreeMap::new();
    let mut by_state = BTreeMap::new();
    for a in agents {
        *by_kind.entry(a.kind.clone()).or_insert(0) += 1;
        if !a.parked {
            *by_state.entry(a.state.clone()).or_insert(0) += 1;
        }
    }
    let ids = |state: &str| {
        agents
            .iter()
            .filter(|a| a.state == state && !a.parked)
            .map(|a| a.id.clone())
            .collect::<Vec<_>>()
    };
    let runs = snapshot
        .runs
        .iter()
        .map(|run| {
            let members: Vec<&AgentView> = agents
                .iter()
                .filter(|a| a.run_id.as_deref() == Some(run.id.as_str()))
                .collect();
            let mut run_states = BTreeMap::new();
            for a in members.iter().filter(|a| !a.parked) {
                *run_states.entry(a.state.clone()).or_insert(0) += 1;
            }
            let coordinator = run
                .coordinator
                .as_ref()
                .and_then(|id| agents.iter().find(|a| a.id == id.0));
            let repo = coordinator
                .map(|c| c.repo.clone())
                .or_else(|| members.first().map(|a| a.repo.clone()));
            RunView {
                id: run.id.clone(),
                objective: run.objective.clone(),
                repo,
                coordinator: coordinator.map(|c| c.id.clone()),
                coordinator_state: coordinator.map(|c| c.state.clone()),
                attendant: snapshot.attendant(&run.id).map(|a| a.0.clone()),
                tasks: run.task_counts.as_ref().map(TaskCountsView::from),
                workers: members.iter().filter(|a| a.kind == "worker").count(),
                by_state: run_states,
                needs_input: members.iter().any(|a| a.needs_input.is_some()),
                stalled: members.iter().any(|a| a.stalled),
                last_activity_ms: members.iter().filter_map(|a| a.last_output_ms).max(),
            }
        })
        .collect();
    FleetSummary {
        state: state_name(fleet_state(snapshot.agent_nodes())).into(),
        agents: agents.len(),
        live: agents.iter().filter(|a| a.live).count(),
        by_kind,
        by_state,
        needs_input: ids("needs_input"),
        stalled: ids("stalled"),
        parked: agents
            .iter()
            .filter(|a| a.parked)
            .map(|a| a.id.clone())
            .collect(),
        runs,
    }
}

fn whip_view(inputs: &ViewInputs<'_>) -> WhipView {
    let s = inputs.whip_settings;
    let st = inputs.whip_status;
    let skip = inputs.whip_log.len().saturating_sub(WHIP_LOG_KEPT);
    WhipView {
        enabled: s.enabled,
        dry_run: s.dry_run,
        notify: s.notify,
        threshold_ms: s.threshold_ms,
        cooldown_ms: s.cooldown_ms,
        max_nudges: s.max_nudges,
        tick_ms: s.tick_ms,
        abandon_after_ms: s.abandon_after_ms,
        last_tick_ms: st.last_tick_ms,
        findings: st.findings,
        actions_logged: st.actions_logged,
        escalated: st.escalated.iter().cloned().collect(),
        log: inputs
            .whip_log
            .iter()
            .skip(skip)
            .map(|i| WhipLogView::from(*i))
            .collect(),
    }
}

fn quotas_view(quotas: &Quotas) -> QuotasView {
    QuotasView {
        fetched_at_ms: quotas.fetched_at_ms,
        error: quotas.error.clone(),
        providers: quotas
            .providers
            .iter()
            .map(|p| ProviderView {
                key: p.limit.key.clone(),
                name: p.name(),
                status: p.limit.status.clone(),
                error: p.limit.error.clone(),
                windows: p
                    .limit
                    .windows
                    .iter()
                    .map(|w| WindowView {
                        key: w.key.clone(),
                        label: quota::window_label(w),
                        used_percent: w.used_percent,
                        resets_at_ms: w.resets_at_ms,
                        reset_description: w.reset_description.clone(),
                        severity: severity_name(Severity::from_percent(w.used_percent)).into(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn filter_view(filter: &FleetFilter, snapshot: &FleetSnapshot) -> FilterView {
    FilterView {
        repos: filter.repos.iter().cloned().collect(),
        states: filter
            .states
            .iter()
            .map(|s| state_name(*s).into())
            .collect(),
        kinds: filter.kinds.iter().map(|k| kind_name(*k).into()).collect(),
        text: filter.text.clone(),
        show_loose: filter.show_loose,
        show_released: filter.show_released,
        hidden: if filter.is_active() {
            filter.hidden(snapshot).len()
        } else {
            0
        },
    }
}

/// Whip settings as the API reports them.
pub fn whip_settings_json(settings: &WhipSettings) -> serde_json::Value {
    serde_json::to_value(settings).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Sharing

/// The newest published view, shared between the app (writer) and server threads (readers).
#[derive(Clone, Default)]
pub struct SharedStatus {
    inner: Arc<RwLock<Arc<StatusView>>>,
}

impl SharedStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, view: StatusView) {
        let view = Arc::new(view);
        match self.inner.write() {
            Ok(mut slot) => *slot = view,
            Err(poisoned) => *poisoned.into_inner() = view,
        }
    }

    pub fn current(&self) -> Arc<StatusView> {
        match self.inner.read() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// Parses state names into a set; errors on the first unknown name.
pub fn states_of(names: &[String]) -> Result<BTreeSet<NodeState>, String> {
    names
        .iter()
        .map(|n| parse_state(n).ok_or_else(|| format!("unknown state {n:?}")))
        .collect()
}

/// Parses kind names into a set; errors on the first unknown name.
pub fn kinds_of(names: &[String]) -> Result<BTreeSet<NodeKind>, String> {
    names
        .iter()
        .map(|n| parse_kind(n).ok_or_else(|| format!("unknown kind {n:?}")))
        .collect()
}
