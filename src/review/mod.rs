//! The review queue: PRs agents finished and marked ready, shown in the King panel with their
//! GitHub state until the user reviews them or they merge.
//!
//! Marks come from the `mark_pr_ready` MCP tool and `stronghold pr-ready` (both carry the
//! caller's Orca identity), and from `merge_ready` bus messages (listed only with a `✅` marker
//! on the PR's head; see [`queue`]). The app reads each PR with
//! `gh pr view` (read-only). Docs: `docs/control-api.md` (`pr_ready`, `review_queue`),
//! `docs/mcp.md`.

pub mod bus;
pub mod control;
pub mod github;
pub mod mark;
pub mod poller;
pub mod pr;
pub mod queue;
pub mod view;

use anyhow::{Result, bail};
use serde_json::Value;

pub use control::ReviewCommand;
pub use mark::{MarkSource, Origin, ReadyMark, Submitted};
pub use pr::PrRef;
pub use queue::{Entry, ReviewQueue};
pub use view::{AgentRef, ReviewRow};

const USAGE: &str = "usage: stronghold pr-ready <pr url> [--title <title>] [--summary <text>]\n\
Marks a PR ready for review in Stronghold's review queue, attributed to this Orca terminal. \
It only records locally; nothing is sent to GitHub.";

/// `stronghold pr-ready <url> [--title …] [--summary …]`; `None` for other subcommands.
pub fn run_cli(args: &[String]) -> Option<Result<()>> {
    let (cmd, rest) = args.split_first()?;
    (cmd == "pr-ready").then(|| pr_ready_cli(rest))
}

/// The parsed `pr-ready` arguments: url, title, summary.
pub fn parse_cli_args(args: &[String]) -> Result<(String, Option<String>, Option<String>)> {
    let mut url = None;
    let (mut title, mut summary) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "-h" | "--help" => bail!("{USAGE}"),
            "--title" => title = Some(value("--title")?),
            "--summary" => summary = Some(value("--summary")?),
            s if s.starts_with("--title=") => title = Some(s["--title=".len()..].to_owned()),
            s if s.starts_with("--summary=") => {
                summary = Some(s["--summary=".len()..].to_owned());
            }
            s if s.starts_with('-') => bail!("unknown option {s}\n{USAGE}"),
            s if url.is_none() => url = Some(s.to_owned()),
            s => bail!("unexpected argument {s:?}\n{USAGE}"),
        }
    }
    let Some(url) = url else { bail!("{USAGE}") };
    Ok((url, title, summary))
}

fn pr_ready_cli(args: &[String]) -> Result<()> {
    let (url, title, summary) = parse_cli_args(args)?;
    let mark = ReadyMark::new(
        &url,
        title.as_deref(),
        summary.as_deref(),
        Origin::from_env(),
        MarkSource::Cli,
        crate::control::server::now_ms(),
    )
    .map_err(anyhow::Error::msg)?;
    let mut control = crate::mcp::SocketControl::new(crate::control::socket_path());
    let pending = mark::default_pending_path();
    let submitted =
        mark::submit(&mut control, pending.as_deref(), &mark).map_err(anyhow::Error::msg)?;
    println!("{}", submitted_text(&mark, &submitted));
    Ok(())
}

/// What the tool and the CLI answer after a mark.
pub fn submitted_text(mark: &ReadyMark, submitted: &Submitted) -> String {
    let who = mark
        .origin
        .terminal_handle
        .as_deref()
        .or(mark.origin.pane_key.as_deref())
        .map_or_else(
            || " (no Orca terminal identity found, so it shows as \"unknown agent\")".to_owned(),
            |id| format!(" from {id}"),
        );
    match submitted {
        Submitted::App(reply) => {
            let listed = reply.get("count").and_then(Value::as_u64);
            let agent = reply
                .pointer("/agent/label")
                .and_then(Value::as_str)
                .map(|l| format!(", shown under {l}"))
                .unwrap_or_default();
            format!(
                "Marked {} ready for review{who}{agent}. Stronghold's review queue now lists {}.",
                mark.pr,
                listed.map_or_else(
                    || "it".to_owned(),
                    |n| format!("{n} PR{}", if n == 1 { "" } else { "s" })
                ),
            )
        }
        Submitted::Queued(path) => format!(
            "Marked {} ready for review{who}. Stronghold isn't running; the mark waits in {} and \
             shows when it starts.",
            mark.pr,
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn cli_arguments() {
        let (url, title, summary) = parse_cli_args(&args(&[
            "https://github.com/o/r/pull/1",
            "--title",
            "Fix it",
            "--summary=green, threads closed",
        ]))
        .unwrap();
        assert_eq!(url, "https://github.com/o/r/pull/1");
        assert_eq!(title.as_deref(), Some("Fix it"));
        assert_eq!(summary.as_deref(), Some("green, threads closed"));
        assert!(parse_cli_args(&args(&[])).is_err());
        assert!(parse_cli_args(&args(&["u", "--title"])).is_err());
        assert!(parse_cli_args(&args(&["u", "v"])).is_err());
        assert!(parse_cli_args(&args(&["u", "--bogus"])).is_err());
        assert!(run_cli(&args(&["ctl"])).is_none());
    }

    #[test]
    fn answers_name_the_pr_and_where_it_went() {
        let mut mark = ReadyMark::new(
            "https://github.com/o/r/pull/1",
            None,
            None,
            Origin {
                terminal_handle: Some("term_a".into()),
                ..Origin::default()
            },
            MarkSource::Tool,
            0,
        )
        .unwrap();
        let app = Submitted::App(json!({"count": 2, "agent": {"label": "worker 3"}}));
        assert_eq!(
            submitted_text(&mark, &app),
            "Marked o/r#1 ready for review from term_a, shown under worker 3. Stronghold's \
             review queue now lists 2 PRs."
        );
        mark.origin = Origin::default();
        let queued = Submitted::Queued("/d/pr-ready.jsonl".into());
        let text = submitted_text(&mark, &queued);
        assert!(text.contains("unknown agent"), "{text}");
        assert!(text.contains("/d/pr-ready.jsonl"), "{text}");
    }
}
