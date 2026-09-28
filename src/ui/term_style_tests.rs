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

fn style(text: &str, agent: AgentKind) -> Vec<StyledLine> {
    style_lines(&lines(text), agent)
}

#[test]
#[ignore = "prints a fixture's styling: SCREEN=<file> AGENT=claude|codex"]
fn dump() {
    let name = std::env::var("SCREEN").unwrap();
    let agent = match std::env::var("AGENT").as_deref() {
        Ok("codex") => AgentKind::Codex,
        Ok("transcript") => AgentKind::Transcript,
        Ok("unknown") => AgentKind::Unknown,
        _ => AgentKind::Claude,
    };
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/screens/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    for l in style(&text, agent) {
        let spans: Vec<String> = l
            .body
            .iter()
            .filter(|s| !matches!(s.style, Style::Text | Style::Dim | Style::Accent))
            .map(|s| {
                format!(
                    "{:?}:{}",
                    s.style,
                    s.text.chars().take(30).collect::<String>()
                )
            })
            .collect();
        let t: String = l.text().chars().take(110).collect();
        println!(
            "{:<12} {:<4} |{}|  {}",
            format!("{:?}", l.kind),
            if l.prose { "P" } else { "" },
            t,
            spans.join(" ")
        );
    }
}

/// The first styled line whose text contains `needle`.
fn find<'a>(styled: &'a [StyledLine], needle: &str) -> &'a StyledLine {
    styled
        .iter()
        .find(|l| l.text().contains(needle))
        .unwrap_or_else(|| panic!("no line contains {needle:?}"))
}

fn kind_of(styled: &[StyledLine], needle: &str) -> LineKind {
    find(styled, needle).kind
}

fn span_style(line: &StyledLine, text: &str) -> Style {
    line.prefix
        .iter()
        .chain(&line.body)
        .find(|s| s.text.contains(text))
        .unwrap_or_else(|| panic!("no span contains {text:?} in {line:?}"))
        .style
}

#[test]
fn claude_line_kinds() {
    let s = style(
        screen!("claude_update_diff_synthetic.txt"),
        AgentKind::Claude,
    );
    let call = find(&s, "Update(src/ui/terminal.rs)");
    assert_eq!(call.kind, LineKind::ToolCall);
    assert_eq!(span_style(call, "⏺"), Style::ToolBullet);
    assert_eq!(span_style(call, "Update"), Style::ToolName);
    assert_eq!(span_style(call, "src/ui/terminal.rs"), Style::Link);

    let prose = find(&s, "I'll switch the screen view");
    assert_eq!(prose.kind, LineKind::Assistant);
    assert!(prose.prose);
    assert_eq!(span_style(prose, "⏺"), Style::AssistantBullet);

    assert_eq!(kind_of(&s, "Read 970 lines"), LineKind::ToolResult);
    assert_eq!(kind_of(&s, "Error: test result"), LineKind::Error);
    // The continuation of an error result stays an error.
    assert_eq!(kind_of(&s, "thread 'term_style"), LineKind::Error);
    assert_eq!(kind_of(&s, "… +12 lines"), LineKind::ToolResult);
    assert_eq!(kind_of(&s, "Percolating"), LineKind::Status);
    assert_eq!(kind_of(&s, "1 warning emitted"), LineKind::Warning);
    assert_eq!(kind_of(&s, "Summary"), LineKind::Heading);

    let md = find(&s, "One diff assertion");
    assert_eq!(span_style(md, "diff"), Style::Strong);
    assert_eq!(span_style(md, "LayoutJob"), Style::Code);
    assert_eq!(span_style(md, "https://docs.rs/egui"), Style::Link);
    // Markup characters are dropped.
    assert!(!md.text().contains("**") && !md.text().contains('`'));
}

