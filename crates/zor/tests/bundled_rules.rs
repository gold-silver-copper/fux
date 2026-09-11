#![cfg(feature = "cli")]

use std::borrow::Cow;
use zor::rules::{
    RuleState, evaluate, load,
    view::{Captured, Progress, ScreenView},
};

/// Builds the `format:"cells"` capture fux would serve for a plain text fixture at this size:
/// one text cell per character, rows wrapped at `columns` and the window ending at the last
/// non-blank row, as the observer sees it.
#[allow(clippy::expect_used)]
fn captured(text: &str, rows: u16, columns: u16) -> Captured {
    let width = usize::from(columns);
    let mut wrapped: Vec<Vec<char>> = Vec::new();
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            wrapped.push(Vec::new());
        }
        for chunk in chars.chunks(width) {
            wrapped.push(chunk.to_vec());
        }
    }
    while wrapped.last().is_some_and(Vec::is_empty) {
        wrapped.pop();
    }
    let start = wrapped.len().saturating_sub(usize::from(rows));
    let mut lines: Vec<serde_json::Value> = wrapped
        .get(start..)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(row, chars)| {
            let mut cells: Vec<serde_json::Value> = chars
                .iter()
                .map(|c| serde_json::json!({"text": c.to_string()}))
                .collect();
            if chars.len() < width {
                cells.push(serde_json::json!({"run": width - chars.len()}));
            }
            serde_json::json!({"row": row, "wrapped": false, "cells": cells})
        })
        .collect();
    while lines.len() < usize::from(rows) {
        lines.push(
            serde_json::json!({"row": lines.len(), "wrapped": false, "cells": [{"run": width}]}),
        );
    }
    Captured::from_capture(&serde_json::json!({
        "seq": 1, "input_sequence": 1, "revision": 1, "rows": rows, "columns": columns,
        "cursor": {"row": 0, "column": 0, "hidden": false}, "title": "", "progress": null,
        "unchanged": false, "truncated": false, "lines": lines,
    }))
    .expect("fixture capture is valid")
}

struct Screen(String);
impl ScreenView for Screen {
    fn lines(&self) -> impl Iterator<Item = Cow<'_, str>> {
        self.0.lines().map(Cow::Borrowed)
    }
    fn text(&self) -> &str {
        &self.0
    }
    fn title(&self) -> &str {
        ""
    }
    fn progress(&self) -> Option<Progress> {
        None
    }
    fn size(&self) -> (u16, u16) {
        (23, 80)
    }
}

#[test]
fn real_codex_startup_and_derived_negative_screens() -> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("codex.toml"),
        include_str!("../rules/codex.toml"),
    )?;
    let evidence: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/agents/codex-0.153.4/startup.json"))?;
    let captures = evidence
        .get("captures")
        .and_then(|value| value.as_array())
        .ok_or("captures")?;
    let mut positives = 0;
    for entry in captures {
        let text = entry
            .get("capture")
            .and_then(|value| value.get("text"))
            .and_then(|value| value.as_str())
            .ok_or("text")?;
        // Feed visible rows through the production terminal model, as the observer does.
        // CRLF places each plain captured row at column zero; this is not an OSC transcript.
        let screen = captured(text, 23, 80);
        let text = screen.text();
        let verdict = evaluate(&set, &screen);
        if text.is_empty() {
            assert!(verdict.rule.is_none());
            continue;
        }
        positives += 1;
        assert_eq!(verdict.state, RuleState::Blocked);
        assert_eq!(verdict.rule.as_deref(), Some("codex-0.153.4-sign-in"));
        assert!(verdict.visible.blocker);
        // Derived adversarial fixtures, not additional real-agent captures.
        for negative in [
            format!("$ cat documentation\n{text}\n$ "),
            text.lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            format!("Quoted earlier startup:\n{text}\nCurrent response: done"),
            format!("{text}\n\x1b]7877;malformed\x07"),
            text.replace("Press enter to continue", "Working..."),
            "codex\nSign in with ChatGPT\nPress enter to continue".to_owned(),
        ] {
            let verdict = evaluate(&set, &Screen(negative));
            assert!(verdict.rule.is_none());
            assert_eq!(verdict.state, RuleState::Unknown);
        }
    }
    assert!(positives > 0);
    Ok(())
}

