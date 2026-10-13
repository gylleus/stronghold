use std::path::Path;
use std::process::Command;

use super::*;

const PLAYBOOK_MD: &str = "\
# Clan playbook

Intro.

## Topology

```
## not a heading
```

Tree.

## Token economy

Spend little.

## Coordinator duties

Watch the clan.

### Sub

Detail.

## Complexity discipline

Keep it simple.
";

const PROTOCOL_MD: &str = "\
# Protocol

## Lifecycle

Start, work, report.

## Complexity discipline

Workers keep it simple too.
";

const ATTENDANT_MD: &str = "\
# Attendant brief — <campaign>

## First actions

Read the brief.
";

fn write_playbook(dir: &Path) {
    std::fs::write(dir.join(PLAYBOOK), PLAYBOOK_MD).unwrap();
    std::fs::write(dir.join(PROTOCOL), PROTOCOL_MD).unwrap();
    std::fs::write(dir.join(ATTENDANT_BRIEF), ATTENDANT_MD).unwrap();
    std::fs::write(
        dir.join(KICKOFF),
        "# Kickoff <track>\n\n## Scope\n\n<scope>\n",
    )
    .unwrap();
}

fn temp_playbook() -> (tempfile::TempDir, Playbook) {
    let dir = tempfile::tempdir().unwrap();
    write_playbook(dir.path());
    let pb = Playbook::at(dir.path());
    (dir, pb)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

// ---------------------------------------------------------------------------
// Dir resolution

#[test]
fn env_dir_wins() {
    let (env, _) = temp_playbook();
    let (baked, _) = temp_playbook();
    let got = resolve_dir(Some(env.path().into()), baked.path(), None).unwrap();
    assert_eq!(got, env.path());
}

#[test]
fn env_dir_without_files_is_an_error_not_a_fallthrough() {
    let empty = tempfile::tempdir().unwrap();
    let (baked, _) = temp_playbook();
    let err = resolve_dir(Some(empty.path().into()), baked.path(), None).unwrap_err();
    assert!(err.to_string().contains(ENV_DIR), "{err}");
}

#[test]
fn baked_dir_then_home() {
    let (baked, _) = temp_playbook();
    let (home, _) = temp_playbook();
    let missing = baked.path().join("gone");
    assert_eq!(
        resolve_dir(None, baked.path(), Some(home.path().into())).unwrap(),
        baked.path()
    );
    assert_eq!(
        resolve_dir(Some("".into()), &missing, Some(home.path().into())).unwrap(),
        home.path()
    );
}

#[test]
fn no_dir_with_files_is_a_clear_error() {
    let empty = tempfile::tempdir().unwrap();
    let err = resolve_dir(None, empty.path(), Some(empty.path().join("x")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no playbook found"), "{err}");
    assert!(err.contains(ENV_DIR), "{err}");
}

#[test]
fn the_repo_playbook_is_baked_in() {
    assert!(Path::new(BAKED_DIR).join(PLAYBOOK).is_file());
}

// ---------------------------------------------------------------------------
// Sections

#[test]
fn sections_skip_fenced_headings_and_keep_subsections() {
    let s = sections(PLAYBOOK_MD);
    let titles: Vec<&str> = s.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Topology",
            "Token economy",
            "Coordinator duties",
            "Complexity discipline"
        ]
    );
    assert!(s[0].text.contains("## not a heading"));
    assert!(s[2].text.starts_with("## Coordinator duties\n"));
    assert!(s[2].text.contains("### Sub\n\nDetail."));
    assert!(!s[2].text.contains("Complexity"));
}

#[test]
fn section_hit_is_case_insensitive() {
    let (_d, pb) = temp_playbook();
    let got = pb
        .get(&Target::Role(Role::Coordinator), Some("coordinator DUTIES"))
        .unwrap();
    assert_eq!(got.section.as_deref(), Some("Coordinator duties"));
    assert!(got.body.starts_with("## Coordinator duties"));
    assert!(!got.body.contains("Spend little"));
    assert!(got.text().starts_with("Playbook version: "));
}

#[test]
fn section_prefix_matches() {
    let (_d, pb) = temp_playbook();
    let got = pb.get(&Target::Role(Role::Worker), Some("life")).unwrap();
    assert_eq!(got.section.as_deref(), Some("Lifecycle"));
}

#[test]
fn a_title_in_two_files_returns_both() {
    let (_d, pb) = temp_playbook();
    let got = pb
        .get(&Target::Role(Role::Coordinator), Some("complexity"))
        .unwrap();
    assert!(got.body.contains("Keep it simple."));
    assert!(got.body.contains("Workers keep it simple too."));
    assert!(got.body.contains(&format!("<!-- {PROTOCOL} -->")));
}

#[test]
fn section_miss_lists_titles() {
    let (_d, pb) = temp_playbook();
    let err = pb
        .get(&Target::Role(Role::Coordinator), Some("nope"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no section \"nope\""), "{err}");
    for title in ["Topology", "Coordinator duties", "Lifecycle", PROTOCOL] {
        assert!(err.contains(title), "{err} lists {title}");
    }
}

// ---------------------------------------------------------------------------
// Roles and files

#[test]
fn coordinator_gets_playbook_and_protocol() {
    let (_d, pb) = temp_playbook();
    let got = pb.get(&Target::Role(Role::Coordinator), None).unwrap();
    assert_eq!(got.files, [PLAYBOOK, PROTOCOL]);
    assert!(got.body.contains("# Clan playbook"));
    assert!(got.body.contains("# Protocol"));
}

#[test]
fn attendant_gets_brief_and_two_playbook_sections() {
    let (_d, pb) = temp_playbook();
    let got = pb.get(&Target::Role(Role::Attendant), None).unwrap();
    assert_eq!(got.files, [ATTENDANT_BRIEF, PLAYBOOK]);
    assert!(got.body.contains("<campaign>"));
    assert!(got.body.contains("Watch the clan."));
    assert!(got.body.contains("Spend little."));
    assert!(!got.body.contains("Tree."));
    assert!(!got.body.contains("Keep it simple."));
}

#[test]
fn worker_gets_protocol_only() {
    let (_d, pb) = temp_playbook();
    let got = pb.get(&Target::Role(Role::Worker), None).unwrap();
    assert_eq!(got.files, [PROTOCOL]);
    assert_eq!(got.body, PROTOCOL_MD.trim_end());
}

#[test]
fn envoy_gets_its_playbook_only() {
    let (dir, pb) = temp_playbook();
    std::fs::write(
        dir.path().join(ENVOY),
        "# Envoy\n\n## Guardrails\n\nDraft only.\n",
    )
    .unwrap();
    assert_eq!(Role::parse(" Envoy "), Some(Role::Envoy));
    let got = pb.get(&Target::parse("envoy"), None).unwrap();
    assert_eq!(got.target, Target::Role(Role::Envoy));
    assert_eq!(got.files, [ENVOY]);
    assert_eq!(got.body, "# Envoy\n\n## Guardrails\n\nDraft only.");
    assert!(
        got.text().starts_with("Playbook version: hash "),
        "{}",
        got.text()
    );
}

#[test]
fn the_repo_envoy_playbook_has_its_tool_reference() {
    let got = Playbook::at(BAKED_DIR)
        .get(&Target::Role(Role::Envoy), Some("tool reference"))
        .unwrap();
    for tool in crate::envoy::control::METHODS {
        assert!(got.body.contains(&format!("`{tool}`")), "{tool}");
    }
}

#[test]
fn workers_and_coordinators_are_told_to_reach_people_through_the_envoy() {
    for role in [Role::Worker, Role::Coordinator] {
        let got = Playbook::at(BAKED_DIR)
            .get(
                &Target::Role(role),
                Some("reaching people outside the clan"),
            )
            .unwrap();
        assert!(got.body.contains("`reach_out`"), "{role:?}");
    }
}

#[test]
fn envoy_launch_prompt_points_at_the_system_prompt_and_states_dry_run() {
    for (dry_run, mode) in [(true, "Dry-run is ON"), (false, "Dry-run is OFF")] {
        let prompt = envoy_launch_prompt(dry_run);
        assert!(prompt.contains("system prompt"), "{prompt}");
        assert!(!prompt.contains("get_playbook"), "{prompt}");
        assert!(prompt.contains(mode), "{prompt}");
    }
}

#[test]
fn envoy_system_prompt_is_the_served_envoy_role_under_its_version() {
    let pb = Playbook::at(BAKED_DIR);
    let text = envoy_system_prompt(&pb).unwrap();
    let served = pb.get(&Target::Role(Role::Envoy), None).unwrap();
    assert!(text.starts_with("# Envoy standing rules\n\n"));
    assert!(text.contains(&served.version.header()));
    assert!(text.ends_with(&served.body));
}

#[test]
fn files_by_loose_name() {
    let (_d, pb) = temp_playbook();
    for name in [
        "KICKOFF-TEMPLATE.md",
        "kickoff-template",
        "Kickoff-Template.MD",
    ] {
        let got = pb.get(&Target::parse(name), None).unwrap();
        assert_eq!(got.files, [KICKOFF], "{name}");
    }
    let got = pb.get(&Target::parse("agent_protocol"), None).unwrap();
    assert_eq!(got.files, [PROTOCOL]);
    let err = pb.get(&Target::parse("nope.md"), None).unwrap_err();
    assert!(err.to_string().contains("Files: "), "{err}");
}

#[test]
fn missing_role_file_is_an_error() {
    let (dir, pb) = temp_playbook();
    std::fs::remove_file(dir.path().join(PROTOCOL)).unwrap();
    let err = pb
        .get(&Target::Role(Role::Worker), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains(PROTOCOL) && err.contains("not found"), "{err}");
}

#[test]
fn listing_has_files_sections_and_roles() {
    let (dir, pb) = temp_playbook();
    std::fs::write(dir.path().join("EXTRA.md"), "# x\n\n## Y\n").unwrap();
    let listing = pb.listing().unwrap();
    let names: Vec<&str> = listing.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [PLAYBOOK, PROTOCOL, ATTENDANT_BRIEF, KICKOFF, "EXTRA.md"]
    );
    assert_eq!(
        listing.files[1].sections,
        ["Lifecycle", "Complexity discipline"]
    );
    let text = listing.text();
    assert!(text.contains("- coordinator: AGENT-FLEET-PLAYBOOK.md + _AGENT_PROTOCOL.md"));
    assert!(text.contains("Topology · Token economy"));
    assert_eq!(listing.to_json()["roles"][1]["name"], "attendant");
}