#[test]
fn claude_diff_lines() {
    let s = style(
        screen!("claude_update_diff_synthetic.txt"),
        AgentKind::Claude,
    );
    let del = find(&s, "let text = screen.lines");
    assert_eq!(del.kind, LineKind::DiffDel);
    assert_eq!(del.prefix_text().trim(), "635");
    assert_eq!(span_style(del, "635"), Style::LineNo);
    assert!(del.body_text().starts_with("-"));
    assert_eq!(
        kind_of(&s, "// Cached per screen update."),
        LineKind::DiffAdd
    );
    assert_eq!(kind_of(&s, "fn screen_view"), LineKind::DiffContext);
    assert!(!LineKind::DiffAdd.wraps());

    let s = style(screen!("claude_diff_tail.txt"), AgentKind::Claude);
    assert_eq!(s[0].kind, LineKind::DiffAdd);
    assert_eq!(
        kind_of(&s, "sends the diff and the branch"),
        LineKind::DiffContext
    );

    let s = style(
        screen!("claude_permission_edit_synthetic.txt"),
        AgentKind::Claude,
    );
    assert_eq!(kind_of(&s, "172 -    let preview"), LineKind::DiffDel);
    assert_eq!(kind_of(&s, "172 +    detect_in_preview"), LineKind::DiffAdd);
    assert_eq!(
        kind_of(&s, "170  /// Cheap heuristic"),
        LineKind::DiffContext
    );
    assert_eq!(kind_of(&s, "1. Yes"), LineKind::MenuSelected);
    // The dialog text after the rule is not part of the tool call.
    assert_eq!(
        kind_of(&s, "Do you want to make this edit"),
        LineKind::Plain
    );

    // Write results list the file with line numbers.
    let s = style(screen!("claude_tool_results_write.txt"), AgentKind::Claude);
    assert_eq!(
        kind_of(&s, "1 # Karl's local depict stack"),
        LineKind::DiffContext
    );
}

#[test]
fn claude_chrome_input_and_rules() {
    let s = style(screen!("claude_tool_results_write.txt"), AgentKind::Claude);
    let n = s.len();
    assert_eq!(s[n - 1].kind, LineKind::Chrome);
    assert!(s[n - 1].text().starts_with("⏵⏵ auto mode on"));
    assert_eq!(s[n - 2].kind, LineKind::Rule);
    assert_eq!(s[n - 3].kind, LineKind::Input);
    assert_eq!(s[n - 4].kind, LineKind::Rule);
    // The right-aligned notice is trimmed chrome.
    let notice = find(&s, "Auto-update failed");
    assert_eq!(notice.kind, LineKind::Chrome);
    assert!(notice.text().starts_with('✘'));

    let s = style(
        screen!("claude_update_diff_synthetic.txt"),
        AgentKind::Claude,
    );
    let footer = find(&s, "? for shortcuts");
    assert_eq!(footer.kind, LineKind::Chrome);
    assert_eq!(span_style(footer, "bypass"), Style::Faint);
    assert!(
        s.iter()
            .any(|l| l.kind == LineKind::Input && l.text() == ">")
    );
}

#[test]
fn claude_user_prompt_status_and_summaries() {
    let s = style(screen!("claude_idle_user_message.txt"), AgentKind::Claude);
    let user = find(&s, "is it still running?");
    assert_eq!(user.kind, LineKind::UserPrompt);
    assert_eq!(span_style(user, "❯"), Style::Accent);
    assert_eq!(kind_of(&s, "Brewed for 39s"), LineKind::Status);
    assert_eq!(
        span_style(find(&s, "Brewed for 39s"), "Brewed"),
        Style::Status
    );
    assert_eq!(kind_of(&s, "Ran 2 shell commands"), LineKind::ToolResult);
    assert_eq!(kind_of(&s, "12 skills available"), LineKind::ToolResult);
}

#[test]
fn wrapped_prose_is_joined() {
    let s = style(screen!("claude_markdown_table.txt"), AgentKind::Claude);
    let para = find(&s, "Here's the full picture from Linear");
    // The terminal wrapped this paragraph; its second row is joined back.
    assert!(
        para.text().contains("issues in flight, ~30 open"),
        "{}",
        para.text()
    );
    // List items keep their marker as the hanging prefix and join their continuation.
    let item = find(&s, "VROOM-1496 (High)");
    assert_eq!(item.prefix_text(), "  - ");
    assert!(item.text().contains("rewritten or closed."));
    // Short lines are never joined.
    assert_eq!(
        find(&s, "Projects (the containers)").body_text(),
        "Projects (the containers)"
    );
    assert_eq!(
        kind_of(&s, "│ Make Recommendations & Search testable"),
        LineKind::Table
    );
    let table = find(&s, "┌───");
    assert!(!table.kind.wraps());
    assert_eq!(span_style(table, "┌"), Style::Faint);
}

