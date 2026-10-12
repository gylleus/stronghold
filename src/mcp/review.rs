//! The review queue's MCP tools: `mark_pr_ready` for agents that finished a PR, and
//! `review_queue` for Karl (through the Queen) to see what is waiting.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::control::protocol::Params;
use crate::control::server::now_ms;
use crate::mcp::backend::{CallError, Control};
use crate::mcp::tools::ToolOutput;
use crate::review::github::{Ci, PrState};
use crate::review::mark::{self, MarkSource, Origin, ReadyMark};
use crate::review::queue::ReviewQueue;
use crate::review::view::{queue_json, rows};

/// What the review tools need from the `myrstack mcp` process.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewEnv {
    /// The Orca identity of the agent this MCP server runs under.
    pub origin: Origin,
    /// Where marks wait while the app isn't running; `None` refuses to queue.
    pub pending: Option<PathBuf>,
    /// The app's saved queue, read when the app isn't running.
    pub queue_file: Option<PathBuf>,
    /// Where `notify` queues notifications while the app isn't running; `None` refuses to
    /// queue.
    pub notify_pending: Option<PathBuf>,
}

impl ReviewEnv {
    /// This process's identity and the data dir's files (none in unit tests).
    pub fn from_env() -> Self {
        Self {
            origin: Origin::from_env(),
            pending: mark::default_pending_path(),
            queue_file: (!cfg!(test)).then(ReviewQueue::default_path),
            notify_pending: crate::notify::pending::default_path(),
        }
    }
}

/// The review tools, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "mark_pr_ready",
            "title": "Mark PR ready for review",
            "description": "Tells Myrstack (Karl's fleet dashboard) that a pull request is ready \
                for Karl's review, so it shows in his review queue attributed to your terminal. \
                Call it right after sending merge_ready for a gate-ready PR, or when setting the \
                ✅ ready marker. It only records locally: nothing is posted to GitHub or sent to \
                other agents. Works even when Myrstack isn't running (it shows on start).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pr_url": { "type": "string", "description": "The PR URL, https://github.com/<org>/<repo>/pull/<n>." },
                    "title": { "type": "string", "description": "Short title (GitHub's title is used once Myrstack reads the PR)." },
                    "summary": { "type": "string", "description": "One line: what is ready and what was verified." },
                },
                "required": ["pr_url"],
            },
            "annotations": {
                "title": "Mark PR ready for review",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        }),
        json!({
            "name": "review_queue",
            "title": "Review queue",
            "description": "PRs agents marked ready for Karl's review that he hasn't reviewed \
                yet, newest first: repo, number, title, the agent that marked it (with its node \
                id), CI, the Ankra verdict (stale when it reviewed an older commit than the \
                head), the ready marker, and age. Merged or closed PRs drop off.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": {
                "title": "Review queue",
                "readOnlyHint": true,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        }),
    ]
}

/// Runs a review tool; `None` for other tools.
pub fn call(
    control: &mut dyn Control,
    env: &ReviewEnv,
    name: &str,
    args: &Value,
) -> Option<ToolOutput> {
    Some(match name {
        "mark_pr_ready" => mark_pr_ready(control, env, args),
        "review_queue" => review_queue(control, env),
        _ => return None,
    })
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        text,
        structured: None,
        is_error: true,
    }
}

fn mark_pr_ready(control: &mut dyn Control, env: &ReviewEnv, args: &Value) -> ToolOutput {
    let p = Params::new(args);
    let fields = (|| {
        let url = p
            .str("pr_url")?
            .or(p.str("url")?)
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| crate::control::ControlError::invalid_params("missing \"pr_url\""))?;
        Ok::<_, crate::control::ControlError>((url, p.str("title")?, p.str("summary")?))
    })();
    let (url, title, summary) = match fields {
        Ok(f) => f,
        Err(e) => return error(format!("Invalid arguments: {}", e.message)),
    };
    let mark = match ReadyMark::new(
        url,
        title,
        summary,
        env.origin.clone(),
        MarkSource::Tool,
        now_ms(),
    ) {
        Ok(m) => m,
        Err(e) => return error(format!("Invalid arguments: {e}")),
    };
    match mark::submit(control, env.pending.as_deref(), &mark) {
        Ok(submitted) => {
            let (queued, reply) = match &submitted {
                mark::Submitted::App(reply) => (false, reply.clone()),
                mark::Submitted::Queued(_) => (true, Value::Null),
            };
            ToolOutput {
                text: crate::review::submitted_text(&mark, &submitted),
                structured: Some(json!({
                    "url": mark.pr.url(),
                    "origin": mark.origin,
                    "queued": queued,
                    "app": reply,
                })),
                is_error: false,
            }
        }
        Err(e) => error(format!("Couldn't record the mark: {e}")),
    }
}

