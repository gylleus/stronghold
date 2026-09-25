//! Where filled templates get their fleet data: the running app's control API (`fleet_data`),
//! else one read-only Orca poll. Also finds the coordinator's Claude scratchpad dir.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::claude::hook::{default_session_map_path, load_hook_mappings};
use crate::claude::sessions::default_projects_root;
use crate::claude::{SessionLocator, SessionMatch};
use crate::control::server::now_ms;
use crate::mcp::backend::Control;
use crate::orca::Terminal;
use crate::playbook::fill::{self, FillContext, Filled, FleetFacts};
use crate::playbook::{ATTENDANT_BRIEF, KICKOFF, Playbook, Target};

pub const FROM_APP: &str = "the running Myrstack app";
pub const FROM_POLL: &str = "one direct Orca poll as Myrstack isn't running";

/// Where Claude Code keeps session scratchpads: `<root>/claude-<uid>/<project>/<session>/scratchpad`.
pub const SCRATCH_ROOTS: [&str; 2] = ["/private/tmp", "/tmp"];

/// A template Myrstack can fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// Filled for a run id.
    AttendantBrief,
    /// Filled for a task id.
    Kickoff,
}

impl Template {
    pub fn file(self) -> &'static str {
        match self {
            Template::AttendantBrief => ATTENDANT_BRIEF,
            Template::Kickoff => KICKOFF,
        }
    }

    /// The CLI subcommand and prompt name.
    pub fn name(self) -> &'static str {
        match self {
            Template::AttendantBrief => "attendant-brief",
            Template::Kickoff => "kickoff",
        }
    }

    /// The MCP tool name.
    pub fn tool(self) -> &'static str {
        match self {
            Template::AttendantBrief => "attendant_brief",
            Template::Kickoff => "kickoff",
        }
    }

    /// The id argument's name.
    pub fn arg(self) -> &'static str {
        match self {
            Template::AttendantBrief => "run_id",
            Template::Kickoff => "task_id",
        }
    }

    pub fn parse(name: &str) -> Option<Template> {
        [Template::AttendantBrief, Template::Kickoff]
            .into_iter()
            .find(|t| t.name() == name || t.tool() == name)
    }
}

/// The fleet data from the app, else from a one-off poll; with the source's description.
pub fn fetch_facts(control: &mut dyn Control) -> Result<(FleetFacts, &'static str), String> {
    let parse = |v: Value| {
        serde_json::from_value::<FleetFacts>(v).map_err(|e| format!("unexpected fleet data: {e}"))
    };
    match control.call("fleet_data", Value::Null) {
        Ok(v) => return parse(v).map(|f| (f, FROM_APP)),
        Err(e) => log::info!("fleet data from the app: {e}; polling Orca instead"),
    }
    let fallback = control.fallback()?;
    let v = fallback
        .call("fleet_data", Value::Null)
        .map_err(|e| e.to_string())?;
    parse(v).map(|f| (f, FROM_POLL))
}

/// Fills `template` for `id` (a run id or a task id) from `playbook` and live data.
pub fn render(
    playbook: &Playbook,
    control: &mut dyn Control,
    template: Template,
    id: &str,
) -> Result<Filled, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err(format!("{} is required", template.arg()));
    }
    let served = playbook
        .get(&Target::File(template.file().to_owned()), None)
        .map_err(|e| format!("{e:#}"))?;
    let (facts, source) = fetch_facts(control)?;
    let data = facts.into_fleet();
    let context = FillContext {
        version: served.version,
        source: source.to_owned(),
        now_ms: now_ms(),
    };
    match template {
        Template::AttendantBrief => {
            let scratchpad = data
                .effective_coordinator(id)
                .and_then(|h| data.terminal(&h).and_then(coordinator_scratchpad));
            fill::attendant_brief(
                &served.body,
                &data,
                id,
                scratchpad.as_deref().and_then(Path::to_str),
                context,
            )
        }
        Template::Kickoff => fill::kickoff(&served.body, &data, id, context),
    }
}

/// The scratchpad dir of the Claude session running in `terminal`, when the session is known
/// for sure (hook mapping, or the only Claude terminal in its worktree) and the dir exists.
pub fn coordinator_scratchpad(terminal: &Terminal) -> Option<PathBuf> {
    let hooks = load_hook_mappings(&default_session_map_path());
    let locator = SessionLocator::new(default_projects_root());
    let matches = locator.map_terminals(std::slice::from_ref(terminal), Some(&hooks));
    let SessionMatch::Unique(session) = matches.get(&terminal.handle)? else {
        return None;
    };
    let roots: Vec<PathBuf> = SCRATCH_ROOTS.iter().map(PathBuf::from).collect();
    find_scratchpad(&roots, &session.session_id)
}