#[test]
fn codex_line_kinds() {
    let s = style(screen!("codex_diff_plan.txt"), AgentKind::Codex);
    assert_eq!(kind_of(&s, "186 +Falsifiers"), LineKind::DiffAdd);
    let ran = find(&s, "Ran git status --short");
    assert_eq!(ran.kind, LineKind::ToolCall);
    assert_eq!(span_style(ran, "Ran"), Style::ToolName);
    assert_eq!(span_style(ran, "•"), Style::ToolBullet);
    assert_eq!(kind_of(&s, "└ (no output)"), LineKind::ToolResult);
    assert_eq!(
        kind_of(&s, "│ from pathlib import Path"),
        LineKind::ToolCall
    );
    assert_eq!(
        kind_of(&s, "Heads up, you have less than 10%"),
        LineKind::Warning
    );
    let assistant = find(&s, "I recommend using the existing dumps");
    assert_eq!(assistant.kind, LineKind::Assistant);
    assert!(
        assistant.text().contains("bottleneck yet"),
        "wrapped row is joined"
    );
    let worked = find(&s, "Worked for 3m 26s");
    assert_eq!(worked.kind, LineKind::Rule);
    assert_eq!(worked.text(), "Worked for 3m 26s");
    let n = s.len();
    assert_eq!(s[n - 2].kind, LineKind::Input);
    assert_eq!(s[n - 2].body_text(), "Ask Codex to do anything");
    assert_eq!(s[n - 1].kind, LineKind::Chrome);
}

#[test]
fn codex_working_and_approval() {
    let s = style(screen!("codex_working_braille.txt"), AgentKind::Codex);
    assert_eq!(kind_of(&s, "Working (1h 01m 02s"), LineKind::Status);
    let input = s.iter().find(|l| l.kind == LineKind::Input).expect("input");
    // The idle animation's braille is stripped from the input line.
    assert_eq!(input.body_text(), "Ask Codex to do anything");
    assert!(s.iter().all(|l| !l.text().contains('⠁')));
    let footer = s.last().unwrap();
    assert_eq!(footer.kind, LineKind::Chrome);
    assert!(footer.text().starts_with("gpt-6-astra high"));

    let s = style(
        screen!("codex_approval_edits_synthetic.txt"),
        AgentKind::Codex,
    );
    assert_eq!(kind_of(&s, "Yes, proceed (y)"), LineKind::MenuSelected);
    assert_eq!(kind_of(&s, "9 +pub mod detect;"), LineKind::DiffAdd);
    assert_eq!(kind_of(&s, "8  pub mod claude;"), LineKind::DiffContext);
    assert_eq!(kind_of(&s, "Would you like to make"), LineKind::Plain);
}

#[test]
fn transcript_lines() {
    let tail = lines(
        "> fix the bug\n\
         I'll look at **state.rs** first.\n\
         → Read(src/model/state.rs)\n  \
         ← pub fn derive_state(\n\
         → Bash(cargo test)\n  \
         ← error: test failed\n\
         ## Done\n\
         ```rust\n\
         let x = 1;\n\
         ```\n\
         - fixed `derive_state`",
    );
    let s = style_lines(&tail, AgentKind::Transcript);
    let kinds: Vec<LineKind> = s.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![
            LineKind::UserPrompt,
            LineKind::Assistant,
            LineKind::ToolCall,
            LineKind::ToolResult,
            LineKind::ToolCall,
            LineKind::Error,
            LineKind::Heading,
            LineKind::Code,
            LineKind::Code,
            LineKind::Code,
            LineKind::Assistant,
        ]
    );
    assert_eq!(span_style(&s[1], "state.rs"), Style::Strong);
    assert_eq!(span_style(&s[2], "Read"), Style::ToolName);
    assert_eq!(span_style(&s[2], "→"), Style::ToolBullet);
    assert_eq!(s[6].body_text(), "Done");
    assert_eq!(s[8].body_text(), "let x = 1;");
    assert_eq!(s[10].prefix_text(), "- ");
    assert_eq!(span_style(&s[10], "derive_state"), Style::Code);
    // Transcript lines are never joined, however long.
    let long = vec!["a ".repeat(150), "  b".to_string()];
    assert_eq!(style_lines(&long, AgentKind::Transcript).len(), 2);
}

