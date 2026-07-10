//! Deterministic handoff-brief generator.
//!
//! When a session can't (or shouldn't) be natively converted, the portal
//! instead writes a structured markdown brief and launches a fresh target
//! session pointed at it. This module builds that brief purely from IR facts —
//! no model, fully reproducible. An optional local-LLM pass (see
//! [`crate::migration::ollama`]) may rewrite the prose afterward, but this text
//! is always the floor and the fallback.

use std::collections::BTreeMap;

use crate::ir::{Block, CanonicalSession, Role};

const RECENT_TURNS: usize = 8;
const GOAL_MAX_CHARS: usize = 1200;

/// Facts extracted from a session, shared by the deterministic renderer and the
/// LLM enrichment prompt so both work from the same ground truth.
pub struct BriefFacts {
    pub title: String,
    pub source_agent: String,
    pub short_id: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub goal: Option<String>,
    pub recent: Vec<(Role, String)>,
    pub files_touched: Vec<String>,
    pub tool_counts: Vec<(String, usize)>,
    pub recent_commands: Vec<String>,
    pub next_steps: Option<String>,
}

pub fn extract_facts(session: &CanonicalSession) -> BriefFacts {
    let short_id = session
        .identity
        .native_id
        .chars()
        .take(8)
        .collect::<String>();

    let goal = first_user_text(session).map(|t| truncate(&strip_tags(&t), GOAL_MAX_CHARS));

    // Recent state: the last few non-meta turns, text only, tools collapsed.
    let mut recent: Vec<(Role, String)> = Vec::new();
    for turn in session.timeline.iter().rev() {
        if turn.is_meta {
            continue;
        }
        let text = turn_text(turn);
        if let Some(text) = text {
            recent.push((turn.role, truncate(&text, 400)));
            if recent.len() >= RECENT_TURNS {
                break;
            }
        }
    }
    recent.reverse();

    // Files touched + per-tool counts + recent shell commands.
    let mut files: Vec<String> = Vec::new();
    let mut seen_files = std::collections::HashSet::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut commands: Vec<String> = Vec::new();
    for turn in &session.timeline {
        for block in &turn.blocks {
            if let Block::ToolCall {
                name, arguments, ..
            } = block
            {
                *counts.entry(name.clone()).or_default() += 1;
                for path in paths_from_tool(name, arguments) {
                    if seen_files.insert(path.clone()) {
                        files.push(path);
                    }
                }
                if let Some(cmd) = shell_command(name, arguments) {
                    commands.push(truncate(&cmd, 160));
                }
            }
        }
    }
    // Keep the most recent handful of commands.
    let recent_commands = commands.iter().rev().take(5).rev().cloned().collect();

    let mut tool_counts: Vec<(String, usize)> = counts.into_iter().collect();
    tool_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let next_steps = last_assistant_text(session).map(|t| truncate(&t, 600));

    BriefFacts {
        title: session
            .title
            .clone()
            .unwrap_or_else(|| format!("session {short_id}")),
        source_agent: session.identity.agent_id.clone(),
        short_id,
        cwd: session.workspace.cwd.clone(),
        git_branch: session.workspace.git_branch.clone(),
        goal,
        recent,
        files_touched: files,
        tool_counts,
        recent_commands,
        next_steps,
    }
}

