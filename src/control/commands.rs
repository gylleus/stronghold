//! Action methods: parsed into [`ControlCommand`]s and carried out by a [`CommandHandler`].
//!
//! In the app the handler is [`AppCommands`]: it queues the command for the UI thread (polled in
//! `logic()`), wakes the UI and blocks the connection's thread until the app replies.

use std::collections::BTreeSet;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serde_json::{Value, json};

use crate::broadcast::{BroadcastPlan, BroadcastSender};
use crate::control::protocol::{ControlError, ErrorCode, Params};
use crate::control::view::{kinds_of, states_of};
use crate::model::{NodeKind, NodeState};
use crate::ui::sidebar::ClanFilter;

/// How long a terminal send may take: Orca waits up to 30 s for the agent, the runner 45 s.
pub const ACTION_REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a close may take: its steps run one after another.
pub const CLOSE_REPLY_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a manual whip may take: each nudge is an orca call on the whip thread.
pub const WHIP_REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a spawn may take: worktree creation, the agent's start and the prompt check.
/// Stays under the control client's own timeout.
pub const SPAWN_REPLY_TIMEOUT: Duration = Duration::from_secs(190);
/// How long the app may take to apply a dashboard change.
pub const UI_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// A single key sent to an agent's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKey {
    Enter,
    Escape,
    /// Ctrl-C.
    Interrupt,
}

/// Changes to the dashboard filter; `None` fields are left as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterChange {
    /// Start from the default (show everything) before applying the other fields.
    pub clear: bool,
    pub repos: Option<BTreeSet<String>>,
    pub states: Option<BTreeSet<NodeState>>,
    pub kinds: Option<BTreeSet<NodeKind>>,
    pub text: Option<String>,
    pub show_loose: Option<bool>,
    pub show_released: Option<bool>,
}

impl FilterChange {
    pub fn apply(&self, filter: &mut ClanFilter) {
        if self.clear {
            *filter = ClanFilter::default();
        }
        if let Some(repos) = &self.repos {
            filter.repos = repos.clone();
        }
        if let Some(states) = &self.states {
            filter.states = states.clone();
        }
        if let Some(kinds) = &self.kinds {
            filter.kinds = kinds.clone();
        }
        if let Some(text) = &self.text {
            filter.text = text.clone();
        }
        if let Some(v) = self.show_loose {
            filter.show_loose = v;
        }
        if let Some(v) = self.show_released {
            filter.show_released = v;
        }
    }
}

/// Something the API asks the app to do. `agent` fields are node ids, terminal handles or
/// labels, resolved by the app against its current snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCommand {
    /// Inbox, drafting and relay requests, restricted to the persisted Envoy terminal.
    Envoy(crate::envoy::control::EnvoyCommand),
    /// Type `text` into the agent's terminal, then Enter when `enter`. `from` is the sender's
    /// terminal handle (`from`, or the caller's `origin`), `None` for the King or Karl.
    Send {
        agent: String,
        text: String,
        enter: bool,
        from: Option<String>,
    },
    Key {
        agent: String,
        key: TerminalKey,
    },
    /// A bus message to the coordinators of `runs` (`None`: every run).
    Broadcast {
        text: String,
        runs: Option<Vec<String>>,
    },
    WhipSettings {
        enabled: Option<bool>,
        dry_run: Option<bool>,
        notify: Option<bool>,
    },
    /// Select a node (`None` clears the selection) without moving the farm.
    Select {
        agent: Option<String>,
    },
    /// Select several nodes (the first is the primary) without moving the farm.
    SelectMany {
        agents: Vec<String>,
    },
    /// Select a node and pan to it at the current zoom; `raise` also brings the window forward.
    Focus {
        agent: String,
        raise: bool,
    },
    Filter(FilterChange),
    OpenWhipSettings,
    /// Close (kill) the agents' sessions: worker-stop for active dispatches, then
    /// `terminal close`. With `dry_run` only the plan is returned.
    Close {
        agents: Vec<String>,
        whole_tab: bool,
        dry_run: bool,
    },
    /// Park `agents` in the resting area ([`crate::park`]), or with `park` false wake them.
    Park {
        agents: Vec<String>,
        park: bool,
    },
    /// Nudge `agents` now (live, bypassing threshold and cooldown), or with `stalled` run an
    /// immediate whip tick under the normal rules.
    Whip {
        agents: Vec<String>,
        stalled: bool,
    },
    /// Review queue methods (`pr_ready`, `review_queue`, …), answered by the app.
    Review(crate::review::ReviewCommand),
    /// Start a new agent session (existing worktree, new worktree, or a run's worker).
    Spawn(crate::spawn::SpawnArgs),
    /// A notification from an agent to Karl, through the app's dispatcher.
    Notify(crate::notify::agent::AgentNotice),
    /// An agent's question to Karl (`ask_karl`): notifies him and shows the agent waiting.
    Ask(crate::ask::AskRequest),
    /// An agent ending its own session (`finish_session`), or with `dry_run` checking whether
    /// it may.
    Finish(crate::finish::FinishRequest),
    /// A message between agents happened elsewhere (the Envoy's relays, say): draw it.
    AgentMessage(crate::traffic::AgentMessage),
    /// An MCP tool call acted on agents: draw it as a beam from its caller.
    ToolCall(crate::traffic::beam::ToolCall),
}

