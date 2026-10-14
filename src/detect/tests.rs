use super::*;

macro_rules! screen {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/screens/",
            $name
        ))
    };
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

const CLAUDE: Option<&AgentIdentity> = Some(&AgentIdentity::Claude);
const CODEX: Option<&AgentIdentity> = Some(&AgentIdentity::Codex);

fn detect(text: &str, agent: Option<&AgentIdentity>) -> Option<PromptKind> {
    detect_prompt(&lines(text), agent)
}

fn choice(options: &[&str]) -> Option<PromptKind> {
    Some(PromptKind::Choice {
        options: options.iter().map(|o| o.to_string()).collect(),
    })
}

#[test]
fn claude_prompts() {
    let cases = [
        (
            screen!("claude_choice_ask_user_question.txt"),
            choice(&[
                "Submit that draft, then forward mine",
                "Discard it, forward mine only",
                "Leave it, I'll handle it myself",
                "Type something.",
                "Chat about this",
            ]),
        ),
        (
            screen!("claude_permission_bash_synthetic.txt"),
            Some(PromptKind::Permission),
        ),
        (
            screen!("claude_permission_edit_synthetic.txt"),
            Some(PromptKind::Permission),
        ),
        (
            screen!("claude_plan_approval_synthetic.txt"),
            Some(PromptKind::Permission),
        ),
    ];
    for (i, (text, expected)) in cases.into_iter().enumerate() {
        assert_eq!(detect(text, CLAUDE), expected, "case {i}");
        assert_eq!(detect(text, None), expected, "case {i} without agent");
    }
}

#[test]
fn codex_prompts() {
    let cases = [
        (
            screen!("codex_approval_command_synthetic.txt"),
            Some(PromptKind::Permission),
        ),
        (
            screen!("codex_approval_edits_synthetic.txt"),
            Some(PromptKind::Permission),
        ),
        (
            screen!("codex_model_switch_synthetic.txt"),
            choice(&[
                "Switch to gpt-5.6-luna-mini",
                "Keep current model",
                "Keep current model (never show again)",
            ]),
        ),
    ];
    for (i, (text, expected)) in cases.into_iter().enumerate() {
        assert_eq!(detect(text, CODEX), expected, "case {i}");
        assert_eq!(detect(text, None), expected, "case {i} without agent");
    }
}

#[test]
fn shell_prompts() {
    assert_eq!(
        detect(screen!("shell_confirm_yn_synthetic.txt"), None),
        Some(PromptKind::Confirm)
    );
    assert_eq!(
        detect(screen!("shell_password_synthetic.txt"), None),
        Some(PromptKind::FreeformQuestion)
    );
    assert_eq!(
        detect(screen!("shell_answered_yn_synthetic.txt"), None),
        None
    );
}

#[test]
fn idle_and_working_screens_are_not_prompts() {
    let claude = [
        screen!("claude_idle_bypass.txt"),
        screen!("claude_idle_prose_question.txt"),
        screen!("claude_idle_user_message.txt"),
        screen!("claude_idle_fresh_session.txt"),
        screen!("claude_idle_after_clear.txt"),
        screen!("claude_idle_mentions_approve.txt"),
        screen!("claude_idle_numbered_list.txt"),
        screen!("claude_working_synthetic.txt"),
        screen!("claude_old_prompt_in_scrollback_synthetic.txt"),
    ];
    for text in claude {
        assert_eq!(detect(text, CLAUDE), None, "false positive:\n{text}");
        assert_eq!(
            detect(text, None),
            None,
            "false positive (no agent):\n{text}"
        );
    }
    let codex = [
        screen!("codex_idle.txt"),
        screen!("codex_idle_startup_warnings.txt"),
        screen!("codex_idle_animation.txt"),
        screen!("codex_working_synthetic.txt"),
    ];
    for text in codex {
        assert_eq!(detect(text, CODEX), None, "false positive:\n{text}");
        assert_eq!(
            detect(text, None),
            None,
            "false positive (no agent):\n{text}"
        );
    }
}

#[test]
fn only_the_bottom_counts() {
    let prompt = screen!("claude_permission_bash_synthetic.txt");
    assert_eq!(detect(prompt, CLAUDE), Some(PromptKind::Permission));

    // The same dialog scrolled above the window by later output.
    let mut scrolled = lines(prompt);
    scrolled.extend((0..SCREEN_WINDOW).map(|i| format!("  output line {i}")));
    assert_eq!(detect_prompt(&scrolled, CLAUDE), None);

    // Trailing blank rows do not push the dialog out of the window.
    let mut padded = lines(prompt);
    padded.extend(std::iter::repeat_n(" ".repeat(40), 50));
    assert_eq!(detect_prompt(&padded, CLAUDE), Some(PromptKind::Permission));

    // An input line below the dialog makes it history.
    let mut answered = lines(prompt);
    answered.extend(lines("────────\n❯ \n────────\n  ⏵⏵ auto mode on"));
    assert_eq!(detect_prompt(&answered, CLAUDE), None);

    // Codex's input line is `›`; Claude's `❯` does not cut a Codex screen.
    let codex = screen!("codex_approval_command_synthetic.txt");
    let mut after = lines(codex);
    after.push("› Ask Codex to do anything".into());
    assert_eq!(detect_prompt(&after, CODEX), None);
    let mut foreign = lines("❯ earlier text");
    foreign.extend(lines(codex));
    assert_eq!(detect_prompt(&foreign, CODEX), Some(PromptKind::Permission));
}