#[test]
fn real_claude_startup_keeps_partial_and_derived_negative_screens_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("claude.toml"),
        include_str!("../rules/claude.toml"),
    )?;
    let evidence: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/agents/claude-2.1.263/startup.json"))?;
    let captures = evidence
        .get("captures")
        .and_then(serde_json::Value::as_array)
        .ok_or("captures")?;
    assert_eq!(captures.len(), 3);
    for (entry, expected) in
        captures
            .iter()
            .zip([RuleState::Unknown, RuleState::Unknown, RuleState::Blocked])
    {
        let text = entry
            .pointer("/capture/text")
            .and_then(serde_json::Value::as_str)
            .ok_or("text")?;
        // Feed visible rows through the production terminal model, as the observer does.
        // CRLF places each plain captured row at column zero; this is not an OSC transcript.
        let screen = captured(text, 23, 80);
        let text = screen.text();
        let verdict = evaluate(&set, &screen);
        assert_eq!(verdict.state, expected, "normalized screen: {text:?}");
        if expected == RuleState::Unknown {
            assert!(verdict.rule.is_none());
            continue;
        }
        assert_eq!(
            verdict.rule.as_deref(),
            Some("claude-2.1.263-theme-selection")
        );
        assert!(verdict.visible.blocker);
        // Synthetic mutations of real output test specificity; they add no real-state coverage.
        for negative in [
            format!("$ cat documentation\n{text}\n$ "),
            text.lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            format!("Earlier startup:\n{text}\nCurrent response: done"),
            format!("{text}\n\x1b]7877;malformed\x07"),
            text.replace("Choose the text style", "Working on the text style"),
            text.replace(
                "\n   5. Light mode",
                "\nextra inserted line\n   5. Light mode",
            ),
            text.replace(" ❯ 2. Dark mode ✔", "   2. Dark mode"),
            "claude\nChoose the text style\nDark mode".to_owned(),
        ] {
            let verdict = evaluate(&set, &Screen(negative));
            assert!(verdict.rule.is_none());
            assert_eq!(verdict.state, RuleState::Unknown);
        }
    }
    Ok(())
}

#[test]
fn real_opencode_startup_input_keeps_blank_and_changed_screens_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("opencode.toml"),
        include_str!("../rules/opencode.toml"),
    )?;
    let mut positives = 0;
    for fixture in [
        include_str!("fixtures/agents/opencode-1.18.29/startup.json"),
        include_str!("fixtures/agents/opencode-1.18.29/startup-long.json"),
        include_str!("fixtures/agents/opencode-1.18.29/startup-variant.json"),
    ] {
        let evidence: serde_json::Value = serde_json::from_str(fixture)?;
        for entry in evidence
            .get("captures")
            .and_then(serde_json::Value::as_array)
            .ok_or("captures")?
        {
            let text = entry
                .pointer("/capture/text")
                .and_then(serde_json::Value::as_str)
                .ok_or("text")?;
            // As in the observer, normalize the actual captured rows through the terminal model.
            let screen = captured(text, 23, 80);
            let text = screen.text();
            let verdict = evaluate(&set, &screen);
            if text.is_empty() {
                assert_eq!(verdict.state, RuleState::Unknown);
                assert!(verdict.rule.is_none());
                continue;
            }
            positives += 1;
            assert_eq!(verdict.state, RuleState::Idle, "{text:?}");
            assert_eq!(
                verdict.rule.as_deref(),
                Some("opencode-1.18.29-startup-input")
            );
            assert!(verdict.visible.idle);
            // Mutations test specificity, not real authenticated states or OSC parsing.
            for negative in [
                format!("$ cat old-screen\n{text}"),
                format!("{text}Current response: done\n"),
                text.lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                text.replace("Build · Big Pickle", "Build · Different Model"),
                text.replace("Ask anything…", "Working on"),
                text.replace("1.18.29", "1.18.30"),
                text.replace("● Tip Run /connect", "● Tip Run /other"),
                text.replace("tab agents", "esc interrupt"),
                text.replace("█▀▀▄", "aaaa"),
                text.replace("   ┃\n", "   ┃\nextra line\n"),
                format!("{text}\x1b]7877;malformed\x07"),
                "opencode\nAsk anything…\nBuild · Big Pickle OpenCode Zen".to_owned(),
            ] {
                let verdict = evaluate(&set, &Screen(negative));
                assert!(verdict.rule.is_none());
                assert_eq!(verdict.state, RuleState::Unknown);
            }
        }
    }
    assert_eq!(positives, 3);
    Ok(())
}