impl ControlCommand {
    /// The command for an action method; `None` when `method` is not an action.
    pub fn parse(method: &str, params: &Value) -> Option<Result<Self, ControlError>> {
        let now = crate::control::server::now_ms();
        if let Some(envoy) = crate::envoy::control::EnvoyCommand::parse(method, params) {
            return Some(envoy.map(Self::Envoy));
        }
        if let Some(review) = crate::review::ReviewCommand::parse(method, params, now) {
            return Some(review.map(Self::Review));
        }
        let p = Params::new(params);
        let agent = || p.required_str("id").map(str::to_owned);
        let key = |key| {
            Ok(Self::Key {
                agent: agent()?,
                key,
            })
        };
        let result = match method {
            "send" => (|| {
                let text = p
                    .str("text")?
                    .ok_or_else(|| ControlError::invalid_params("missing \"text\""))?;
                let enter = p.bool("enter")?.unwrap_or(true);
                if text.is_empty() && !enter {
                    return Err(ControlError::invalid_params(
                        "empty \"text\" without enter sends nothing",
                    ));
                }
                let from = match p.str("from")? {
                    Some(from) => Some(from.to_owned()),
                    None => crate::finish::origin_of(params)?.terminal_handle,
                };
                Ok(Self::Send {
                    agent: agent()?,
                    text: text.to_owned(),
                    enter,
                    from,
                })
            })(),
            "enter" => key(TerminalKey::Enter),
            "esc" | "escape" => key(TerminalKey::Escape),
            "interrupt" => key(TerminalKey::Interrupt),
            "broadcast" => (|| {
                let text = p.required_str("text")?.to_owned();
                let all = p.bool("all")?.unwrap_or(false);
                let runs = p.strings("runs")?;
                let runs = match (all, runs) {
                    (true, None) => None,
                    (false, Some(runs)) if !runs.is_empty() => Some(runs),
                    (true, Some(_)) => {
                        return Err(ControlError::invalid_params(
                            "give either \"runs\" or \"all\": true, not both",
                        ));
                    }
                    (false, _) => {
                        return Err(ControlError::invalid_params(
                            "\"runs\" (run ids) or \"all\": true is required",
                        ));
                    }
                };
                Ok(Self::Broadcast { text, runs })
            })(),
            "whip_settings" => (|| {
                let command = Self::WhipSettings {
                    enabled: p.bool("enabled")?,
                    dry_run: p.bool("dry_run")?,
                    notify: p.bool("notify")?,
                };
                Ok(command)
            })(),
            "select" => (|| {
                let agent = p.str("id")?.map(str::to_owned);
                let Some(ids) = p.strings("ids")? else {
                    return Ok(Self::Select { agent });
                };
                let mut agents: Vec<String> = agent.into_iter().chain(ids).collect();
                agents.retain(|a| !a.trim().is_empty());
                Ok(match agents.len() {
                    0 => Self::Select { agent: None },
                    1 => Self::Select {
                        agent: agents.pop(),
                    },
                    _ => Self::SelectMany { agents },
                })
            })(),
            "focus" => (|| {
                Ok(Self::Focus {
                    agent: agent()?,
                    raise: p.bool("raise")?.unwrap_or(false),
                })
            })(),
            "filter" => parse_filter(&p).map(Self::Filter),
            "open_whip_settings" => Ok(Self::OpenWhipSettings),
            "close" => (|| {
                let mut agents = p.strings("ids")?.unwrap_or_default();
                if let Some(id) = p.str("id")? {
                    agents.insert(0, id.to_owned());
                }
                agents.retain(|a| !a.trim().is_empty());
                if agents.is_empty() {
                    return Err(ControlError::invalid_params("missing \"id\" or \"ids\""));
                }
                Ok(Self::Close {
                    agents,
                    whole_tab: p.bool("whole_tab")?.unwrap_or(false),
                    dry_run: p.bool("dry_run")?.unwrap_or(false),
                })
            })(),
            "park" => (|| {
                let mut agents = p.strings("ids")?.unwrap_or_default();
                if let Some(id) = p.str("id")? {
                    agents.insert(0, id.to_owned());
                }
                agents.retain(|a| !a.trim().is_empty());
                if agents.is_empty() {
                    return Err(ControlError::invalid_params("missing \"id\" or \"ids\""));
                }
                Ok(Self::Park {
                    agents,
                    park: p.bool("park")?.unwrap_or(true),
                })
            })(),
            "whip" if ["id", "ids", "stalled"].iter().any(|k| p.has(k)) => (|| {
                let mut agents = p.strings("ids")?.unwrap_or_default();
                if let Some(id) = p.str("id")? {
                    agents.insert(0, id.to_owned());
                }
                agents.retain(|a| !a.trim().is_empty());
                let stalled = p.bool("stalled")?.unwrap_or(false);
                match (stalled, agents.is_empty()) {
                    (true, false) => Err(ControlError::invalid_params(
                        "give either \"ids\" or \"stalled\": true, not both",
                    )),
                    (false, true) => Err(ControlError::invalid_params(
                        "\"ids\" (agent ids) or \"stalled\": true is required",
                    )),
                    _ => Ok(Self::Whip { agents, stalled }),
                }
            })(),
            "spawn" => crate::spawn::SpawnArgs::parse(params).map(Self::Spawn),
            "notify" => crate::notify::agent::AgentNotice::from_params(params).map(Self::Notify),
            "ask" => crate::ask::AskRequest::from_params(params).map(Self::Ask),
            "finish" => crate::finish::FinishRequest::from_params(params).map(Self::Finish),
            "agent_message" => {
                crate::traffic::AgentMessage::from_params(params).map(Self::AgentMessage)
            }
            "tool_call" => crate::traffic::beam::ToolCall::from_params(params).map(Self::ToolCall),
            _ => return None,
        };
        Some(result)
    }