// ---------------------------------------------------------------------------
// Version

#[test]
fn version_outside_git_is_hash_only() {
    let (_d, pb) = temp_playbook();
    let got = pb.get(&Target::Role(Role::Worker), None).unwrap();
    assert!(!got.version.in_git);
    assert_eq!(got.version.commit, None);
    assert_eq!(got.version.hash, short_hash(&got.body));
    assert_eq!(
        got.version.header(),
        format!("Playbook version: hash {} (not in git)", got.version.hash)
    );
}

#[test]
fn version_in_git_has_commit_and_dirty_flag() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path().join("playbook");
    std::fs::create_dir(&dir).unwrap();
    write_playbook(&dir);
    git(repo.path(), &["init", "-q"]);
    let pb = Playbook::at(&dir);

    let before = pb.version("x");
    assert!(before.in_git);
    assert_eq!(before.commit, None);
    assert!(before.dirty, "untracked files are dirty");
    assert!(before.to_string().starts_with("uncommitted (dirty)"));

    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "playbook"]);
    let clean = pb.get(&Target::Role(Role::Worker), None).unwrap().version;
    let head = Command::new("git")
        .env_remove("GIT_DIR")
        .arg("-C")
        .arg(repo.path())
        .args(["log", "-1", "--format=%h"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    assert_eq!(clean.commit.as_deref(), Some(head.as_str()));
    assert!(!clean.dirty);
    assert_eq!(
        clean.header(),
        format!("Playbook version: {head} (clean) · hash {}", clean.hash)
    );

    // A change outside the playbook dir leaves it clean.
    std::fs::write(repo.path().join("other.txt"), "x").unwrap();
    assert!(!pb.version("x").dirty);

    std::fs::write(dir.join(PROTOCOL), "# Protocol v2\n").unwrap();
    let dirty = pb.get(&Target::Role(Role::Worker), None).unwrap().version;
    assert!(dirty.dirty);
    assert_ne!(dirty.hash, clean.hash);
    assert!(dirty.to_string().contains("(dirty)"));
    assert_eq!(dirty.to_json()["dirty"], true);
}

#[test]
fn version_follows_symlinks_into_the_repo() {
    let repo = tempfile::tempdir().unwrap();
    write_playbook(repo.path());
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "playbook"]);
    let links = tempfile::tempdir().unwrap();
    for f in [PLAYBOOK, PROTOCOL] {
        std::os::unix::fs::symlink(repo.path().join(f), links.path().join(f)).unwrap();
    }
    let v = Playbook::at(links.path()).version("x");
    assert!(v.in_git && v.commit.is_some() && !v.dirty, "{v:?}");
}