fn review_queue(control: &mut dyn Control, env: &ReviewEnv) -> ToolOutput {
    let now = now_ms();
    let (result, saved) = match control.call("review_queue", Value::Null) {
        Ok(result) => (result, false),
        Err(CallError::NotRunning) => match &env.queue_file {
            Some(path) => {
                let queue = ReviewQueue::load(path);
                let list = rows(&queue, now, |_| None);
                (queue_json(&list, None), true)
            }
            None => return error(crate::mcp::backend::NOT_RUNNING.into()),
        },
        Err(e) => return error(format!("Myrstack error: {e}")),
    };
    let list: Vec<Value> = result
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    if saved {
        out.push(
            "Note: Myrstack isn't running, so this is its saved queue with GitHub state as last \
             read."
                .to_owned(),
        );
    }
    if list.is_empty() {
        out.push("Nothing is waiting for review.".into());
    } else {
        out.push(format!("Ready for review ({}):", list.len()));
        out.extend(list.iter().map(|e| entry_line(e, now)));
    }
    if result.pointer("/github/available") == Some(&Value::Bool(false)) {
        let why = result
            .pointer("/github/error")
            .and_then(Value::as_str)
            .unwrap_or("gh failed");
        out.push(format!("GitHub status unavailable: {why}"));
    }
    let mut structured = result;
    structured["saved"] = json!(saved);
    ToolOutput {
        text: out.join("\n"),
        structured: Some(structured),
        is_error: false,
    }
}

/// One row as a chat line.
fn entry_line(entry: &Value, now: i64) -> String {
    let row: Option<ReviewRowText> = serde_json::from_value(entry.clone()).ok();
    let Some(r) = row else {
        return format!("- {entry}");
    };
    let mut parts = Vec::new();
    let agent = match (&r.agent, &r.agent_handle) {
        (Some(a), _) => format!("{} `{}`", a.label, a.id),
        (None, Some(h)) => format!("terminal {h} (not in the farm)"),
        (None, None) => "unknown agent".into(),
    };
    parts.push(agent);
    if let Some(state) = r.state {
        if state != PrState::Open {
            parts.push(state.name().to_owned());
        }
    } else if let Some(e) = &r.error {
        parts.push(format!("GitHub: {e}"));
    } else {
        parts.push("GitHub not read yet".into());
    }
    if let Some(ci) = r.ci {
        parts.push(ci.describe());
    }
    if r.state.is_some() {
        parts.push(match &r.ankra_verdict {
            None => "no Ankra review".into(),
            Some(v) if r.ankra_on_head == Some(false) => format!("Ankra: {v} (stale, not on head)"),
            Some(v) => format!("Ankra: {v}"),
        });
        parts.push(match r.marker.as_deref() {
            None => "no ready marker".into(),
            Some(kind) => {
                let base = if kind == "ready" {
                    "✅ ready marker"
                } else {
                    "⏸ not-ready marker"
                };
                if r.marker_on_head == Some(false) {
                    format!("{base} (stale)")
                } else {
                    base.to_owned()
                }
            }
        });
    }
    if let Some(d) = &r.review_decision {
        parts.push(format!("review: {}", d.to_lowercase().replace('_', " ")));
    }
    if r.updated_since_review {
        parts.push("pushed to since Karl's review".into());
    }
    parts.push(format!(
        "marked {} ago",
        crate::quota::format_duration(now - r.marked_ms)
    ));
    let title = if r.title.is_empty() {
        String::new()
    } else {
        format!(" {}", r.title)
    };
    let mut line = format!("- {}#{}{title} — {}", r.repo, r.number, parts.join(" · "));
    if let Some(s) = &r.summary {
        line.push_str(&format!("\n  {s}"));
    }
    line.push_str(&format!("\n  {}", r.url));
    line
}

