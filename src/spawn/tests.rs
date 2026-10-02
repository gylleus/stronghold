use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Value, json};

use super::exec::{
    Job, Progress, SpawnBackend, SpawnEvent, VERIFY_DEADLINE, Verifier, VerifyStep, run_job,
    run_plan,
};
use super::pending::{PENDING_SELECT_MS, PendingSelect, node_for_handle, take_ready};
use super::resolve::LiveAgent;
use super::*;
use crate::actions::Key;
use crate::fleet::FleetData;
use crate::model::{FleetSnapshot, NodeId};
use crate::orca::client::parse_envelope;
use crate::orca::spawn::{
    WorkerPlacement, parse_repo_list, parse_task_create, parse_terminal_create,
    parse_terminal_wait, parse_worker_start, parse_worktree_create, parse_worktree_list,
};
use crate::orca::{OrcaError, Run, TaskStatus, Terminal, TerminalInput};

// ---------------------------------------------------------------------------
// Fixtures

macro_rules! orca_fixture {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/orca/",
            $name
        ))
        .as_slice()
    };
}

macro_rules! screen {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/screens/",
            $name
        ))
        .lines()
        .map(str::to_owned)
        .collect::<Vec<String>>()
    };
}

const WT_ID: &str = "f2c8fcb1-8fa7-4763-8986-a597cba57cdd::/Users/gylleus/orca/workspaces/commerce-platform/hack-77-admin-speed";
const WT_PATH: &str = "/Users/gylleus/orca/workspaces/commerce-platform/hack-77-admin-speed";
const REPO_ID: &str = "f2c8fcb1-8fa7-4763-8986-a597cba57cdd";
const PROMPT: &str = "Fix the flaky checkout test in packages/platform: it times out on CI about \
                      one run in five. Find the cause, fix it, and add a regression test.";

fn result(bytes: &[u8]) -> Value {
    parse_envelope(bytes).expect("ok envelope")
}

fn catalog() -> Catalog {
    Catalog {
        repos: parse_repo_list(orca_fixture!("repo_list.json"))
            .unwrap()
            .repos,
        worktrees: parse_worktree_list(orca_fixture!("worktree_list.json"))
            .unwrap()
            .worktrees,
    }
}

fn runs() -> Vec<Run> {
    crate::orca::commands::parse_run_list(orca_fixture!("run_list.json")).unwrap()
}

fn terminals() -> Vec<Terminal> {
    crate::orca::commands::parse_terminal_list(orca_fixture!("terminal_list.json"))
        .unwrap()
        .terminals
}

fn request(target: SpawnTarget) -> SpawnRequest {
    SpawnRequest {
        target,
        agent: Agent::Claude,
        model: None,
        prompt: PROMPT.into(),
        label: None,
    }
}

fn existing() -> SpawnTarget {
    SpawnTarget::Worktree {
        worktree_id: WT_ID.into(),
        path: WT_PATH.into(),
    }
}

fn new_worktree() -> SpawnTarget {
    SpawnTarget::NewWorktree {
        repo_id: REPO_ID.into(),
        name: "spawn-test".into(),
        base_branch: None,
    }
}