    /// How long the connection waits for the app's reply.
    pub fn reply_timeout(&self) -> Duration {
        match self {
            Self::Send { .. } | Self::Key { .. } | Self::Broadcast { .. } => ACTION_REPLY_TIMEOUT,
            Self::Close { dry_run: false, .. } => CLOSE_REPLY_TIMEOUT,
            Self::Whip { .. } => WHIP_REPLY_TIMEOUT,
            Self::Spawn(_) => SPAWN_REPLY_TIMEOUT,
            Self::Envoy(_) => ACTION_REPLY_TIMEOUT,
            _ => UI_REPLY_TIMEOUT,
        }
    }
}

fn parse_filter(p: &Params<'_>) -> Result<FilterChange, ControlError> {
    let invalid = ControlError::invalid_params;
    let change = FilterChange {
        clear: p.bool("clear")?.unwrap_or(false),
        repos: p.strings("repos")?.map(|r| r.into_iter().collect()),
        states: p
            .strings("states")?
            .map(|s| states_of(&s))
            .transpose()
            .map_err(invalid)?,
        kinds: p
            .strings("kinds")?
            .map(|k| kinds_of(&k))
            .transpose()
            .map_err(invalid)?,
        text: p.str("text")?.map(str::to_owned),
        show_loose: p.bool("show_loose")?,
        show_released: p.bool("show_released")?,
    };
    if change == FilterChange::default() {
        return Err(invalid(
            "nothing to change; give clear, repos, states, kinds, text, show_loose or show_released"
                .into(),
        ));
    }
    Ok(change)
}