/// `<root>/claude-*/<any project>/<session_id>/scratchpad`, the first that is a dir.
pub fn find_scratchpad(roots: &[PathBuf], session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || session_id.contains('/') {
        return None;
    }
    let dirs = |p: &Path| -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        out.sort();
        out
    };
    roots.iter().find_map(|root| {
        dirs(root)
            .into_iter()
            .filter(|d| {
                d.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("claude-"))
            })
            .flat_map(|user| dirs(&user))
            .map(|project| project.join(session_id).join("scratchpad"))
            .find(|p| p.is_dir())
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::control::{ControlError, ErrorCode};
    use crate::mcp::CallError;
    use crate::playbook::fill::tests::{RUN, fixture_fleet};

    /// Answers `fleet_data` with `reply`; its fallback (if any) answers with the fixture.
    struct Fake {
        reply: Result<Value, CallError>,
        fallback: Option<Box<Fake>>,
        calls: usize,
    }

    impl Control for Fake {
        fn call(&mut self, method: &str, _: Value) -> Result<Value, CallError> {
            assert_eq!(method, "fleet_data");
            self.calls += 1;
            self.reply.clone()
        }

        fn fallback(&mut self) -> Result<&mut dyn Control, String> {
            match &mut self.fallback {
                Some(f) => Ok(f.as_mut()),
                None => Err("no fallback".into()),
            }
        }
    }

    fn facts_json() -> Value {
        serde_json::to_value(FleetFacts::from_fleet(&fixture_fleet())).unwrap()
    }

    fn fake(reply: Result<Value, CallError>) -> Fake {
        Fake {
            reply,
            fallback: Some(Box::new(Fake {
                reply: Ok(facts_json()),
                fallback: None,
                calls: 0,
            })),
            calls: 0,
        }
    }

    #[test]
    fn facts_come_from_the_app_when_it_answers() {
        let mut c = fake(Ok(facts_json()));
        let (facts, source) = fetch_facts(&mut c).unwrap();
        assert_eq!(source, FROM_APP);
        assert_eq!(facts.runs.len(), 2);
        assert_eq!(c.fallback.unwrap().calls, 0);
    }

    #[test]
    fn facts_fall_back_to_a_poll_when_the_app_is_down_old_or_not_ready() {
        for reply in [
            Err(CallError::NotRunning),
            Err(CallError::Control(ControlError::new(
                ErrorCode::UnknownMethod,
                "unknown method",
            ))),
            Err(CallError::Control(ControlError::new(
                ErrorCode::Unavailable,
                "not polled yet",
            ))),
        ] {
            let mut c = fake(reply);
            let (facts, source) = fetch_facts(&mut c).unwrap();
            assert_eq!(source, FROM_POLL);
            assert_eq!(facts.runs[0].id, RUN);
        }
        let mut broken = fake(Ok(json!({"runs": "nope"})));
        assert!(fetch_facts(&mut broken).unwrap_err().contains("unexpected"));
        let mut no_fallback = Fake {
            reply: Err(CallError::NotRunning),
            fallback: None,
            calls: 0,
        };
        assert_eq!(fetch_facts(&mut no_fallback).unwrap_err(), "no fallback");
    }

    #[test]
    fn render_fills_the_templates_from_the_playbook_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(ATTENDANT_BRIEF),
            "# Brief <campaign>\nCOORD `<coordinator terminal handle>` <mystery>\n",
        )
        .unwrap();
        let pb = Playbook::at(dir.path());
        let f = render(
            &pb,
            &mut fake(Ok(facts_json())),
            Template::AttendantBrief,
            RUN,
        )
        .unwrap();
        assert!(
            f.result
                .text
                .contains("COORD `term_coord` <mystery: FILL IN>")
        );
        assert_eq!(f.context.source, FROM_APP);
        assert_eq!(f.context.version.commit, None);

        let e = render(&pb, &mut fake(Ok(facts_json())), Template::Kickoff, "t_a").unwrap_err();
        assert!(e.contains("KICKOFF-TEMPLATE.md"), "{e}");
        let e = render(
            &pb,
            &mut fake(Ok(facts_json())),
            Template::AttendantBrief,
            " ",
        )
        .unwrap_err();
        assert_eq!(e, "run_id is required");
    }

    #[test]
    fn templates_parse_from_cli_prompt_and_tool_names() {
        assert_eq!(
            Template::parse("attendant-brief"),
            Some(Template::AttendantBrief)
        );
        assert_eq!(
            Template::parse("attendant_brief"),
            Some(Template::AttendantBrief)
        );
        assert_eq!(Template::parse("kickoff"), Some(Template::Kickoff));
        assert_eq!(Template::parse("coordinator"), None);
    }

    #[test]
    fn scratchpads_are_found_under_any_claude_user_and_project() {
        let root = tempfile::tempdir().unwrap();
        let pad = root
            .path()
            .join("claude-501/-Users-me-repo/sess-1/scratchpad");
        std::fs::create_dir_all(&pad).unwrap();
        std::fs::create_dir_all(root.path().join("other/-x/sess-2/scratchpad")).unwrap();
        let roots = [root.path().join("missing"), root.path().to_path_buf()];
        assert_eq!(find_scratchpad(&roots, "sess-1"), Some(pad));
        assert_eq!(
            find_scratchpad(&roots, "sess-2"),
            None,
            "only claude-* dirs"
        );
        assert_eq!(find_scratchpad(&roots, "nope"), None);
        assert_eq!(find_scratchpad(&roots, ""), None);
        assert_eq!(find_scratchpad(&roots, "../sess-1"), None);
    }
}