#[test]
fn real_resized_startup_screens_remain_explicitly_unknown() -> Result<(), Box<dyn std::error::Error>>
{
    for (name, rules, fixture) in [
        (
            "claude",
            include_str!("../rules/claude.toml"),
            include_str!("fixtures/agents/claude-2.1.263/lifecycle.json"),
        ),
        (
            "codex",
            include_str!("../rules/codex.toml"),
            include_str!("fixtures/agents/codex-0.153.4/lifecycle.json"),
        ),
        (
            "opencode",
            include_str!("../rules/opencode.toml"),
            include_str!("fixtures/agents/opencode-1.18.29/lifecycle.json"),
        ),
    ] {
        let set = load(std::path::Path::new(name), rules)?;
        let evidence: serde_json::Value = serde_json::from_str(fixture)?;
        let captures = evidence
            .pointer("/stages/1/captures")
            .and_then(serde_json::Value::as_array)
            .ok_or("narrow captures")?;
        assert!(!captures.is_empty());
        for capture in captures {
            let rows = u16::try_from(
                capture
                    .get("rows")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("rows")?,
            )?;
            let columns = u16::try_from(
                capture
                    .get("columns")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("columns")?,
            )?;
            let text = capture
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or("text")?;
            let screen = captured(text, rows, columns);
            assert_eq!(
                evaluate(&set, &screen).state,
                RuleState::Unknown,
                "{name} narrowed screen"
            );
        }
    }
    Ok(())
}

#[test]
fn real_codex_authenticated_trust_working_and_response() -> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("codex.toml"),
        include_str!("../rules/codex.toml"),
    )?;
    let evidence: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/agents/codex-0.153.4/authenticated.json"
    ))?;
    let mut blocked = 0;
    let mut working = 0;
    let mut response = 0;
    for entry in evidence
        .get("captures")
        .and_then(serde_json::Value::as_array)
        .ok_or("captures")?
    {
        let capture = entry.get("capture").ok_or("capture")?;
        let text = capture
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or("text")?;
        let screen = captured(text, 23, 80);
        let text = screen.text();
        let expected = if text.contains("Do you trust the contents of this directory?") {
            blocked += 1;
            RuleState::Blocked
        } else if text
            .lines()
            .any(|line| line.starts_with("• Working (") || line.starts_with("◦ Working ("))
        {
            working += 1;
            RuleState::Working
        } else if text.lines().any(|line| line == "• FUX_OBSERVATION_OK") {
            response += 1;
            RuleState::Idle
        } else {
            RuleState::Unknown
        };
        assert_eq!(evaluate(&set, &screen).state, expected, "{text:?}");
        if expected == RuleState::Idle {
            let verdict = evaluate(&set, &screen);
            assert!(verdict.visible.idle && !verdict.visible.working && !verdict.visible.blocker);
            assert_eq!(verdict.rule.as_deref(), Some("codex-0.153.4-input-ready"));
        }
        // Synthetic adversarial changes do not add real-agent state coverage.
        if expected != RuleState::Unknown {
            for negative in [
                format!("$ cat old-screen\n{text}"),
                format!("{text}Current response: done\n"),
                text.lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                format!("{text}\x1b]7877;malformed\x07"),
                text.replace("Press enter to continue", "Working now")
                    .replace("esc to interrupt", "interrupted")
                    .replace("Ask Codex to do anything", "Unrecognized composer"),
            ] {
                assert_eq!(evaluate(&set, &Screen(negative)).state, RuleState::Unknown);
            }
        }
        if text.contains("esc to interrupt") {
            // Even a changed interrupt hint cannot make active Working/MCP boot Idle.
            assert_eq!(
                evaluate(
                    &set,
                    &Screen(text.replace("esc to interrupt", "interrupted"))
                )
                .state,
                RuleState::Unknown
            );
        }
    }
    assert!(blocked > 0 && working > 0 && response > 0);
    Ok(())
}