/// The fields of a serialized [`crate::review::ReviewRow`] the text uses.
#[derive(serde::Deserialize)]
struct ReviewRowText {
    url: String,
    repo: String,
    number: u64,
    #[serde(default)]
    title: String,
    summary: Option<String>,
    agent: Option<AgentText>,
    agent_handle: Option<String>,
    marked_ms: i64,
    state: Option<PrState>,
    ci: Option<Ci>,
    ankra_verdict: Option<String>,
    ankra_on_head: Option<bool>,
    marker: Option<String>,
    marker_on_head: Option<bool>,
    review_decision: Option<String>,
    error: Option<String>,
    #[serde(default)]
    updated_since_review: bool,
}

#[derive(serde::Deserialize)]
struct AgentText {
    id: String,
    label: String,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::control::server::{Backend, ScreenRead, ScreenReader};
    use crate::control::{
        CommandHandler, ControlCommand, ControlError, ControlServer, SharedStatus,
    };
    use crate::mcp::{McpServer, SocketControl};
    use crate::review::github::{Ci, PrState, PrStatus};
    use crate::review::view::{AgentRef, ReviewRow};
    use crate::review::{ReviewCommand, ReviewQueue};

    const URL: &str = "https://github.com/depict-org/realtime/pull/255";

    /// Records every call; `running: false` answers like an app that isn't running.
    struct Fake {
        calls: Vec<(String, Value)>,
        running: bool,
        reply: Value,
    }

    impl Fake {
        fn new(running: bool, reply: Value) -> Self {
            Self {
                calls: Vec::new(),
                running,
                reply,
            }
        }
    }