/// Carries out action commands; called on a connection's thread.
pub trait CommandHandler: Send + Sync {
    fn execute(&self, command: ControlCommand) -> Result<Value, ControlError>;
}

/// Where the app sends a command's outcome.
#[derive(Debug)]
pub struct Reply(Sender<Result<Value, ControlError>>);

impl Reply {
    /// Sends the outcome; a caller that timed out and left is ignored.
    pub fn send(self, result: Result<Value, ControlError>) {
        let _ = self.0.send(result);
    }
}

/// A command waiting for the app, with where to send its outcome.
#[derive(Debug)]
pub struct PendingCommand {
    pub command: ControlCommand,
    pub reply: Reply,
}

/// Wakes the UI so `logic()` runs and picks up queued commands.
pub type Wake = Box<dyn Fn() + Send + Sync>;

/// [`CommandHandler`] that hands commands to the app over a channel.
pub struct AppCommands {
    tx: Sender<PendingCommand>,
    wake: Option<Wake>,
}

impl AppCommands {
    /// The handler and the receiver the app polls.
    pub fn channel(wake: Option<Wake>) -> (Self, Receiver<PendingCommand>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (Self { tx, wake }, rx)
    }
}

impl CommandHandler for AppCommands {
    fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
        let timeout = command.reply_timeout();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(PendingCommand {
                command,
                reply: Reply(reply_tx),
            })
            .map_err(|_| ControlError::new(ErrorCode::Unavailable, "the app is shutting down"))?;
        if let Some(wake) = &self.wake {
            wake();
        }
        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(ControlError::new(
                ErrorCode::Timeout,
                format!("no reply from the app within {} s", timeout.as_secs()),
            )),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(ControlError::new(
                ErrorCode::Unavailable,
                "the app dropped the command",
            )),
        }
    }
}

/// Sends a broadcast plan's deliveries one by one and reports each outcome.
pub fn deliver_broadcast(sender: &mut dyn BroadcastSender, plan: &BroadcastPlan) -> Value {
    let mut sent = 0;
    let deliveries: Vec<Value> = plan
        .deliveries
        .iter()
        .map(|d| match sender.send(&d.message) {
            Ok(message_id) => {
                sent += 1;
                json!({
                    "coordinator": d.coordinator_handle,
                    "runs": d.run_ids,
                    "ok": true,
                    "message_id": message_id,
                })
            }
            Err(error) => json!({
                "coordinator": d.coordinator_handle,
                "runs": d.run_ids,
                "ok": false,
                "error": error,
            }),
        })
        .collect();
    let skipped: Vec<Value> = plan
        .skipped
        .iter()
        .map(|s| json!({"run_id": s.run_id, "reason": s.reason.describe()}))
        .collect();
    json!({
        "sent": sent,
        "failed": plan.deliveries.len() - sent,
        "deliveries": deliveries,
        "skipped": skipped,
    })
}