#[test]
fn short_hash_is_stable() {
    assert_eq!(short_hash(""), "cbf29ce");
    assert_eq!(short_hash("a").len(), 7);
    assert_ne!(short_hash("a"), short_hash("b"));
}

// ---------------------------------------------------------------------------
// CLI parsing (the dir comes from locate(), so only the error paths run here)

#[test]
fn cli_rejects_bad_options() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(cli_text(&args(&["--bogus"])).is_err());
    assert!(cli_text(&args(&["worker", "coordinator"])).is_err());
    assert!(cli_text(&args(&["--section"])).is_err());
    let usage = cli_text(&args(&["--help"])).unwrap();
    assert!(
        usage.starts_with("usage:") && usage.contains("|envoy|"),
        "{usage}"
    );
    assert!(run_cli(&args(&["mcp"])).is_none());
}

#[test]
fn template_subcommands_need_exactly_one_id() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let e = cli_text(&args(&["kickoff"])).unwrap_err().to_string();
    assert_eq!(e, "usage: myrstack playbook kickoff <task_id>");
    let e = cli_text(&args(&["attendant-brief", "a", "b"]))
        .unwrap_err()
        .to_string();
    assert_eq!(e, "usage: myrstack playbook attendant-brief <run_id>");
    let e = cli_text(&args(&["attendant-brief", "--json"]))
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("usage:"), "{e}");
    let help = cli_text(&args(&["kickoff", "--help"])).unwrap();
    assert!(
        help.contains("myrstack playbook kickoff <task_id>"),
        "{help}"
    );
}