    impl Control for Fake {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
            self.calls.push((method.to_owned(), params));
            if self.running {
                Ok(self.reply.clone())
            } else {
                Err(CallError::NotRunning)
            }
        }
    }

    /// An Orca agent terminal's environment.
    fn agent_env(dir: &std::path::Path) -> ReviewEnv {
        let env: HashMap<&str, &str> = [
            ("ORCA_TERMINAL_HANDLE", "term_442fcb9f"),
            ("ORCA_PANE_KEY", "16b3a609:e499ad8b"),
            ("ORCA_TAB_ID", "16b3a609"),
            ("HOME", "/Users/x"),
        ]
        .into();
        ReviewEnv {
            origin: Origin::from_vars(|k| env.get(k).map(|v| (*v).to_owned())),
            pending: Some(dir.join(mark::PENDING_FILE)),
            queue_file: Some(dir.join(crate::review::queue::QUEUE_FILE)),
            notify_pending: None,
        }
    }

    fn call_tool<C: Control>(server: &mut McpServer<C>, name: &str, args: Value) -> Value {
        let reply = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": name, "arguments": args},
            }))
            .unwrap();
        reply["result"].clone()
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn tools_are_listed_with_their_hints() {
        let mut server = McpServer::new(Fake::new(true, json!({})));
        let list = server
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
        let mark = find("mark_pr_ready");
        assert_eq!(mark["annotations"]["destructiveHint"], false);
        assert_eq!(mark["annotations"]["readOnlyHint"], false);
        assert_eq!(mark["inputSchema"]["required"], json!(["pr_url"]));
        assert_eq!(find("review_queue")["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn mark_pr_ready_sends_the_callers_orca_identity() {
        let dir = tempfile::tempdir().unwrap();
        let reply = json!({"url": URL, "outcome": "new", "count": 1, "agent": {"id": "term:term_442fcb9f", "label": "realtime-255"}});
        let mut server =
            McpServer::new(Fake::new(true, reply)).with_review_env(agent_env(dir.path()));
        let r = call_tool(
            &mut server,
            "mark_pr_ready",
            json!({"pr_url": format!("{URL}/files"), "title": "gate legacy config", "summary": "CI green"}),
        );
        assert_eq!(r["isError"], false, "{}", text(&r));
        assert_eq!(
            text(&r),
            "Marked depict-org/realtime#255 ready for review from term_442fcb9f, shown under \
             realtime-255. Myrstack's review queue now lists 1 PR."
        );
        let (method, params) = &server.control().calls[0];
        assert_eq!(method, "pr_ready");
        assert_eq!(
            params,
            &json!({
                "url": URL,
                "via": "tool",
                "title": "gate legacy config",
                "summary": "CI green",
                "origin": {
                    "terminal_handle": "term_442fcb9f",
                    "pane_key": "16b3a609:e499ad8b",
                    "tab_id": "16b3a609",
                },
            })
        );
        assert!(!dir.path().join(mark::PENDING_FILE).exists());

        let r = call_tool(
            &mut server,
            "mark_pr_ready",
            json!({"pr_url": "https://github.com/o/r/issues/1"}),
        );
        assert_eq!(r["isError"], true);
        assert!(text(&r).contains("not a GitHub pull request URL"));
        let r = call_tool(&mut server, "mark_pr_ready", json!({}));
        assert_eq!(text(&r), "Invalid arguments: missing \"pr_url\"");
    }

    /// With the app down the mark waits in the pending file, and the next app takes it.
    #[test]
    fn mark_pr_ready_queues_when_the_app_is_down() {
        let dir = tempfile::tempdir().unwrap();
        let env = agent_env(dir.path());
        let pending = env.pending.clone().unwrap();
        let mut server = McpServer::new(Fake::new(false, Value::Null)).with_review_env(env);
        let r = call_tool(&mut server, "mark_pr_ready", json!({"pr_url": URL}));
        assert_eq!(r["isError"], false, "{}", text(&r));
        assert!(text(&r).contains("Myrstack isn't running"), "{}", text(&r));
        assert_eq!(r["structuredContent"]["queued"], true);

        let taken = mark::take_pending(&pending);
        assert_eq!(taken.len(), 1);
        assert_eq!(
            taken[0].origin.terminal_handle.as_deref(),
            Some("term_442fcb9f")
        );
        assert_eq!(taken[0].source, MarkSource::Tool);
        let mut queue = ReviewQueue::default();
        queue.add(&taken[0]);
        assert_eq!(queue.visible(taken[0].at_ms).len(), 1);
        assert_eq!(
            queue.take_new(taken[0].at_ms).len(),
            1,
            "it notifies when the app starts"
        );

        let mut no_dir =
            McpServer::new(Fake::new(false, Value::Null)).with_review_env(ReviewEnv::default());
        let r = call_tool(&mut no_dir, "mark_pr_ready", json!({"pr_url": URL}));
        assert_eq!(r["isError"], true);
    }

    fn row(ci: Ci, ankra_on_head: Option<bool>) -> ReviewRow {
        ReviewRow {
            url: URL.into(),
            repo: "depict-org/realtime".into(),
            number: 255,
            title: "recs: gate legacy config retirement".into(),
            summary: Some("PR #255 green".into()),
            agent: Some(AgentRef {
                id: crate::model::NodeId::terminal("term_a"),
                label: "realtime-255".into(),
                live: true,
            }),
            agent_handle: None,
            source: "bus",
            marked_ms: crate::control::server::now_ms() - 2 * 3_600_000,
            state: Some(PrState::Open),
            ci: Some(ci),
            head_sha: Some("92b1c94".into()),
            ankra_verdict: Some("comments".into()),
            ankra_on_head,
            marker: Some("ready"),
            marker_on_head: Some(false),
            review_decision: Some("REVIEW_REQUIRED".into()),
            error: None,
            updated_since_review: false,
        }
    }

    #[test]
    fn review_queue_lists_rows_as_text() {
        let reply = queue_json(
            &[row(Ci::Passing { total: 9 }, Some(false))],
            Some("gh auth login"),
        );
        let mut server = McpServer::new(Fake::new(true, reply));
        let r = call_tool(&mut server, "review_queue", json!({}));
        assert_eq!(server.control().calls[0].0, "review_queue");
        let t = text(&r);
        assert!(t.starts_with("Ready for review (1):\n- depict-org/realtime#255 recs: gate legacy config retirement — realtime-255 `term:term_a` · CI green (9 checks) · Ankra: comments (stale, not on head) · ✅ ready marker (stale) · review: review required · marked 2h"), "{t}");
        assert!(t.contains(&format!("\n  {URL}")), "{t}");
        assert!(
            t.ends_with("GitHub status unavailable: gh auth login"),
            "{t}"
        );
        assert_eq!(r["structuredContent"]["count"], 1);
        assert_eq!(r["structuredContent"]["saved"], false);

        let mut empty = McpServer::new(Fake::new(true, queue_json(&[], None)));
        assert_eq!(
            text(&call_tool(&mut empty, "review_queue", json!({}))),
            "Nothing is waiting for review."
        );
    }

    #[test]
    fn review_queue_reads_the_saved_queue_when_the_app_is_down() {
        let dir = tempfile::tempdir().unwrap();
        let env = agent_env(dir.path());
        let mut queue = ReviewQueue::default();
        let m = ReadyMark::new(
            URL,
            None,
            None,
            env.origin.clone(),
            MarkSource::Cli,
            crate::control::server::now_ms(),
        )
        .unwrap();
        queue.add(&m);
        queue.apply_status(
            URL,
            Ok(PrStatus {
                number: 255,
                title: "recs: gate legacy config retirement".into(),
                url: URL.into(),
                state: PrState::Open,
                head_sha: "92b1c94".into(),
                ci: Ci::Failing {
                    failed: 1,
                    total: 4,
                },
                ankra: None,
                marker: None,
                review_decision: None,
                merged_at: None,
                fetched_at_ms: 0,
            }),
            m.at_ms,
        );
        queue.save(env.queue_file.as_ref().unwrap()).unwrap();
        let mut server = McpServer::new(Fake::new(false, Value::Null)).with_review_env(env);
        let r = call_tool(&mut server, "review_queue", json!({}));
        let t = text(&r);
        assert!(t.starts_with("Note: Myrstack isn't running"), "{t}");
        assert!(t.contains("#255 recs: gate legacy config retirement — terminal term_442fcb9f (not in the farm) · CI failing (1 of 4 checks) · no Ankra review · no ready marker"), "{t}");
        assert_eq!(r["structuredContent"]["saved"], true);
    }

    /// A stand-in for the app: answers review commands from a queue.
    struct QueueApp(Mutex<ReviewQueue>);

    impl CommandHandler for QueueApp {
        fn execute(&self, command: ControlCommand) -> Result<Value, ControlError> {
            let ControlCommand::Review(command) = command else {
                return Err(ControlError::failed("not a review command"));
            };
            let mut queue = self.0.lock().unwrap();
            match command {
                ReviewCommand::Ready(mark) => {
                    queue.add(&mark);
                    Ok(
                        json!({"url": mark.pr.url(), "count": queue.visible(mark.at_ms).len(), "agent": null}),
                    )
                }
                ReviewCommand::Queue => {
                    let now = crate::control::server::now_ms();
                    Ok(queue_json(
                        &crate::review::view::rows(&queue, now, |_| None),
                        None,
                    ))
                }
                _ => Err(ControlError::failed("unexpected")),
            }
        }
    }

    struct NoScreens;

    impl ScreenReader for NoScreens {
        fn read_screen(&self, _: &str) -> Result<ScreenRead, String> {
            Err("none".into())
        }
    }

    /// The tool, the control socket and the app's command handler, end to end.
    #[test]
    fn mark_pr_ready_reaches_the_app_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let app = Arc::new(QueueApp(Mutex::new(ReviewQueue::default())));
        let server = ControlServer::start(
            &path,
            Backend {
                status: SharedStatus::new(),
                screens: Arc::new(NoScreens),
                commands: app.clone(),
            },
        )
        .unwrap();
        let mut mcp =
            McpServer::new(SocketControl::new(path.clone())).with_review_env(agent_env(dir.path()));
        let r = call_tool(
            &mut mcp,
            "mark_pr_ready",
            json!({"pr_url": URL, "summary": "green"}),
        );
        assert_eq!(r["isError"], false, "{}", text(&r));
        assert!(text(&r).ends_with("now lists 1 PR."), "{}", text(&r));
        {
            let queue = app.0.lock().unwrap();
            let entry = queue.get(URL).unwrap();
            assert_eq!(
                entry.origin.terminal_handle.as_deref(),
                Some("term_442fcb9f")
            );
            assert_eq!(entry.origin.pane_key.as_deref(), Some("16b3a609:e499ad8b"));
            assert_eq!(entry.summary.as_deref(), Some("green"));
            assert_eq!(entry.source, MarkSource::Tool);
        }
        let r = call_tool(&mut mcp, "review_queue", json!({}));
        assert!(
            text(&r).starts_with("Ready for review (1):"),
            "{}",
            text(&r)
        );
        drop(server);
    }
}