/// The always-available deterministic brief. Stable output for stable input.
pub fn render(facts: &BriefFacts) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Handoff: {}\n\n", facts.title));
    out.push_str(&format!(
        "Continued from **{}** session `{}` · project `{}`",
        facts.source_agent, facts.short_id, facts.cwd
    ));
    if let Some(branch) = &facts.git_branch {
        out.push_str(&format!(" · branch `{branch}`"));
    }
    out.push_str("\n\n");

    out.push_str("## Goal\n\n");
    match &facts.goal {
        Some(goal) => out.push_str(&format!("{goal}\n\n")),
        None => out.push_str("_No explicit opening request found._\n\n"),
    }

    if !facts.recent.is_empty() {
        out.push_str("## Where things stand\n\n");
        for (role, text) in &facts.recent {
            out.push_str(&format!("- **{}:** {}\n", role_label(*role), text));
        }
        out.push('\n');
    }

    if !facts.files_touched.is_empty() {
        out.push_str("## Files touched\n\n");
        for path in facts.files_touched.iter().take(30) {
            out.push_str(&format!("- `{path}`\n"));
        }
        out.push('\n');
    }

    if !facts.tool_counts.is_empty() {
        out.push_str("## Tool activity\n\n");
        let summary = facts
            .tool_counts
            .iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("{summary}\n\n"));
        if !facts.recent_commands.is_empty() {
            out.push_str("Recent commands:\n\n");
            for cmd in &facts.recent_commands {
                out.push_str(&format!("- `{cmd}`\n"));
            }
            out.push('\n');
        }
    }

    out.push_str("## Suggested next step\n\n");
    match &facts.next_steps {
        Some(next) => out.push_str(&format!("{next}\n\n")),
        None => out.push_str("_Continue from where the conversation left off._\n\n"),
    }

    out.push_str("---\n\n");
    out.push_str(
        "You are continuing this work. Read the files above before acting, and confirm the current state with the user before making large changes.\n",
    );
    out
}