#[test]
fn unknown_agent_is_detected_or_left_plain() {
    let claude = style(screen!("claude_working_synthetic.txt"), AgentKind::Unknown);
    assert_eq!(kind_of(&claude, "Read(src/poller.rs)"), LineKind::ToolCall);
    let shell = style(screen!("shell_password_synthetic.txt"), AgentKind::Unknown);
    assert!(shell.iter().all(|l| !l.prose));
}

#[test]
fn inline_links_and_errors() {
    let spans = inline_spans(
        "see (src/app.rs:12), https://x.io/a. and e.g. 3/4 then failed",
        Style::Text,
        false,
    );
    let styled: Vec<(&str, Style)> = spans
        .iter()
        .filter(|s| s.style != Style::Text)
        .map(|s| (s.text.as_str(), s.style))
        .collect();
    assert_eq!(
        styled,
        vec![
            ("src/app.rs:12", Style::Link),
            ("https://x.io/a", Style::Link),
            ("failed", Style::Error),
        ]
    );
    // Nothing is lost.
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        text,
        "see (src/app.rs:12), https://x.io/a. and e.g. 3/4 then failed"
    );
}

#[test]
fn styling_is_stable() {
    for (text, agent) in [
        (screen!("claude_tool_results_write.txt"), AgentKind::Claude),
        (screen!("claude_markdown_table.txt"), AgentKind::Claude),
        (screen!("codex_diff_plan.txt"), AgentKind::Codex),
        (screen!("codex_working_braille.txt"), AgentKind::Codex),
    ] {
        assert_eq!(style(text, agent), style(text, agent));
    }
}

#[test]
fn every_fixture_keeps_its_text() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/screens");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Unknown] {
            let styled = style(&text, agent);
            assert!(!styled.is_empty() || text.trim().is_empty(), "{path:?}");
            // No logical line is empty-but-typed except blanks, rules, chrome and input.
            for l in &styled {
                if l.text().trim().is_empty() {
                    assert!(
                        matches!(
                            l.kind,
                            LineKind::Blank | LineKind::Rule | LineKind::Chrome | LineKind::Input
                        ),
                        "{path:?}: empty {:?}",
                        l.kind
                    );
                }
            }
        }
    }
}

#[test]
fn palette_differs_by_theme_and_stays_legible() {
    let dark = Palette::new(&eframe::egui::Visuals::dark());
    let light = Palette::new(&eframe::egui::Visuals::light());
    assert!(dark.dark && !light.dark);
    assert_ne!(dark.accent, light.accent);
    assert_ne!(dark.add, light.add);
    for p in [dark, light] {
        // Foregrounds differ from the screen background, and chrome is dimmer than text.
        for style in [
            Style::Text,
            Style::Dim,
            Style::Faint,
            Style::Accent,
            Style::DiffAdd,
            Style::Error,
        ] {
            assert_ne!(p.color(style), p.screen_bg, "{style:?}");
        }
        let luma = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        let bg = luma(p.screen_bg) as i64;
        let contrast = |c| (luma(c) as i64 - bg).abs();
        assert!(contrast(p.faint) < contrast(p.text));
        assert!(p.row_bg(LineKind::DiffAdd).is_some());
        assert!(p.row_bg(LineKind::Assistant).is_none());
        assert!(p.row_bg(LineKind::Input).is_some());
    }
}