#[test]
fn real_codex_unanswered_command_approval_is_blocked() -> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("codex.toml"),
        include_str!("../rules/codex.toml"),
    )?;
    let evidence: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/agents/codex-0.153.4/approval.json"))?;
    let capture = evidence
        .get("captures")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.last())
        .and_then(|entry| entry.get("capture"))
        .ok_or("capture")?;
    let text = capture
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or("text")?;
    let screen = captured(text, 23, 80);
    let text = screen.text();
    let verdict = evaluate(&set, &screen);
    assert_eq!(verdict.state, RuleState::Blocked);
    assert!(verdict.visible.blocker && !verdict.visible.working);
    assert_eq!(
        verdict.rule.as_deref(),
        Some("codex-0.153.4-command-approval")
    );
    // These are derived negative strings. An exact copied viewport is indistinguishable.
    for negative in [
        format!("$ cat old-screen\n{text}"),
        format!("{text}Current response: done\n"),
        text.lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("{text}\x1b]7877;malformed\x07"),
        text.replace(
            "Press enter to confirm or esc to cancel",
            "Command finished",
        ),
        text.replace("› 1. Yes, proceed (y)", "  1. Yes, proceed (y)"),
        text.replace("  Environment: local", "  Environment: remote"),
    ] {
        assert_eq!(evaluate(&set, &Screen(negative)).state, RuleState::Unknown);
    }
    Ok(())
}

#[test]
fn real_opencode_permission_and_question_are_passive_blockers()
-> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("opencode.toml"),
        include_str!("../rules/opencode.toml"),
    )?;
    for (source, rule, marker) in [
        (
            include_str!("fixtures/agents/opencode-1.18.29/integration.json"),
            "opencode-1.18.29-permission",
            "Allow once   Allow always   Reject",
        ),
        (
            include_str!("fixtures/agents/opencode-1.18.29/integration-storage.json"),
            "opencode-1.18.29-question",
            "select   submit      dismiss",
        ),
    ] {
        let value: serde_json::Value = serde_json::from_str(source)?;
        let text = value
            .pointer("/capture/text")
            .and_then(serde_json::Value::as_str)
            .ok_or("text")?;
        let screen = captured(text, 23, 40);
        let text = screen.text();
        let verdict = evaluate(&set, &screen);
        assert_eq!(verdict.state, RuleState::Blocked);
        assert_eq!(verdict.rule.as_deref(), Some(rule));
        assert!(verdict.visible.blocker && !verdict.visible.idle);
        for negative in [
            format!("$ cat old-screen\n{text}"),
            format!("{text}Tool finished\n"),
            text.lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            text.replace(marker, "Tool already returned"),
            format!("{text}\x1b]7877;malformed\x07"),
        ] {
            assert_eq!(evaluate(&set, &Screen(negative)).state, RuleState::Unknown);
        }
    }
    Ok(())
}

#[test]
fn real_claude_working_footer_does_not_turn_response_into_completion()
-> Result<(), Box<dyn std::error::Error>> {
    let set = load(
        std::path::Path::new("claude.toml"),
        include_str!("../rules/claude.toml"),
    )?;
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/agents/claude-2.1.263/authenticated.json"
    ))?;
    let captures = value
        .get("captures")
        .and_then(serde_json::Value::as_array)
        .ok_or("captures")?;
    let mut working = 0;
    let mut response = 0;
    let mut idle = 0;
    for entry in captures {
        let text = entry
            .pointer("/capture/text")
            .and_then(serde_json::Value::as_str)
            .ok_or("text")?;
        if !text.contains("Sonnet 5 · API Usage Billing") {
            continue;
        }
        let screen = captured(text, 23, 80);
        let text = screen.text();
        let expected = if text.contains("esc to interrupt") {
            working += 1;
            RuleState::Working
        } else {
            if text.contains("⏺ FUX_CLAUDE_OBSERVATION_OK") {
                response += 1;
            }
            idle += 1;
            RuleState::Idle
        };
        assert_eq!(evaluate(&set, &screen).state, expected, "{text:?}");
        if expected == RuleState::Idle {
            let verdict = evaluate(&set, &screen);
            assert!(verdict.visible.idle && !verdict.visible.working && !verdict.visible.blocker);
            assert_eq!(verdict.rule.as_deref(), Some("claude-2.1.263-input-ready"));
        }
        if expected == RuleState::Working || expected == RuleState::Idle {
            for negative in [
                format!("$ cat saved-screen\n{text}"),
                format!("{text}Tool finished\n"),
                text.lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                text.replace("manual mode on", "unrecognized mode"),
                format!("{text}\x1b]7877;malformed\x07"),
            ] {
                assert_eq!(evaluate(&set, &Screen(negative)).state, RuleState::Unknown);
            }
        }
        if expected == RuleState::Working {
            // A changed footer alone cannot turn an active spinner into input readiness.
            let changed = text.replace("esc to interrupt", "? for shortcuts");
            assert_eq!(evaluate(&set, &Screen(changed)).state, RuleState::Unknown);
        }
    }
    assert!(working > 0 && response > 0 && idle > 1);
    Ok(())
}