fn turn_text(turn: &crate::ir::Turn) -> Option<String> {
    let mut parts = Vec::new();
    for block in &turn.blocks {
        if let Block::Text { text } = block {
            let t = strip_tags(text);
            if !t.trim().is_empty() {
                parts.push(t.trim().to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn first_user_text(session: &CanonicalSession) -> Option<String> {
    session
        .timeline
        .iter()
        .filter(|t| t.role == Role::User && !t.is_meta)
        .find_map(|t| turn_text(t).filter(|s| !s.trim().is_empty()))
}

fn last_assistant_text(session: &CanonicalSession) -> Option<String> {
    session
        .timeline
        .iter()
        .rev()
        .filter(|t| t.role == Role::Assistant && !t.is_meta)
        .find_map(turn_text)
        .map(|t| {
            // Trailing paragraph(s) tend to hold the "what's next" summary.
            t.rsplit("\n\n").next().unwrap_or(&t).trim().to_string()
        })
}

/// Recognized path-bearing argument keys across common tools, plus a
/// conservative scan of shell commands for file-looking tokens.
fn paths_from_tool(name: &str, args: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in [
        "file_path",
        "path",
        "filePath",
        "notebook_path",
        "target_file",
    ] {
        if let Some(p) = args.get(key).and_then(|v| v.as_str()) {
            out.push(p.to_string());
        }
    }
    if let Some(cmd) = shell_command(name, args) {
        for token in cmd.split_whitespace() {
            let t = token.trim_matches(|c| c == '"' || c == '\'' || c == '`');
            if looks_like_path(t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

fn shell_command(name: &str, args: &serde_json::Value) -> Option<String> {
    let lname = name.to_ascii_lowercase();
    if lname.contains("bash") || lname.contains("shell") || lname.contains("exec") {
        for key in ["command", "cmd", "script"] {
            if let Some(c) = args.get(key).and_then(|v| v.as_str()) {
                return Some(c.to_string());
            }
        }
        // Codex shell_command sometimes passes an argv array.
        if let Some(arr) = args.get("command").and_then(|v| v.as_array()) {
            let joined = arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.is_empty() {
                return Some(joined);
            }
        }
    }
    None
}

fn looks_like_path(t: &str) -> bool {
    if t.len() < 3 || t.len() > 200 {
        return false;
    }
    // has an extension and a separator, no shell metacharacters
    let has_ext = t
        .rsplit('/')
        .next()
        .map(|f| f.contains('.'))
        .unwrap_or(false)
        || t.rsplit('\\')
            .next()
            .map(|f| f.contains('.'))
            .unwrap_or(false);
    let has_sep = t.contains('/') || t.contains('\\');
    let clean = !t.contains(['|', '&', ';', '$', '(', ')', '*', '<', '>']);
    has_ext && has_sep && clean
}

fn strip_tags(s: &str) -> String {
    // Agents prefix the real message with injected context wrapped in XML-ish
    // blocks (<user_info>…</user_info>, <local-command-caveat>…, <git_status>…).
    // Peel whole balanced blocks off the front, then scrub any stray markers,
    // so the human-readable request survives and the injected content doesn't.
    let mut rest = s.trim();
    loop {
        if !rest.starts_with('<') {
            break;
        }
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[1..close];
        if tag.starts_with('/') {
            break; // a stray closing tag; leave it to the scrubber
        }
        let name = tag.split_whitespace().next().unwrap_or("");
        let end = format!("</{name}>");
        if let Some(pos) = rest.find(&end) {
            rest = rest[pos + end.len()..].trim_start();
        } else {
            rest = rest[close + 1..].trim_start(); // unbalanced: drop the opener
        }
    }

    // Scrub any remaining inline tag markers (keep their text).
    let mut out = String::with_capacity(rest.len());
    let mut in_tag = false;
    for c in rest.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn truncate(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "You",
        Role::Assistant => "Agent",
        Role::Tool => "Tool",
        Role::System => "System",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;

    fn session() -> CanonicalSession {
        let mut user = Turn {
            id: "u1".into(),
            parent_id: None,
            role: Role::User,
            timestamp: None,
            model: None,
            is_meta: false,
            blocks: vec![Block::Text {
                text: "<user_info>ignore me</user_info>Build the auth module and add tests".into(),
            }],
            usage: None,
            raw: None,
        };
        let assistant = Turn {
            id: "a1".into(),
            role: Role::Assistant,
            blocks: vec![
                Block::Text {
                    text: "I'll start.".into(),
                },
                Block::ToolCall {
                    call_id: "c1".into(),
                    name: "Edit".into(),
                    arguments: serde_json::json!({"file_path": "P:/app/auth.ts"}),
                },
            ],
            ..user.clone()
        };
        let tool = Turn {
            id: "t1".into(),
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                call_id: "c1".into(),
                output: serde_json::json!("ok"),
                is_error: false,
            }],
            ..user.clone()
        };
        let last = Turn {
            id: "a2".into(),
            role: Role::Assistant,
            blocks: vec![Block::Text {
                text: "Auth module is scaffolded.\n\nNext, wire it into the router.".into(),
            }],
            ..user.clone()
        };
        user.blocks = user.blocks.clone();
        CanonicalSession {
            ir_version: IR_VERSION,
            identity: SessionIdentity {
                portal_id: "p".into(),
                native_id: "abcdef12-0000".into(),
                agent_id: "codex".into(),
                store_path: "s".into(),
                agent_version: None,
                read_at: chrono::Utc::now(),
            },
            workspace: Workspace {
                cwd: "P:/app".into(),
                cwd_normalized: "p:/app".into(),
                git_branch: Some("main".into()),
                project_label: "app".into(),
            },
            title: Some("Auth module".into()),
            timeline: vec![user, assistant, tool, last],
            attachments: vec![],
            usage: UsageTotals::default(),
            losses: vec![],
            fidelity: Fidelity::BriefOnly,
        }
    }

    #[test]
    fn deterministic_brief_is_stable_and_complete() {
        let facts = extract_facts(&session());
        assert_eq!(
            facts.goal.as_deref(),
            Some("Build the auth module and add tests")
        );
        assert!(facts.files_touched.contains(&"P:/app/auth.ts".to_string()));
        assert_eq!(facts.tool_counts, vec![("Edit".to_string(), 1)]);
        assert!(facts
            .next_steps
            .as_deref()
            .unwrap()
            .contains("wire it into the router"));

        let a = render(&facts);
        let b = render(&extract_facts(&session()));
        assert_eq!(a, b, "render must be deterministic");
        assert!(a.contains("# Handoff: Auth module"));
        assert!(a.contains("## Goal"));
        assert!(a.contains("`P:/app/auth.ts`"));
        assert!(!a.contains("<user_info>"), "injected tags must be stripped");
    }
}