#[test]
fn empty_screens() {
    assert_eq!(detect_prompt(&[], CLAUDE), None);
    assert_eq!(detect_prompt(&lines("\n  \n"), None), None);
}

#[test]
fn options_are_parsed() {
    let cases = [
        (
            "❯ 1. Yes",
            Some(MenuOption {
                cursor: true,
                number: 1,
                label: "Yes",
            }),
        ),
        (
            "  12. Twelfth  with a description",
            Some(MenuOption {
                cursor: false,
                number: 12,
                label: "Twelfth",
            }),
        ),
        (
            "› 2. No, and tell Codex what to do differently (esc)",
            Some(MenuOption {
                cursor: true,
                number: 2,
                label: "No, and tell Codex what to do differently (esc)",
            }),
        ),
        ("❯", None),
        ("❯ 1.", None),
        ("  1.5 GB free", None),
        ("  123. too many digits", None),
        ("│ 1. inside a box", None),
        ("v1. not an option", None),
    ];
    for (line, expected) in cases {
        assert_eq!(parse_option(line), expected, "{line:?}");
    }
}

#[test]
fn menus_need_a_cursor_or_hint_and_numbering_from_one() {
    // A numbered list in output: no cursor, no hint.
    assert_eq!(detect("  1. first\n  2. second\n  3. third", None), None);
    // With a cursor it is a live menu.
    assert_eq!(
        detect("Pick one\n  1. first\n❯ 2. second\n  3. third", None),
        choice(&["first", "second", "third"])
    );
    // Numbering must start at 1.
    assert_eq!(detect("❯ 2. second\n  3. third", None), None);
    // A lone option is not a menu.
    assert_eq!(detect("❯ 1. only", None), None);
    // Descriptions between options are skipped.
    assert_eq!(
        detect(
            "❯ 1. Alpha\n     about alpha\n     more\n  2. Beta\n     about beta\n\
             Enter to select · ↑/↓ to navigate · Esc to cancel",
            None
        ),
        choice(&["Alpha", "Beta"])
    );
    // Yes/No options under a selection hint stay a choice (AskUserQuestion).
    assert_eq!(
        detect(
            "❯ 1. Yes\n  2. No\nEnter to select · ↑/↓ to navigate · Esc to cancel",
            None
        ),
        choice(&["Yes", "No"])
    );
    // Yes/No options without one are an approval.
    assert_eq!(
        detect("❯ 1. Yes\n  2. No\nEsc to cancel", None),
        Some(PromptKind::Permission)
    );
}

#[test]
fn confirmations() {
    for text in [
        "Overwrite existing file? [y/N]",
        "Proceed (y/n)?",
        "Are you sure you want to continue connecting (yes/no/[fingerprint])?",
        "some text\n  Press enter to continue",
    ] {
        assert_eq!(detect(text, None), Some(PromptKind::Confirm), "{text}");
    }
    // Only the bottom rows count for a bare y/n.
    assert_eq!(detect("Continue? [y/N]\nyes\nok\ndone", None), None);
}

#[test]
fn preview_detection() {
    let prompts = [
        (
            "about switching models.   Press enter to confirm or esc to go back   \
             › Ask Codex to do anything   gpt-5.6-luna medium · ~/orca/workspaces/shop/x",
            PromptKind::Confirm,
        ),
        (
            "  4. Type something.\n  5. Chat about this\n\
             Enter to select · ↑/↓ to navigate · Esc to cancel",
            PromptKind::Choice {
                options: vec!["Type something.".into(), "Chat about this".into()],
            },
        ),
        (
            "Esc to cancel · Tab to amend · ctrl+e to explain",
            PromptKind::Permission,
        ),
    ];
    for (preview, expected) in prompts {
        assert_eq!(detect_in_preview(preview), Some(expected), "{preview}");
    }
    assert_eq!(detect_in_preview(""), None);
    assert_eq!(
        detect_in_preview("⏵⏵ auto mode on (shift+tab to cycle) · ← for agents"),
        None
    );
}

mod claude_session_live {
    use crate::detect::claude_session_live;
    use crate::orca::{AgentIdentity, Terminal};

    fn term(agent: Option<AgentIdentity>, preview: &str) -> Terminal {
        Terminal {
            handle: "term_c".into(),
            connected: true,
            agent_identity: agent,
            preview: Some(preview.into()),
            ..Default::default()
        }
    }