fn worker(placement: WorkerPlacement) -> SpawnTarget {
    SpawnTarget::Worker {
        run_id: "run_7c25cdcf46e0".into(),
        coordinator: "term_df9f48c9-c8e8-4ceb-afca-932028245fb1".into(),
        placement,
        title: None,
        path: None,
    }
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn ctx_with(handle: &str) -> SpawnContext {
    SpawnContext {
        handle: Some(handle.into()),
        ..SpawnContext::default()
    }
}

// ---------------------------------------------------------------------------
// Plans

#[test]
fn existing_worktree_plan_launches_claude_with_the_prompt_as_argv() {
    let plan = build_plan(&request(existing())).unwrap();
    assert_eq!(plan.steps.len(), 3);
    let command = format!("claude '{PROMPT}'");
    assert_eq!(
        plan.steps[0].orca_args(&plan.initial).unwrap().unwrap(),
        args(&[
            "terminal",
            "create",
            &format!("--worktree=id:{WT_ID}"),
            &format!("--command={command}"),
        ])
    );
    assert_eq!(
        plan.steps[1]
            .orca_args(&ctx_with("term_x"))
            .unwrap()
            .unwrap(),
        args(&[
            "terminal",
            "wait",
            "--terminal=term_x",
            "--for=tui-idle",
            "--timeout-ms=8000",
        ])
    );
    assert_eq!(
        plan.steps[2],
        Step::Verify {
            expect: Expect::Prompt(PROMPT.into()),
            retype: true,
            allow_bypass: true
        }
    );
    assert_eq!(plan.initial.worktree_path.as_deref(), Some(WT_PATH));
    // The wait needs the handle the terminal step produces.
    assert!(plan.steps[1].orca_args(&plan.initial).unwrap().is_err());
    assert!(plan.display_lines()[1].contains(r#"--terminal="<agent terminal>""#));
}

#[test]
fn codex_with_a_model_gets_model_flag_before_the_prompt() {
    let mut req = request(existing());
    req.agent = Agent::Codex;
    req.model = Some(" gpt-6-astra ".into());
    req.prompt = "don't stop".into();
    let plan = build_plan(&req).unwrap();
    assert_eq!(plan.request.model.as_deref(), Some("gpt-6-astra"));
    let Step::CreateTerminal { command, .. } = &plan.steps[0] else {
        panic!("{:?}", plan.steps[0]);
    };
    assert_eq!(
        command,
        r#"codex --dangerously-bypass-approvals-and-sandbox --model gpt-6-astra 'don'"'"'t stop'"#
    );
}

#[test]
fn codex_in_a_new_worktree_launches_its_own_never_ask_terminal() {
    let mut req = request(new_worktree());
    req.agent = Agent::Codex;
    let plan = build_plan(&req).unwrap();
    let create = plan.steps[0].orca_args(&plan.initial).unwrap().unwrap();
    assert!(
        !create.iter().any(|a| a.starts_with("--agent")),
        "{create:?}"
    );
    let Step::CreateTerminal { command, .. } = &plan.steps[2] else {
        panic!("{:?}", plan.steps[2]);
    };
    assert_eq!(
        command,
        &format!("codex --dangerously-bypass-approvals-and-sandbox '{PROMPT}'")
    );
}

#[test]
fn new_worktree_without_model_lets_orca_launch_the_agent() {
    let plan = build_plan(&request(new_worktree())).unwrap();
    assert_eq!(
        plan.steps[0].orca_args(&plan.initial).unwrap().unwrap(),
        args(&[
            "worktree",
            "create",
            &format!("--repo=id:{REPO_ID}"),
            "--name=spawn-test",
            "--no-parent",
            "--agent=claude",
            &format!("--prompt={PROMPT}"),
        ])
    );
    assert_eq!(plan.steps[1], Step::DirenvAllow);
    assert!(matches!(plan.steps[2], Step::WaitIdle { .. }));
    assert!(matches!(plan.steps[3], Step::Verify { retype: true, .. }));
    assert_eq!(plan.steps[4], Step::CloseStartupShell);
    assert_eq!(plan.steps.len(), 5);
}

#[test]
fn new_worktree_with_model_launches_its_own_terminal() {
    let mut req = request(SpawnTarget::NewWorktree {
        repo_id: REPO_ID.into(),
        name: " fix checkout ".into(),
        base_branch: Some("origin/release".into()),
    });
    req.model = Some("opus[1m]".into());
    let plan = build_plan(&req).unwrap();
    assert_eq!(
        plan.steps[0].orca_args(&plan.initial).unwrap().unwrap(),
        args(&[
            "worktree",
            "create",
            &format!("--repo=id:{REPO_ID}"),
            "--name=fix-checkout",
            "--no-parent",
            "--base-branch=origin/release",
        ])
    );
    assert_eq!(plan.steps[1], Step::DirenvAllow);
    let ctx = SpawnContext {
        worktree_id: Some("r::/wt".into()),
        ..SpawnContext::default()
    };
    assert_eq!(
        plan.steps[2].orca_args(&ctx).unwrap().unwrap(),
        args(&[
            "terminal",
            "create",
            "--worktree=id:r::/wt",
            &format!("--command=claude --model 'opus[1m]' '{PROMPT}'"),
        ])
    );
    assert_eq!(
        plan.steps[2].orca_args(&SpawnContext::default()).unwrap(),
        Err("no worktree from an earlier step".into())
    );
    assert!(plan.display_lines()[2].contains("<new worktree>"));
}

#[test]
fn worker_plan_creates_the_task_as_the_coordinator_then_starts_it() {
    let plan = build_plan(&request(worker(WorkerPlacement::NewWorktree {
        repo_id: REPO_ID.into(),
        name: "flaky-checkout".into(),
    })))
    .unwrap();
    assert_eq!(plan.steps.len(), 5);
    assert_eq!(plan.steps[4], Step::CloseStartupShell);
    assert_eq!(
        plan.steps[0].orca_args(&plan.initial).unwrap().unwrap(),
        args(&[
            "orchestration",
            "task-create",
            "--run=run_7c25cdcf46e0",
            "--from=term_df9f48c9-c8e8-4ceb-afca-932028245fb1",
            &format!("--spec={PROMPT}"),
            "--task-title=Fix the flaky checkout test in packages/platform: it times…",
        ])
    );
    let ctx = SpawnContext {
        task_id: Some("task_1".into()),
        ..SpawnContext::default()
    };
    assert_eq!(
        plan.steps[1].orca_args(&ctx).unwrap().unwrap(),
        args(&[
            "orchestration",
            "worker-start",
            "--task=task_1",
            "--run=run_7c25cdcf46e0",
            "--from=term_df9f48c9-c8e8-4ceb-afca-932028245fb1",
            "--worktree=new-top-level",
            "--name=flaky-checkout",
            &format!("--repo=id:{REPO_ID}"),
            "--agent=claude",
            "--timeout-ms=90000",
        ])
    );
    assert_eq!(plan.steps[2], Step::DirenvAllow);
    assert_eq!(
        plan.steps[3],
        Step::Verify {
            expect: Expect::AnyInput,
            retype: false,
            allow_bypass: true
        }
    );
}

/// A label names the worktree right after it exists, or becomes a worker's task display name.
#[test]
fn a_label_names_the_agent() {
    let named = |target| SpawnRequest {
        label: Some("  Checkout retries ".into()),
        ..request(target)
    };
    let name_args = |worktree: &str| {
        args(&[
            "worktree",
            "set",
            &format!("--worktree=id:{worktree}"),
            "--display-name=Checkout retries",
        ])
    };

    let plan = build_plan(&named(existing())).unwrap();
    assert_eq!(plan.request.label.as_deref(), Some("Checkout retries"));
    assert_eq!(
        plan.steps[0].orca_args(&plan.initial).unwrap().unwrap(),
        name_args(WT_ID)
    );

    let plan = build_plan(&named(new_worktree())).unwrap();
    assert!(matches!(plan.steps[0], Step::CreateWorktree(_)));
    let ctx = SpawnContext {
        worktree_id: Some("new-wt".into()),
        ..SpawnContext::default()
    };
    assert_eq!(
        plan.steps[1].orca_args(&ctx).unwrap().unwrap(),
        name_args("new-wt")
    );

    let plan = build_plan(&named(worker(WorkerPlacement::Worktree {
        worktree_id: WT_ID.into(),
    })))
    .unwrap();
    assert!(
        !plan
            .steps
            .iter()
            .any(|s| matches!(s, Step::NameWorktree { .. })),
        "a worker's worktree may be shared; its task carries the name"
    );
    let task = plan.steps[0].orca_args(&plan.initial).unwrap().unwrap();
    assert_eq!(task.last().unwrap(), "--display-name=Checkout retries");

    let plan = build_plan(&request(existing())).unwrap();
    assert!(
        !plan
            .steps
            .iter()
            .any(|s| matches!(s, Step::NameWorktree { .. }))
    );
}

#[test]
fn worker_in_an_existing_worktree_or_terminal() {
    let mut req = request(worker(WorkerPlacement::Worktree {
        worktree_id: WT_ID.into(),
    }));
    req.agent = Agent::Codex;
    req.model = Some("gpt-6-astra".into());
    let plan = build_plan(&req).unwrap();
    let ctx = SpawnContext {
        task_id: Some("t".into()),
        ..SpawnContext::default()
    };
    let start = plan.steps[1].orca_args(&ctx).unwrap().unwrap();
    assert_eq!(
        start[5..],
        args(&[
            &format!("--worktree=id:{WT_ID}"),
            "--agent=codex",
            "--model=gpt-6-astra",
            "--timeout-ms=90000",
        ])[..]
    );
    assert_eq!(
        plan.steps.len(),
        3,
        "no direnv step for an existing worktree"
    );

    let plan = build_plan(&request(worker(WorkerPlacement::Terminal {
        worktree_id: WT_ID.into(),
        handle: "term_w".into(),
    })))
    .unwrap();
    let start = plan.steps[1].orca_args(&ctx).unwrap().unwrap();
    assert_eq!(
        start[5..],
        args(&[
            &format!("--worktree=id:{WT_ID}"),
            "--terminal=term_w",
            "--timeout-ms=90000",
        ])[..]
    );
    assert_eq!(plan.initial.handle.as_deref(), Some("term_w"));
}

#[test]
fn invalid_requests_say_what_is_wrong() {
    let cases: Vec<(SpawnRequest, &str)> = vec![
        (
            request(SpawnTarget::Worktree {
                worktree_id: String::new(),
                path: String::new(),
            }),
            "pick a worktree",
        ),
        (
            request(SpawnTarget::NewWorktree {
                repo_id: REPO_ID.into(),
                name: "  ".into(),
                base_branch: None,
            }),
            "give the new worktree a name",
        ),
        (
            request(SpawnTarget::NewWorktree {
                repo_id: REPO_ID.into(),
                name: "a;rm".into(),
                base_branch: None,
            }),
            "may only use",
        ),
        (
            SpawnRequest {
                model: Some("opus; rm -rf".into()),
                ..request(existing())
            },
            "characters a model id can't have",
        ),
        (
            SpawnRequest {
                prompt: " ".into(),
                ..request(worker(WorkerPlacement::Worktree {
                    worktree_id: WT_ID.into(),
                }))
            },
            "task spec",
        ),
        (
            SpawnRequest {
                model: Some("opus".into()),
                ..request(worker(WorkerPlacement::Terminal {
                    worktree_id: WT_ID.into(),
                    handle: "term_w".into(),
                }))
            },
            "existing terminal",
        ),
        (
            request(SpawnTarget::Worker {
                run_id: "run_1".into(),
                coordinator: String::new(),
                placement: WorkerPlacement::Worktree {
                    worktree_id: WT_ID.into(),
                },
                title: None,
                path: None,
            }),
            "no coordinator",
        ),
    ];
    for (req, want) in cases {
        let err = build_plan(&req).unwrap_err().0;
        assert!(err.contains(want), "{want:?} not in {err:?}");
    }
}

#[test]
fn empty_prompt_launches_bare_agent_and_expects_only_readiness() {
    let plan = build_plan(&SpawnRequest {
        prompt: "  ".into(),
        ..request(existing())
    })
    .unwrap();
    let Step::CreateTerminal { command, .. } = &plan.steps[0] else {
        panic!()
    };
    assert_eq!(command, "claude");
    assert_eq!(
        plan.steps[2],
        Step::Verify {
            expect: Expect::Ready,
            retype: true,
            allow_bypass: true
        }
    );
    let plan = build_plan(&SpawnRequest {
        prompt: String::new(),
        ..request(new_worktree())
    })
    .unwrap();
    let args = plan.steps[0].orca_args(&plan.initial).unwrap().unwrap();
    assert!(args.contains(&"--agent=claude".to_string()));
    assert!(!args.iter().any(|a| a.starts_with("--prompt")));
}

#[test]
fn shell_quoting_matches_orca() {
    assert_eq!(shell_quote("opus"), "opus");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("it's"), r#"'it'"'"'s'"#);
    assert_eq!(shell_quote(r"a\b"), r#"'a'"\\"'b'"#);
    assert_eq!(shell_quote("$HOME `x`"), "'$HOME `x`'");
    assert_eq!(shell_quote("two\nlines"), "'two\nlines'");
    assert_eq!(
        launch_command(Agent::Claude, None, "--help me"),
        "claude -- '--help me'"
    );
    assert_eq!(
        launch_command(Agent::Claude, Some("sonnet"), "go"),
        "claude --model sonnet go"
    );
    assert_eq!(
        launch_command(Agent::Codex, None, ""),
        "codex --dangerously-bypass-approvals-and-sandbox"
    );
}

#[test]
fn names_titles_and_models() {
    assert_eq!(
        validate_name("fix/checkout flake").unwrap(),
        "fix/checkout-flake"
    );
    for bad in ["-x", ".x", "x/", "a..b", "a$b", "x."] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
    assert!(valid_model("claude-opus-4-8"));
    assert!(valid_model("sonnet[1m]"));
    assert!(!valid_model("a b"));
    assert_eq!(title_from("\n  Short one\nmore"), "Short one");
    assert_eq!(title_from(""), "Task from Myrstack");
    assert_eq!(title_from(&"x".repeat(100)).chars().count(), 60);
}

// ---------------------------------------------------------------------------
// Orca result parsing

#[test]
fn repo_and_worktree_lists_parse() {
    let c = catalog();
    assert_eq!(c.repos.len(), 3);
    let cp = c.repos.iter().find(|r| r.id == REPO_ID).unwrap();
    assert_eq!(cp.name(), "commerce-platform");
    assert_eq!(cp.worktree_base_ref.as_deref(), Some("origin/main"));
    assert_eq!(cp.selector(), format!("id:{REPO_ID}"));
    let wt = c.worktree(WT_ID).unwrap();
    assert_eq!(wt.path, WT_PATH);
    assert_eq!(wt.repo(), Some(REPO_ID));
    assert_eq!(wt.name(), "hack-77-admin-speed");
    assert!(!wt.is_main_worktree);
}

#[test]
fn worktree_create_results_parse_with_and_without_agent_handle() {
    let created = parse_worktree_create(orca_fixture!("worktree_create_agent.json")).unwrap();
    assert_eq!(
        created.agent_handle(),
        Some("term_7d1e4c2a-9b3f-4e8d-a6c5-0f1e2d3c4b5a")
    );
    assert_eq!(created.worktree.name(), "spawn-test");
    let legacy = parse_worktree_create(orca_fixture!("worktree_create_legacy.json")).unwrap();
    assert_eq!(
        legacy.agent_handle(),
        Some("term_1b2c3d4e-5f60-4718-8a9b-0c1d2e3f4a5b")
    );
    assert!(legacy.warning.unwrap().contains("Setup hook"));
}

#[test]
fn terminal_task_and_worker_results_parse() {
    let t = parse_terminal_create(orca_fixture!("terminal_create.json")).unwrap();
    assert_eq!(t.handle, "term_9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d");
    assert_eq!(t.surface.as_deref(), Some("background"));
    let w = parse_terminal_wait(orca_fixture!("terminal_wait_idle.json")).unwrap();
    assert!(w.satisfied);
    let w = parse_terminal_wait(orca_fixture!("terminal_wait_blocked.json")).unwrap();
    assert!(!w.satisfied);
    assert_eq!(w.blocked_reason.as_deref(), Some("codex-trust-workspace"));
    let task = parse_task_create(orca_fixture!("task_create.json")).unwrap();
    assert_eq!(task.id, "task_4e5f6a7b8c9d");
    assert_eq!(task.status, TaskStatus::Ready);

    let ready = parse_worker_start(orca_fixture!("worker_start_ready.json")).unwrap();
    assert!(ready.is_ready());
    assert_eq!(
        ready.agent_handle(),
        Some("term_5e6f7a8b-9c0d-4e1f-8a2b-3c4d5e6f7a8b")
    );
    assert!(ready.worktree_id().unwrap().ends_with("/flaky-checkout"));
    let failed = parse_worker_start(orca_fixture!("worker_start_failed.json")).unwrap();
    assert!(!failed.is_ready());
    let text = failed.failure();
    assert!(text.contains("failed at agent_readiness"), "{text}");
    assert!(text.contains("Agent did not become ready"), "{text}");
    assert!(text.contains("1 leftover resource"), "{text}");
    let fenced = parse_worker_start(orca_fixture!("error_consumer_fenced.json")).unwrap_err();
    assert_eq!(fenced.code(), Some("consumer_fenced"));
}

// ---------------------------------------------------------------------------
// Screen verification

fn verdict(lines: &[String], agent: Agent) -> ScreenVerdict {
    classify_screen(lines, agent, &Expect::Prompt(PROMPT.into()))
}

#[test]
fn classifies_claude_launch_screens() {
    use ScreenVerdict::*;
    let claude = Agent::Claude;
    assert_eq!(
        verdict(&screen!("spawn_claude_parked_synthetic.txt"), claude),
        Parked
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_started_synthetic.txt"), claude),
        Started
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_bypass_modal_synthetic.txt"), claude),
        BypassModal
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_trust_synthetic.txt"), claude),
        TrustPrompt
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_booting_synthetic.txt"), claude),
        Booting
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_idle_empty_synthetic.txt"), claude),
        IdleEmpty
    );
    assert_eq!(
        verdict(&screen!("spawn_claude_pasted_synthetic.txt"), claude),
        Parked
    );
    // A fresh session's own ⏺ notices are not the prompt starting.
    assert_eq!(
        verdict(&screen!("claude_idle_fresh_session.txt"), claude),
        IdleEmpty
    );
    assert_eq!(verdict(&[], claude), Booting);
}

#[test]
fn classifies_codex_launch_screens() {
    let codex = Agent::Codex;
    assert_eq!(
        verdict(&screen!("spawn_codex_parked_synthetic.txt"), codex),
        ScreenVerdict::Parked
    );
    assert_eq!(
        verdict(&screen!("spawn_codex_started_synthetic.txt"), codex),
        ScreenVerdict::Started
    );
    // The placeholder is not a parked prompt.
    assert_eq!(
        classify_screen(
            &screen!("codex_idle.txt"),
            codex,
            &Expect::Prompt(PROMPT.into())
        ),
        ScreenVerdict::IdleEmpty
    );
}

#[test]
fn flags_a_codex_session_that_asks_for_approval() {
    assert!(codex_approvals_on(&screen!("spawn_codex_approvals_on.txt")));
    assert!(!codex_approvals_on(&screen!("spawn_codex_yolo.txt")));
    assert!(!codex_approvals_on(&screen!(
        "spawn_codex_started_synthetic.txt"
    )));
    // No header on screen: nothing to tell.
    assert!(!codex_approvals_on(&screen!("codex_working_synthetic.txt")));

    let expect = Expect::Prompt(PROMPT.into());
    let mut v = Verifier::new(Agent::Codex, &expect, true);
    assert_eq!(
        v.next(&screen!("spawn_codex_approvals_on.txt")),
        VerifyStep::Done
    );
    assert!(v.approvals_on);
    let mut v = Verifier::new(Agent::Codex, &expect, true);
    assert_eq!(v.next(&screen!("spawn_codex_yolo.txt")), VerifyStep::Done);
    assert!(!v.approvals_on && v.notes.is_empty());
}

#[test]
fn codex_launch_with_approvals_on_ends_verify_with_a_warning() {
    let mut req = request(existing());
    req.agent = Agent::Codex;
    let plan = build_plan(&req).unwrap();
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        )
        .screens(vec![screen!("spawn_codex_approvals_on.txt")]);
    let (result, progress) = run(&plan, &mut fake);
    let outcome = result.unwrap();
    assert_eq!(
        outcome.notes,
        vec![format!("prompt taken; {}", exec::CODEX_APPROVALS_ON)]
    );
    let verify_end = progress
        .iter()
        .map(|Progress::Step { status, .. }| status)
        .rfind(|s| s.is_finished())
        .unwrap();
    assert!(
        matches!(verify_end, StepStatus::Warned(n) if n.contains("approvals on")),
        "{verify_end:?}"
    );
}

#[test]
fn worker_and_bare_launches_use_looser_expectations() {
    let claude = Agent::Claude;
    // A worker's preamble is text Myrstack didn't write: typed but unsent → parked.
    let parked = screen!("spawn_claude_parked_synthetic.txt");
    assert_eq!(
        classify_screen(&parked, claude, &Expect::AnyInput),
        ScreenVerdict::Parked
    );
    assert_eq!(
        classify_screen(
            &screen!("claude_working_synthetic.txt"),
            claude,
            &Expect::AnyInput
        ),
        ScreenVerdict::Started
    );
    let idle = screen!("spawn_claude_idle_empty_synthetic.txt");
    assert_eq!(
        classify_screen(&idle, claude, &Expect::Ready),
        ScreenVerdict::Started
    );
    assert_eq!(
        classify_screen(&idle, claude, &Expect::AnyInput),
        ScreenVerdict::IdleEmpty
    );
}

#[test]
fn verifier_answers_modals_submits_parked_prompts_and_retypes_once() {
    let mut v = Verifier::new(Agent::Claude, &Expect::Prompt(PROMPT.into()), true);
    let bypass = screen!("spawn_claude_bypass_modal_synthetic.txt");
    assert_eq!(
        v.next(&bypass),
        VerifyStep::Send(
            TerminalInput::key(Key::Digit(2)),
            "accept Bypass Permissions"
        )
    );
    assert_eq!(
        v.next(&screen!("spawn_claude_trust_synthetic.txt")),
        VerifyStep::Send(TerminalInput::key(Key::Digit(1)), "trust the folder")
    );
    let parked = screen!("spawn_claude_parked_synthetic.txt");
    assert_eq!(
        v.next(&parked),
        VerifyStep::Send(TerminalInput::enter_key(), "submit the parked prompt")
    );
    let idle = screen!("spawn_claude_idle_empty_synthetic.txt");
    assert_eq!(
        v.next(&idle),
        VerifyStep::Wait,
        "one idle read is not enough"
    );
    assert_eq!(
        v.next(&idle),
        VerifyStep::Send(TerminalInput::line(PROMPT), "retype the prompt")
    );
    assert_eq!(v.next(&idle), VerifyStep::Wait, "retypes only once");
    assert_eq!(
        v.next(&screen!("spawn_claude_started_synthetic.txt")),
        VerifyStep::Done
    );
    // The bypass answer is capped.
    let mut v = Verifier::new(Agent::Claude, &Expect::Prompt(PROMPT.into()), false);
    assert!(matches!(v.next(&bypass), VerifyStep::Send(..)));
    assert!(matches!(v.next(&bypass), VerifyStep::Send(..)));
    assert_eq!(v.next(&bypass), VerifyStep::Wait);
    // Without retype, a lost prompt only waits.
    assert_eq!(v.next(&idle), VerifyStep::Wait);
    assert_eq!(v.next(&idle), VerifyStep::Wait);
}

// ---------------------------------------------------------------------------
// Executing plans against a fake backend

#[derive(Default)]
struct Fake {
    replies: VecDeque<(&'static str, Result<Value, OrcaError>)>,
    calls: Vec<Vec<String>>,
    /// Screens returned in order; the last one repeats.
    screens: VecDeque<Result<Vec<String>, OrcaError>>,
    last_screen: Vec<String>,
    sent: Vec<(String, TerminalInput)>,
    direnv: Option<Result<Option<String>, String>>,
    direnv_calls: Vec<String>,
    catalog: Catalog,
    branches: Vec<String>,
    clock: Duration,
}

impl Fake {
    fn reply(mut self, subcommand: &'static str, reply: Result<Value, OrcaError>) -> Self {
        self.replies.push_back((subcommand, reply));
        self
    }

    fn screens(mut self, screens: Vec<Vec<String>>) -> Self {
        self.screens.extend(screens.into_iter().map(Ok));
        self
    }
}

impl SpawnBackend for Fake {
    fn orca(&mut self, args: &[String], _timeout: Duration) -> Result<Value, OrcaError> {
        self.calls.push(args.to_vec());
        let (expected, reply) = self
            .replies
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected orca {args:?}"));
        assert_eq!(args[..2].join(" "), expected, "{args:?}");
        reply
    }

    fn read_screen(&mut self, _handle: &str) -> Result<Vec<String>, OrcaError> {
        match self.screens.pop_front() {
            Some(Ok(lines)) => {
                self.last_screen = lines.clone();
                Ok(lines)
            }
            Some(Err(e)) => Err(e),
            None => Ok(self.last_screen.clone()),
        }
    }

    fn send(&mut self, handle: &str, input: &TerminalInput) -> Result<(), String> {
        self.sent.push((handle.to_string(), input.clone()));
        Ok(())
    }

    fn direnv_allow(&mut self, path: &str) -> Result<Option<String>, String> {
        self.direnv_calls.push(path.to_string());
        self.direnv.clone().unwrap_or(Ok(None))
    }

    fn catalog(&mut self) -> Result<Catalog, String> {
        Ok(self.catalog.clone())
    }

    fn branches(&mut self, _repo_path: &str) -> Result<Vec<String>, String> {
        Ok(self.branches.clone())
    }

    fn sleep(&mut self, duration: Duration) {
        self.clock += duration;
    }

    fn elapsed(&self) -> Duration {
        self.clock
    }
}

fn run(
    plan: &SpawnPlan,
    fake: &mut Fake,
) -> (Result<SpawnOutcome, exec::SpawnFailure>, Vec<Progress>) {
    let mut progress = Vec::new();
    let result = run_plan(plan, fake, &mut |p| progress.push(p));
    (result, progress)
}

fn api_error(bytes: &[u8]) -> OrcaError {
    parse_envelope(bytes).unwrap_err()
}

#[test]
fn existing_worktree_run_accepts_bypass_and_submits_the_parked_prompt() {
    let plan = build_plan(&request(existing())).unwrap();
    let timeout = OrcaError::Api(crate::orca::ApiError {
        code: "timeout".into(),
        message: "timeout".into(),
        data: Value::Null,
    });
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply("terminal wait", Err(timeout))
        .screens(vec![
            screen!("spawn_claude_booting_synthetic.txt"),
            screen!("spawn_claude_bypass_modal_synthetic.txt"),
            screen!("spawn_claude_parked_synthetic.txt"),
            screen!("spawn_claude_started_synthetic.txt"),
        ]);
    let (result, progress) = run(&plan, &mut fake);
    let outcome = result.unwrap();
    let handle = "term_9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
    assert_eq!(outcome.context.handle.as_deref(), Some(handle));
    assert!(outcome.verified);
    assert_eq!(
        outcome.notes,
        vec![
            "accepted the Bypass Permissions warning (2)",
            "submitted the parked prompt (Enter)"
        ]
    );
    assert_eq!(
        fake.sent,
        vec![
            (handle.to_string(), TerminalInput::key(Key::Digit(2))),
            (handle.to_string(), TerminalInput::enter_key()),
        ]
    );
    assert_eq!(fake.calls[1][2], format!("--terminal={handle}"));
    assert_eq!(outcome.node_id(), Some(NodeId::terminal(handle)));
    // Each step reports Running, then its end.
    let ends: Vec<&StepStatus> = progress
        .iter()
        .map(|Progress::Step { status, .. }| status)
        .filter(|s| s.is_finished())
        .collect();
    assert_eq!(ends.len(), 3);
    assert!(matches!(ends[1], StepStatus::Done(Some(n)) if n.starts_with("not idle yet")));
    assert!(matches!(ends[2], StepStatus::Done(Some(n)) if n.contains("prompt taken after")));
}

#[test]
fn new_worktree_run_records_the_worktree_and_allows_direnv() {
    let plan = build_plan(&request(new_worktree())).unwrap();
    let mut fake = Fake {
        direnv: Some(Ok(Some("direnv allow …".into()))),
        ..Fake::default()
    }
    .reply(
        "worktree create",
        Ok(result(orca_fixture!("worktree_create_agent.json"))),
    )
    .reply(
        "terminal wait",
        Ok(result(orca_fixture!("terminal_wait_idle.json"))),
    )
    .reply("terminal list", Ok(json!({ "terminals": [] })))
    .screens(vec![screen!("spawn_claude_started_synthetic.txt")]);
    let (result, _) = run(&plan, &mut fake);
    let outcome = result.unwrap();
    let ctx = &outcome.context;
    assert_eq!(
        ctx.worktree_path.as_deref(),
        Some("/Users/gylleus/orca/workspaces/commerce-platform/spawn-test")
    );
    assert_eq!(
        ctx.handle.as_deref(),
        Some("term_7d1e4c2a-9b3f-4e8d-a6c5-0f1e2d3c4b5a")
    );
    assert_eq!(
        fake.direnv_calls,
        vec!["/Users/gylleus/orca/workspaces/commerce-platform/spawn-test"]
    );
    assert!(outcome.verified && fake.sent.is_empty());
}

#[test]
fn worker_run_creates_task_starts_worker_and_checks_the_preamble() {
    let plan = build_plan(&request(worker(WorkerPlacement::NewWorktree {
        repo_id: REPO_ID.into(),
        name: "flaky-checkout".into(),
    })))
    .unwrap();
    let mut fake = Fake::default()
        .reply(
            "orchestration task-create",
            Ok(result(orca_fixture!("task_create.json"))),
        )
        .reply(
            "orchestration worker-start",
            Ok(result(orca_fixture!("worker_start_ready.json"))),
        )
        .reply("terminal list", Ok(json!({ "terminals": [] })))
        .screens(vec![screen!("claude_working_synthetic.txt")]);
    let (result, _) = run(&plan, &mut fake);
    let outcome = result.unwrap();
    assert_eq!(fake.calls[1][2], "--task=task_4e5f6a7b8c9d");
    let ctx = &outcome.context;
    assert_eq!(ctx.task_id.as_deref(), Some("task_4e5f6a7b8c9d"));
    assert_eq!(ctx.dispatch_id.as_deref(), Some("dispatch_0a1b2c3d4e5f"));
    assert_eq!(
        ctx.worktree_path.as_deref(),
        Some("/Users/gylleus/orca/workspaces/commerce-platform/flaky-checkout")
    );
    assert_eq!(
        outcome.node_id(),
        Some(NodeId::dispatch(
            "run_7c25cdcf46e0",
            "dispatch_0a1b2c3d4e5f"
        ))
    );
    assert!(outcome.verified);
    assert_eq!(fake.direnv_calls.len(), 1);
}

#[test]
fn failed_worker_start_stops_the_plan_and_keeps_what_was_created() {
    let plan = build_plan(&request(worker(WorkerPlacement::Worktree {
        worktree_id: WT_ID.into(),
    })))
    .unwrap();
    let mut fake = Fake::default()
        .reply(
            "orchestration task-create",
            Ok(result(orca_fixture!("task_create.json"))),
        )
        .reply(
            "orchestration worker-start",
            Ok(result(orca_fixture!("worker_start_failed.json"))),
        );
    let (result, progress) = run(&plan, &mut fake);
    let failure = result.unwrap_err();
    assert_eq!(failure.step, Some(1));
    assert!(
        failure
            .message
            .starts_with("Start worker: worker failed at agent_readiness")
    );
    assert_eq!(
        failure.context.task_id.as_deref(),
        Some("task_4e5f6a7b8c9d")
    );
    let text = failure.to_string();
    assert!(text.contains("already created"), "{text}");
    assert!(matches!(
        progress.last(),
        Some(Progress::Step {
            index: 1,
            status: StepStatus::Failed(_)
        })
    ));

    // An Orca error envelope fails the same way, with its code.
    let mut fake = Fake::default().reply(
        "orchestration task-create",
        Err(api_error(orca_fixture!("error_consumer_fenced.json"))),
    );
    let (result, _) = run(&plan, &mut fake);
    let failure = result.unwrap_err();
    assert_eq!(failure.step, Some(0));
    assert!(
        failure.message.contains("(consumer_fenced)"),
        "{}",
        failure.message
    );
}

#[test]
fn unconfirmed_start_is_a_warning_after_the_deadline() {
    let plan = build_plan(&request(existing())).unwrap();
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        )
        .screens(vec![screen!("spawn_claude_booting_synthetic.txt")]);
    let (result, _) = run(&plan, &mut fake);
    let outcome = result.unwrap();
    assert!(!outcome.verified);
    assert!(fake.clock >= VERIFY_DEADLINE);
    assert!(
        outcome
            .notes
            .iter()
            .any(|n| n.contains("could not confirm")),
        "{:?}",
        outcome.notes
    );
}

#[test]
fn a_vanished_terminal_fails_the_check() {
    let plan = build_plan(&request(existing())).unwrap();
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        );
    fake.screens.push_back(Err(api_error(orca_fixture!(
        "error_terminal_handle_stale.json"
    ))));
    let (result, _) = run(&plan, &mut fake);
    let failure = result.unwrap_err();
    assert_eq!(failure.step, Some(2));
    assert!(failure.message.contains("gone"));
}

#[test]
fn exited_agent_fails_the_wait() {
    let plan = build_plan(&request(existing())).unwrap();
    let mut exited = result(orca_fixture!("terminal_wait_idle.json"));
    exited["wait"]["status"] = json!("exited");
    exited["wait"]["exitCode"] = json!(1);
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply("terminal wait", Ok(exited));
    let failure = run(&plan, &mut fake).0.unwrap_err();
    assert_eq!(
        failure.message,
        "Wait for the agent: the agent exited with code 1"
    );
}

#[test]
fn api_jobs_resolve_against_orca_lists_then_run() {
    let mut fake = Fake {
        catalog: catalog(),
        ..Fake::default()
    }
    .reply(
        "terminal create",
        Ok(result(orca_fixture!("terminal_create.json"))),
    )
    .reply(
        "terminal wait",
        Ok(result(orca_fixture!("terminal_wait_idle.json"))),
    )
    .screens(vec![screen!("spawn_claude_started_synthetic.txt")]);
    let job = Job::Args {
        args: SpawnArgs {
            mode: ModeName::Worktree,
            worktree: Some("commerce-platform/hack-77-admin-speed".into()),
            prompt: PROMPT.into(),
            ..SpawnArgs::default()
        },
        runs: vec![],
        terminals: vec![],
        agents: vec![],
    };
    let mut events = Vec::new();
    run_job(7, job, &mut fake, &mut |e| events.push(e));
    assert!(
        matches!(&events[0], SpawnEvent::Planned { job: 7, plan } if plan.initial.worktree_id.as_deref() == Some(WT_ID))
    );
    assert!(
        matches!(events.last(), Some(SpawnEvent::Done { job: 7, result: Ok(o) }) if o.verified)
    );

    let bad = Job::Args {
        args: SpawnArgs {
            mode: ModeName::Worktree,
            worktree: Some("nope".into()),
            prompt: "x".into(),
            ..SpawnArgs::default()
        },
        runs: vec![],
        terminals: vec![],
        agents: vec![],
    };
    let mut events = Vec::new();
    run_job(8, bad, &mut fake, &mut |e| events.push(e));
    assert!(
        matches!(&events[..], [SpawnEvent::Done { result: Err(f), .. }] if f.step.is_none() && f.message == "no worktree \"nope\"")
    );
}

// ---------------------------------------------------------------------------
// Catalog and API argument resolution

#[test]
fn catalog_groups_and_finds_by_every_name() {
    let c = catalog();
    let groups = c.groups();
    let names: Vec<&str> = groups.iter().map(|g| g.repo.name()).collect();
    assert_eq!(names, ["commerce-platform", "depict", "fitness-rpg"]);
    let cp = &groups[0];
    assert!(cp.worktrees[0].is_main_worktree, "main checkout first");
    assert_eq!(cp.worktrees.len(), 2);

    // The sidebar names repos after paths.
    assert_eq!(c.repo_for_sidebar("commerce-platform").unwrap().id, REPO_ID);
    assert_eq!(c.repo_for_sidebar("depict").unwrap().name(), "depict");
    assert!(c.repo_for_sidebar("nope").is_none());
    assert_eq!(c.find_repo(&format!("id:{REPO_ID}")).unwrap().id, REPO_ID);
    assert_eq!(
        c.find_repo("/Users/gylleus/repo/hackathon/commerce-platform/")
            .unwrap()
            .id,
        REPO_ID
    );

    assert_eq!(c.find_worktree(WT_ID).unwrap().path, WT_PATH);
    assert_eq!(
        c.find_worktree(&format!("id:{WT_ID}")).unwrap().path,
        WT_PATH
    );
    assert_eq!(c.find_worktree(WT_PATH).unwrap().id, WT_ID);
    assert_eq!(c.find_worktree("hack-77-admin-speed").unwrap().id, WT_ID);
    assert_eq!(
        c.find_worktree("commerce-platform/main").unwrap().path,
        "/Users/gylleus/repo/hackathon/commerce-platform"
    );
    let err = c.find_worktree("main").unwrap_err();
    assert!(err.contains("names 3 worktrees"), "{err}");
}

#[test]
fn api_args_parse_and_resolve_per_mode() {
    let c = catalog();
    let runs = runs();
    let terminals = terminals();
    let parse = |v: Value| SpawnArgs::parse(&v);

    let a = parse(
        json!({"mode": "new_worktree", "repo": "commerce-platform", "name": "x",
                         "prompt": "go", "agent": "Codex", "model": "o3"}),
    )
    .unwrap();
    assert_eq!(a.agent, Agent::Codex);
    assert_eq!(SpawnArgs::parse(&a.to_params()).unwrap(), a, "round trip");
    let req = a.resolve(&c, &runs, &terminals).unwrap();
    assert_eq!(
        req.target,
        SpawnTarget::NewWorktree {
            repo_id: REPO_ID.into(),
            name: "x".into(),
            base_branch: None
        }
    );
    assert_eq!(req.model.as_deref(), Some("o3"));

    let a = parse(json!({"mode": "worker", "run_id": "run_7c25cdcf46e0",
                         "worktree": "hack-77-admin-speed", "prompt": "spec"}))
    .unwrap();
    let req = a.resolve(&c, &runs, &terminals).unwrap();
    let SpawnTarget::Worker {
        coordinator,
        placement,
        path,
        ..
    } = req.target
    else {
        panic!()
    };
    assert_eq!(coordinator, "term_df9f48c9-c8e8-4ceb-afca-932028245fb1");
    assert_eq!(
        placement,
        WorkerPlacement::Worktree {
            worktree_id: WT_ID.into()
        }
    );
    assert_eq!(path.as_deref(), Some(WT_PATH));

    let t = terminals
        .iter()
        .find(|t| t.worktree_id.is_some())
        .expect("a terminal with a worktree");
    let a = parse(json!({"mode": "worker", "run_id": "run_7c25cdcf46e0",
                         "terminal": t.handle, "prompt": "spec"}))
    .unwrap();
    let req = a.resolve(&c, &runs, &terminals).unwrap();
    assert!(matches!(
        req.target,
        SpawnTarget::Worker { placement: WorkerPlacement::Terminal { ref handle, .. }, .. } if handle == &t.handle
    ));

    let a = parse(json!({"mode": "worker", "run_id": "run_nope", "worktree": "x", "prompt": "s"}))
        .unwrap();
    assert_eq!(
        a.resolve(&c, &runs, &terminals).unwrap_err(),
        "no run \"run_nope\""
    );
    let a =
        parse(json!({"mode": "new_worktree", "repo": "nope", "name": "x", "prompt": "s"})).unwrap();
    assert_eq!(
        a.resolve(&c, &runs, &terminals).unwrap_err(),
        "no Orca repo \"nope\""
    );
}

#[test]
fn control_spawn_method_parses_into_a_command() {
    use crate::control::commands::{ControlCommand, SPAWN_REPLY_TIMEOUT};
    let parsed = ControlCommand::parse(
        "spawn",
        &json!({"mode": "worktree", "worktree": "depict/main", "prompt": "hi"}),
    )
    .unwrap()
    .unwrap();
    let ControlCommand::Spawn(args) = &parsed else {
        panic!("{parsed:?}")
    };
    assert_eq!(args.worktree.as_deref(), Some("depict/main"));
    assert_eq!(parsed.reply_timeout(), SPAWN_REPLY_TIMEOUT);
    assert!(
        crate::control::server::ACTION_METHODS.contains(&"spawn"),
        "ping lists it"
    );
    let err = ControlCommand::parse("spawn", &json!({"mode": "worktree", "prompt": "hi"}))
        .unwrap()
        .unwrap_err();
    assert!(
        err.message.contains("needs \"worktree\""),
        "{}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Selecting the new node

fn snapshot() -> (FleetSnapshot, String) {
    let data = FleetData {
        terminals: terminals(),
        ..FleetData::default()
    };
    let snapshot = FleetSnapshot::build(&data, 0);
    let handle = snapshot
        .nodes
        .iter()
        .find(|n| n.live && n.terminal_handle.is_some())
        .and_then(|n| n.terminal_handle.clone())
        .unwrap();
    (snapshot, handle)
}

#[test]
fn pending_select_waits_for_the_node_then_expires() {
    let (snap, handle) = snapshot();
    let id = node_for_handle(&snap, &handle).unwrap();
    assert_eq!(
        snap.node(&id).unwrap().terminal_handle.as_deref(),
        Some(handle.as_str())
    );

    let mut pending = Some(PendingSelect::new(handle.clone(), 1_000));
    assert_eq!(take_ready(&mut pending, &snap, 1_000), Some(id));
    assert!(pending.is_none());

    let empty = FleetSnapshot::default();
    let mut pending = Some(PendingSelect::new("term_new", 1_000));
    assert_eq!(take_ready(&mut pending, &empty, 2_000), None);
    assert!(pending.is_some(), "still waiting");
    assert_eq!(
        take_ready(&mut pending, &empty, 1_000 + PENDING_SELECT_MS + 1),
        None
    );
    assert!(pending.is_none(), "gave up");
}

// ---------------------------------------------------------------------------
// Taken names, startup shells

const NEW_WT_ID: &str = "f2c8fcb1-8fa7-4763-8986-a597cba57cdd::/Users/gylleus/orca/workspaces/commerce-platform/spawn-test";
const NEW_AGENT: &str = "term_7d1e4c2a-9b3f-4e8d-a6c5-0f1e2d3c4b5a";

fn working_agent() -> LiveAgent {
    LiveAgent {
        node: "term:term_busy".into(),
        state: "working".into(),
        worktree_path: Some(WT_PATH.into()),
    }
}

#[test]
fn a_taken_worktree_name_or_branch_is_a_conflict() {
    let catalog = catalog();
    // By display name, by `<repo>` directory and by branch; only in the same repo.
    let taken = name_conflict(&catalog, REPO_ID, "hack-77-admin-speed", &[]).unwrap();
    assert_eq!(taken.worktree.as_ref().unwrap().path, WT_PATH);
    assert!(name_conflict(&catalog, "other-repo", "hack-77-admin-speed", &[]).is_none());
    assert!(name_conflict(&catalog, REPO_ID, "fresh-name", &[]).is_none());
    // Orca prefixes branches (`karl/name`); a bare or prefixed branch both count.
    let branches = vec!["main".to_string(), "karl/fresh-name".to_string()];
    let branch = name_conflict(&catalog, REPO_ID, " fresh name ", &branches).unwrap();
    assert!(branch.worktree.is_none());
    assert_eq!(branch.branch.as_deref(), Some("karl/fresh-name"));
    assert!(name_conflict(&catalog, REPO_ID, "name", &branches).is_none());

    let text = taken.message(&[working_agent()]);
    assert!(text.contains(WT_PATH), "{text}");
    assert!(text.contains("term:term_busy (working)"), "{text}");
    assert!(text.contains("send_to_agent"), "{text}");
    assert!(text.contains("allow_suffix=true"), "{text}");
    assert!(taken.message(&[]).contains("no live agent in it"));
    let text = branch.message(&[]);
    assert!(
        text.starts_with("branch karl/fresh-name already exists"),
        "{text}"
    );
}

fn taken_name_job(allow_suffix: bool) -> Job {
    Job::Args {
        args: SpawnArgs {
            mode: ModeName::NewWorktree,
            repo: Some("commerce-platform".into()),
            name: Some("hack-77-admin-speed".into()),
            prompt: PROMPT.into(),
            allow_suffix,
            ..SpawnArgs::default()
        },
        runs: vec![],
        terminals: vec![],
        agents: vec![working_agent()],
    }
}

#[test]
fn api_spawn_into_a_taken_name_fails_before_creating_anything() {
    let mut fake = Fake {
        catalog: catalog(),
        ..Fake::default()
    };
    let mut events = Vec::new();
    run_job(1, taken_name_job(false), &mut fake, &mut |e| events.push(e));
    let [SpawnEvent::Done { result: Err(f), .. }] = &events[..] else {
        panic!("{events:?}");
    };
    assert_eq!(f.step, None);
    assert!(
        f.message.contains("term:term_busy (working)"),
        "{}",
        f.message
    );
    assert!(fake.calls.is_empty(), "{:?}", fake.calls);

    // A worker's new worktree is checked too; a branch alone is enough.
    let mut fake = Fake {
        catalog: catalog(),
        branches: vec!["gylleus/flaky-checkout".into()],
        ..Fake::default()
    };
    let job = Job::Args {
        args: SpawnArgs {
            mode: ModeName::Worker,
            run_id: Some("run_7c25cdcf46e0".into()),
            repo: Some("commerce-platform".into()),
            name: Some("flaky-checkout".into()),
            prompt: PROMPT.into(),
            ..SpawnArgs::default()
        },
        runs: runs(),
        terminals: vec![],
        agents: vec![],
    };
    let mut events = Vec::new();
    run_job(2, job, &mut fake, &mut |e| events.push(e));
    assert!(
        matches!(&events[..], [SpawnEvent::Done { result: Err(f), .. }] if f.message.contains("branch gylleus/flaky-checkout")),
        "{events:?}"
    );

    // allow_suffix leaves the name to Orca.
    let mut fake = Fake {
        catalog: catalog(),
        ..Fake::default()
    }
    .reply(
        "worktree create",
        Err(api_error(orca_fixture!("error_consumer_fenced.json"))),
    );
    let mut events = Vec::new();
    run_job(3, taken_name_job(true), &mut fake, &mut |e| events.push(e));
    assert!(matches!(&events[0], SpawnEvent::Planned { .. }));
    assert_eq!(fake.calls.len(), 1);
}

#[test]
fn allow_suffix_parses_and_round_trips() {
    let params = json!({
        "mode": "new_worktree", "repo": "r", "name": "n", "prompt": "go", "allow_suffix": true,
    });
    let args = SpawnArgs::parse(&params).unwrap();
    assert!(args.allow_suffix);
    assert_eq!(SpawnArgs::parse(&args.to_params()).unwrap(), args);
    let plain =
        SpawnArgs::parse(&json!({"mode": "new_worktree", "repo": "r", "name": "n"})).unwrap();
    assert!(!plain.allow_suffix);
    assert!(plain.to_params().get("allow_suffix").is_none());
    assert!(
        SpawnArgs::parse(&json!({"mode": "new", "repo": "r", "name": "n", "allow_suffix": "yes"}))
            .is_err()
    );
}

fn terminal_row(handle: &str, tab: &str, title: &str, agent: bool) -> Value {
    let mut row = json!({
        "handle": handle, "worktreeId": NEW_WT_ID, "tabId": tab, "title": title,
        "connected": true, "writable": true,
    });
    if agent {
        row["agentIdentity"] = json!("claude");
    }
    row
}

#[test]
fn new_worktree_run_closes_the_startup_shell_but_not_agents_or_setup() {
    let plan = build_plan(&request(new_worktree())).unwrap();
    let list = json!({ "terminals": [
        terminal_row(NEW_AGENT, "tab-3c9e", "claude", false),
        terminal_row("term_shell", "tab-1", "../spawn-test", false),
        terminal_row("term_split", "tab-3c9e", "zsh", false),
        terminal_row("term_setup", "tab-2", "Setup", false),
        terminal_row("term_other_agent", "tab-4", "codex", true),
    ]});
    let mut fake = Fake::default()
        .reply(
            "worktree create",
            Ok(result(orca_fixture!("worktree_create_agent.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        )
        .reply("terminal list", Ok(list))
        .reply("terminal close", Ok(json!({ "close": {} })))
        .reply("terminal close", Ok(json!({ "close": {} })))
        .screens(vec![screen!("spawn_claude_started_synthetic.txt")]);
    let (result, progress) = run(&plan, &mut fake);
    assert!(result.unwrap().verified);
    assert_eq!(
        fake.calls[2],
        args(&["terminal", "list", &format!("--worktree=id:{NEW_WT_ID}")])
    );
    // Its own tab goes whole; a pane split beside the agent only as a pane.
    assert_eq!(
        fake.calls[3..],
        [
            args(&["terminal", "close", "--terminal=term_shell", "--tab"]),
            args(&["terminal", "close", "--terminal=term_split"]),
        ]
    );
    assert!(matches!(
        progress.last(),
        Some(Progress::Step { index: 4, status: StepStatus::Done(Some(n)) }) if n == "closed term_shell, term_split"
    ));
}

#[test]
fn a_failed_shell_close_only_warns() {
    let plan = build_plan(&request(new_worktree())).unwrap();
    let list = json!({ "terminals": [
        terminal_row(NEW_AGENT, "tab-3c9e", "claude", false),
        terminal_row("term_shell", "tab-1", "../spawn-test", false),
    ]});
    let mut fake = Fake::default()
        .reply(
            "worktree create",
            Ok(result(orca_fixture!("worktree_create_agent.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        )
        .reply("terminal list", Ok(list))
        .reply(
            "terminal close",
            Err(api_error(orca_fixture!("error_terminal_handle_stale.json"))),
        )
        .screens(vec![screen!("spawn_claude_started_synthetic.txt")]);
    let outcome = run(&plan, &mut fake).0.unwrap();
    assert!(
        outcome
            .notes
            .iter()
            .any(|n| n.starts_with("could not close the startup shell (term_shell:")),
        "{:?}",
        outcome.notes
    );
}

#[test]
fn existing_worktree_plans_leave_other_terminals_alone() {
    let plan = build_plan(&request(existing())).unwrap();
    assert!(!plan.steps.contains(&Step::CloseStartupShell));
    let plan = build_plan(&request(worker(WorkerPlacement::Worktree {
        worktree_id: WT_ID.into(),
    })))
    .unwrap();
    assert!(!plan.steps.contains(&Step::CloseStartupShell));
}

#[test]
fn interrupted_spawns_are_recorded_shown_and_pruned() {
    use super::interrupted::{
        GONE_GRACE_MS, InterruptedSpawn, MAX_AGE_MS, current, load, record, section,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.json");
    let args = json!({"mode": "new_worktree", "repo": "myrstack", "name": "agent-notify-tool", "prompt": "Add a notify tool\nmore"});
    let result = json!({"node": "term:term_a", "handle": "term_a", "worktree_path": "/wt/agent-notify-tool"});
    let now = 10 * MAX_AGE_MS;
    let entry = InterruptedSpawn::new("cancelled", now, &args, &result);
    assert_eq!(entry.prompt, "Add a notify tool");
    record(&path, entry.clone()).unwrap();
    let gone = InterruptedSpawn {
        node: Some("term:term_gone".into()),
        handle: Some("term_gone".into()),
        at_ms: now - GONE_GRACE_MS - 1,
        ..entry.clone()
    };
    record(&path, gone).unwrap();
    let old = InterruptedSpawn {
        at_ms: now - MAX_AGE_MS,
        ..entry.clone()
    };
    record(&path, old).unwrap();
    assert_eq!(
        load(&path, now).len(),
        2,
        "the expired record is not loaded"
    );

    // Matched by terminal handle as well as node id; the gone agent's record is dropped.
    let agents = vec![(
        "dispatch:r:d".to_string(),
        Some("term_a".to_string()),
        "working".to_string(),
    )];
    let shown = current(&path, &agents, now + 60_000);
    assert_eq!(shown, vec![(entry, Some("working".into()))]);
    assert_eq!(load(&path, now).len(), 1);
    let text = section(&shown, now + 60_000);
    assert!(text.starts_with("Interrupted spawns (1)"), "{text}");
    assert!(text.contains("send_to_agent"), "{text}");
    assert!(
        text.contains("- `term:term_a` in /wt/agent-notify-tool (cancelled 1 min ago, now working): \"Add a notify tool\""),
        "{text}"
    );
    assert_eq!(section(&[], now), "");
    assert!(load(&dir.path().join("missing.json"), now).is_empty());
}

#[test]
fn envoy_identity_is_observed_before_readiness_and_survives_save_failure() {
    let plan = build_plan(&request(existing())).unwrap();
    let mut fake = Fake::default().reply(
        "terminal create",
        Ok(result(orca_fixture!("terminal_create.json"))),
    );
    let mut recorded = None;
    let failure = exec::run_plan_observed(&plan, &mut fake, &mut |_| {}, &mut |context| {
        recorded.clone_from(&context.handle);
        Err("disk full".into())
    })
    .unwrap_err();
    assert!(recorded.is_some());
    assert_eq!(failure.context.handle, recorded);
    assert_eq!(
        fake.calls.len(),
        1,
        "no readiness or prompt submission before persistence"
    );
    assert!(fake.sent.is_empty());
}

#[test]
fn envoy_verification_never_accepts_bypass_permissions() {
    let mut plan = build_plan(&request(existing())).unwrap();
    for step in &mut plan.steps {
        if let Step::Verify { allow_bypass, .. } = step {
            *allow_bypass = false;
        }
    }
    let mut fake = Fake::default()
        .reply(
            "terminal create",
            Ok(result(orca_fixture!("terminal_create.json"))),
        )
        .reply(
            "terminal wait",
            Ok(result(orca_fixture!("terminal_wait_idle.json"))),
        )
        .screens(vec![screen!("spawn_claude_bypass_modal_synthetic.txt")]);
    let (result, _) = run(&plan, &mut fake);
    assert!(
        result
            .unwrap_err()
            .message
            .contains("refused the Bypass Permissions")
    );
    assert!(fake.sent.is_empty());
}

#[test]
fn envoy_preflight_refuses_a_live_home_agent_without_a_saved_identity() {
    let catalog = Catalog {
        repos: vec![crate::orca::spawn::Repo {
            id: "repo".into(),
            path: "/repo/myrstack".into(),
            ..Default::default()
        }],
        worktrees: vec![crate::orca::spawn::Worktree {
            id: "home".into(),
            repo_id: Some("repo".into()),
            path: "/worktrees/envoy-home".into(),
            ..Default::default()
        }],
    };
    for terminal in [
        json!({"handle":"existing", "worktreeId":"home", "agentIdentity":"claude", "title":"Renamed"}),
        json!({"handle":"existing", "worktreePath":"/worktrees/envoy-home", "agentIdentity":"codex"}),
        json!({"handle":"existing", "worktreeId":"home", "preview":"❯ Try a prompt"}),
    ] {
        let mut fake = Fake {
            catalog: catalog.clone(),
            ..Default::default()
        }
        .reply("terminal list", Ok(json!({"terminals":[terminal]})));
        let error = crate::envoy::session::prepare(&mut fake, &Default::default()).unwrap_err();
        assert!(error.contains("already in envoy-home (existing)"));
        assert_eq!(fake.calls.len(), 1);
    }
    let mut fake = Fake {
        catalog,
        ..Default::default()
    }
    .reply(
        "terminal list",
        Ok(json!({"terminals":[
            {"handle":"shell", "worktreeId":"home", "preview":"envoy-home %"},
            {"handle":"other", "worktreeId":"elsewhere", "agentIdentity":"claude"}
        ]})),
    );
    assert!(crate::envoy::session::prepare(&mut fake, &Default::default()).is_ok());
    fake.replies.push_back((
        "terminal list",
        Ok(json!({"terminals":[], "truncated":true})),
    ));
    assert!(
        crate::envoy::session::prepare(&mut fake, &Default::default())
            .unwrap_err()
            .contains("incomplete")
    );
    fake.replies.push_back((
        "terminal list",
        Err(OrcaError::Api(crate::orca::ApiError {
            code: "unavailable".into(),
            message: "not running".into(),
            data: Value::Null,
        })),
    ));
    assert!(
        crate::envoy::session::prepare(&mut fake, &Default::default())
            .unwrap_err()
            .contains("Cannot check")
    );
}