    #[test]
    fn idle_and_working_claude_screens_are_live() {
        for preview in [
            "some transcript\nnew task? /clear to save 319.5k tokens",
            "done.\n❯\u{a0}add the error type name",
            "reading files\n✢ Brewing… (esc to interrupt)",
            "Flummoxing…                6",
            "⏺ Done. Merged nothing.\n",
        ] {
            assert!(
                claude_session_live(&term(Some(AgentIdentity::Claude), preview)),
                "{preview:?}"
            );
        }
    }

    #[test]
    fn shells_and_other_agents_are_not() {
        let shell = "⏺ old transcript line\n~/orca/workspaces/x  karl/redis-writes ✔        9m";
        for (agent, preview) in [
            (Some(AgentIdentity::Claude), shell),
            (
                Some(AgentIdentity::Claude),
                "new task? /clear to save\n~/repo %",
            ),
            (Some(AgentIdentity::Claude), "⏺ hi\nkarl@mac repo $"),
            (Some(AgentIdentity::Claude), "nothing recognisable"),
            (Some(AgentIdentity::Codex), "⏺ hi\n› type here"),
            (None, "new task? /clear to save"),
        ] {
            assert!(!claude_session_live(&term(agent, preview)), "{preview:?}");
        }
    }

    #[test]
    fn the_title_settles_a_preview_without_claude_markers() {
        let titled = |title: &str, preview: &str| Terminal {
            title: Some(title.into()),
            ..term(Some(AgentIdentity::Claude), preview)
        };
        let warning = "ule (../.claude/settings.json): Bash(rg --files -g '*.ts') has a wildcard";
        for title in ["✳ Envoy listener dwarf", "◑ Recs eval", "⠙ Working"] {
            assert!(claude_session_live(&titled(
                title,
                &format!("{warning}\n❯")
            )));
            assert!(claude_session_live(&titled(title, warning)));
            assert!(!claude_session_live(&titled(title, "⏺ hi\n~/repo %")));
        }
        for title in ["..launch-health", "k@mac: ~/repo", ""] {
            assert!(!claude_session_live(&titled(
                title,
                &format!("{warning}\n❯")
            )));
            assert!(!claude_session_live(&titled(title, warning)));
        }
        let codex = Terminal {
            title: Some("✳ odd".into()),
            ..term(Some(AgentIdentity::Codex), "❯")
        };
        assert!(!claude_session_live(&codex));
    }
}

#[test]
fn composer_state_reads_the_input_line_off_a_rendered_screen() {
    use crate::detect::{ComposerState, composer_state};
    for text in [
        screen!("claude_idle_bypass.txt"),
        screen!("claude_idle_user_message.txt"),
        screen!("claude_idle_after_clear.txt"),
        screen!("claude_idle_numbered_list.txt"),
        screen!("claude_idle_fresh_session.txt"),
    ] {
        assert_eq!(
            composer_state(&lines(text), CLAUDE),
            ComposerState::Empty,
            "idle:\n{text}"
        );
        assert_eq!(composer_state(&lines(text), None), ComposerState::Empty);
    }
    for text in [
        screen!("claude_working_synthetic.txt"),
        screen!("claude_permission_bash_synthetic.txt"),
        screen!("claude_choice_ask_user_question.txt"),
    ] {
        assert_eq!(
            composer_state(&lines(text), CLAUDE),
            ComposerState::Absent,
            "busy or prompting:\n{text}"
        );
    }
    // The live Envoy's screen during the stall: the finished-turn line, the composer and the
    // hint under it that Orca's preview kept instead of the composer.
    let envoy = "⏺ Ignoring this one. It doesn't mention Karl or ask him for anything.\n\n✻ Cooked for 2s · done 3:12 PM\n\n────────\n❯\n────────\n  ⏵⏵ don't ask on (shift+tab to cycle) · ← for agents\nnew task? /clear to save 101.9k tokens\n";
    assert_eq!(composer_state(&lines(envoy), CLAUDE), ComposerState::Empty);
    let typed = envoy.replace("\n❯\n", "\n❯ half-written reply\n");
    assert_eq!(composer_state(&lines(&typed), CLAUDE), ComposerState::Text);
    assert_eq!(
        composer_state(&lines("user@host ~ %"), CLAUDE),
        ComposerState::Absent
    );
    assert_eq!(composer_state(&[], CLAUDE), ComposerState::Absent);
    assert_eq!(
        composer_state(&lines(screen!("codex_idle.txt")), CODEX),
        ComposerState::Empty,
        "Codex's placeholder is an empty composer"
    );
    assert_eq!(
        composer_state(
            &lines("❯ Try \"fix lint errors\"\n  ? for shortcuts"),
            CLAUDE
        ),
        ComposerState::Empty
    );
    assert_eq!(
        composer_state(&lines("❯ Try harder\n  ? for shortcuts"), CLAUDE),
        ComposerState::Text
    );
}
