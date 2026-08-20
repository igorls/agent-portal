//! Extract indexable text and embed chunks from a [`CanonicalSession`].
//!
//! Goal for semantic indexing is **session discovery** ("which session talked
//! about X?"), not RAG-quality answers. Prefer user intent + assistant prose;
//! drop agent boilerplate, tool dumps, and binary-looking noise.

use crate::ir::{Block, CanonicalSession, Role};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub max_chars: usize,
    pub max_block_chars: usize,
    pub include_tool_args: bool,
    pub include_tool_results: bool,
    pub include_thinking: bool,
    /// When true, apply aggressive cleaning for embeddings (topic-finding).
    pub clean_for_embed: bool,
    /// Prefer user turns; cap assistant contribution (embed path).
    pub user_first: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            max_chars: 200_000,
            max_block_chars: 4_000,
            include_tool_args: true,
            include_tool_results: false,
            include_thinking: false,
            clean_for_embed: false,
            user_first: false,
        }
    }
}

impl ExtractOptions {
    /// Options tuned for WormDB embeddings: topic signals, less noise.
    pub fn for_semantic() -> Self {
        Self {
            max_chars: 120_000,
            max_block_chars: 2_500,
            include_tool_args: false,
            include_tool_results: false,
            include_thinking: false,
            clean_for_embed: true,
            user_first: true,
        }
    }
}

/// Flatten a session to a single indexable string (FTS body).
pub fn extract_indexable_text(session: &CanonicalSession, opts: &ExtractOptions) -> String {
    let mut out = String::new();
    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        for block in &turn.blocks {
            let piece = match block {
                Block::Text { text } => Some(text.as_str()),
                Block::Thinking { text, encrypted } => {
                    if opts.include_thinking && !*encrypted {
                        Some(text.as_str())
                    } else {
                        None
                    }
                }
                Block::ToolCall {
                    name, arguments, ..
                } if opts.include_tool_args => {
                    let args = crate::ir::tool_args_text(arguments);
                    let s = format!("tool {name}: {args}");
                    let cleaned = if opts.clean_for_embed {
                        clean_for_embed(&s)
                    } else {
                        s
                    };
                    push_capped(&mut out, &cleaned, opts);
                    None
                }
                Block::ToolResult {
                    output, is_error, ..
                } if opts.include_tool_results => {
                    let body = crate::ir::tool_output_text(output);
                    let s = format!(
                        "tool result{}: {body}",
                        if *is_error { " error" } else { "" }
                    );
                    let cleaned = if opts.clean_for_embed {
                        clean_for_embed(&s)
                    } else {
                        s
                    };
                    push_capped(&mut out, &cleaned, opts);
                    None
                }
                Block::Compaction { summary } => Some(summary.as_str()),
                _ => None,
            };
            if let Some(text) = piece {
                let t = if opts.clean_for_embed {
                    clean_for_embed(text)
                } else {
                    text.to_string()
                };
                push_capped(&mut out, &t, opts);
            }
            if out.len() >= opts.max_chars {
                return out;
            }
        }
    }
    out
}

fn push_capped(out: &mut String, text: &str, opts: &ExtractOptions) {
    if out.len() >= opts.max_chars {
        return;
    }
    let room = opts.max_chars - out.len();
    let take = text
        .chars()
        .take(opts.max_block_chars.min(room))
        .collect::<String>();
    if take.trim().is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&take);
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub chunk_ix: u32,
    pub role: Role,
    pub turn_id: String,
    pub text: String,
}

/// Split session into embed chunks optimized for **finding sessions by topic**.
///
/// - Strips system-reminder / tool noise when `clean_for_embed`
/// - Skips empty/boilerplate turns
/// - User turns get their own chunks more often (strong topic signal)
/// - Tool dumps are off by default in [`ExtractOptions::for_semantic`]
pub fn chunk_for_embed(session: &CanonicalSession, opts: &ExtractOptions) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut buf = String::new();
    let mut buf_role = Role::User;
    let mut buf_turn = String::new();
    let mut ix = 0u32;
    // Smaller chunks → more precise topic anchors for session discovery.
    const TARGET: usize = 900;
    const MIN_CHUNK: usize = 24;

    let flush =
        |chunks: &mut Vec<Chunk>, buf: &mut String, role: Role, turn_id: &str, ix: &mut u32| {
            let text = buf.trim().to_string();
            buf.clear();
            if text.chars().count() < MIN_CHUNK {
                return;
            }
            if is_boilerplate_only(&text) {
                return;
            }
            chunks.push(Chunk {
                chunk_ix: *ix,
                role,
                turn_id: turn_id.to_string(),
                text,
            });
            *ix += 1;
        };

    // Title is a strong topic prior for session discovery — first chunk if present.
    if let Some(title) = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| t.len() >= 4)
    {
        let t = if opts.clean_for_embed {
            clean_for_embed(title)
        } else {
            title.to_string()
        };
        if t.chars().count() >= 4 && !is_boilerplate_only(&t) {
            chunks.push(Chunk {
                chunk_ix: ix,
                role: Role::User,
                turn_id: "title".into(),
                text: format!("Session: {t}"),
            });
            ix += 1;
        }
    }

    for turn in &session.timeline {
        if turn.is_meta {
            continue;
        }
        let mut turn_text = String::new();
        for block in &turn.blocks {
            match block {
                Block::Text { text } => {
                    if !turn_text.is_empty() {
                        turn_text.push('\n');
                    }
                    turn_text.push_str(text);
                }
                Block::Compaction { summary } => {
                    if !turn_text.is_empty() {
                        turn_text.push('\n');
                    }
                    turn_text.push_str(summary);
                }
                Block::ToolCall {
                    name, arguments, ..
                } if opts.include_tool_args => {
                    if !turn_text.is_empty() {
                        turn_text.push('\n');
                    }
                    // Only keep tool *name* as a weak signal (not full JSON dumps).
                    turn_text.push_str(&format!("tool {name}"));
                    let args = crate::ir::tool_args_text(arguments);
                    if args.len() < 200 {
                        turn_text.push_str(": ");
                        turn_text.push_str(&args);
                    }
                }
                _ => {}
            }
        }

        let cleaned = if opts.clean_for_embed {
            clean_for_embed(&turn_text)
        } else {
            turn_text
        };
        if cleaned.trim().is_empty() || is_boilerplate_only(&cleaned) {
            continue;
        }

        // Cap assistant turns more aggressively — user queries carry the topic.
        let max_turn = if opts.user_first && turn.role == Role::Assistant {
            opts.max_block_chars / 2
        } else {
            opts.max_block_chars
        };
        let take: String = cleaned.chars().take(max_turn).collect();
        if take.trim().is_empty() {
            continue;
        }

        // User turns: flush previous buffer and emit a dedicated chunk when large enough.
        if opts.user_first && turn.role == Role::User {
            if !buf.is_empty() {
                flush(&mut chunks, &mut buf, buf_role, &buf_turn, &mut ix);
            }
            buf_role = Role::User;
            buf_turn = turn.id.clone();
            buf.push_str(&take);
            // Short user queries still matter — flush immediately if short.
            if buf.chars().count() >= MIN_CHUNK {
                flush(&mut chunks, &mut buf, buf_role, &buf_turn, &mut ix);
            }
            continue;
        }

        if !buf.is_empty() && (buf.len() + take.len() > TARGET || turn.role != buf_role) {
            flush(&mut chunks, &mut buf, buf_role, &buf_turn, &mut ix);
        }
        if buf.is_empty() {
            buf_role = turn.role;
            buf_turn = turn.id.clone();
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&take);
    }
    flush(&mut chunks, &mut buf, buf_role, &buf_turn, &mut ix);
    chunks
}

/// Strip agent/UI chrome that pollutes embeddings and snippets.
///
/// Keeps natural language; drops system tags, huge base64, and dense JSON tool noise.
pub fn clean_for_embed(input: &str) -> String {
    let mut s = input.to_string();

    // Common Grok/Claude/Codex chrome.
    for tag in [
        "system-reminder",
        "system",
        "user_query",
        "assistant",
        "tool_call",
        "tool_result",
        "function_calls",
        "invoke",
    ] {
        // Remove <tag>...</tag> and <tag/> loosely.
        s = strip_xml_like_blocks(&s, tag);
    }

    // Lines that are pure scaffolding.
    let mut out_lines = Vec::new();
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("you are ") && lower.contains("assistant") {
            continue;
        }
        if lower.starts_with("as you answer") || lower.starts_with("as an ai") {
            continue;
        }
        if lower.contains("this session is being continued from a previous conversation") {
            // Keep a short marker — long compaction summaries still get truncated later.
            if t.len() > 180 {
                out_lines.push(t.chars().take(160).collect::<String>() + "…");
                continue;
            }
        }
        // Skip base64-ish / hex dumps.
        if looks_like_binary_blob(t) {
            continue;
        }
        // Skip huge single-line JSON (tool args / IPC dumps).
        if t.len() > 400 && (t.starts_with('{') || t.starts_with('[')) {
            out_lines.push(summarize_json_line(t));
            continue;
        }
        // Skip paths-only noise lines with no words.
        if t.len() > 80 && word_count(t) < 3 && (t.contains('\\') || t.contains('/')) {
            continue;
        }
        out_lines.push(t.to_string());
    }

    let mut out = out_lines.join("\n");
    // Collapse runs of whitespace.
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

/// Clean a snippet for UI/MCP display (same rules, shorter).
pub fn clean_snippet(input: &str, max_chars: usize) -> String {
    let c = clean_for_embed(input);
    let t = c.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    t.chars().take(max_chars).collect::<String>() + "…"
}

fn strip_xml_like_blocks(s: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(s.len());
    let lower = s.to_ascii_lowercase();
    let open_l = open.to_ascii_lowercase();
    let close_l = close.to_ascii_lowercase();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if let Some(rel) = lower[i..].find(&open_l) {
            let start = i + rel;
            out.push_str(&s[i..start]);
            // Find end of open tag >
            let after_open = start + open.len();
            let rest = &s[after_open..];
            let gt = rest
                .find('>')
                .map(|p| after_open + p + 1)
                .unwrap_or(s.len());
            // Self-closing?
            if s.get(..gt).map(|x| x.ends_with("/>")).unwrap_or(false) {
                i = gt;
                continue;
            }
            if let Some(rel_c) = lower[gt..].find(&close_l) {
                i = gt + rel_c + close.len();
            } else {
                // Unclosed — drop rest of open tag only
                i = gt;
            }
        } else {
            out.push_str(&s[i..]);
            break;
        }
    }
    // Also strip bare open tags left over.
    let _ = bytes;
    out
}

fn looks_like_binary_blob(t: &str) -> bool {
    if t.len() < 80 {
        return false;
    }
    let alnum = t
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=' || *c == '-')
        .count();
    let ratio = alnum as f32 / t.len() as f32;
    ratio > 0.92 && word_count(t) < 4
}

fn summarize_json_line(t: &str) -> String {
    // Keep a few keys if present for topic signal.
    let keys: Vec<&str> = [
        "\"name\"",
        "\"path\"",
        "\"query\"",
        "\"error\"",
        "\"model\"",
    ]
    .iter()
    .filter(|k| t.contains(**k))
    .copied()
    .collect();
    if keys.is_empty() {
        format!("[json {} chars]", t.len())
    } else {
        format!("[json keys: {}]", keys.join(", ").replace('"', ""))
    }
}

fn word_count(t: &str) -> usize {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .count()
}

fn is_boilerplate_only(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    let words = word_count(&lower);
    if words < 3 {
        return true;
    }
    // Pure "tool foo" with no other content.
    if lower.starts_with("tool ") && words <= 4 {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        Attachment, Fidelity, LossNote, SessionIdentity, Turn, UsageTotals, Workspace,
    };
    use chrono::Utc;

    fn sess(texts: &[&str]) -> CanonicalSession {
        let timeline = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Turn {
                id: format!("L{i}"),
                parent_id: None,
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                timestamp: None,
                model: None,
                is_meta: false,
                blocks: vec![Block::Text { text: (*t).into() }],
                usage: None,
                raw: None,
            })
            .collect();
        CanonicalSession {
            ir_version: 1,
            identity: SessionIdentity {
                portal_id: "p".into(),
                native_id: "n".into(),
                agent_id: "a".into(),
                store_path: "/t".into(),
                agent_version: None,
                read_at: Utc::now(),
            },
            workspace: Workspace {
                cwd: "/w".into(),
                cwd_normalized: "/w".into(),
                git_branch: None,
                project_label: "w".into(),
            },
            title: Some("WormDB sidecar shipping".into()),
            timeline,
            attachments: Vec::<Attachment>::new(),
            usage: UsageTotals::default(),
            losses: Vec::<LossNote>::new(),
            fidelity: Fidelity::Partial,
        }
    }

    #[test]
    fn extract_includes_user_text() {
        let s = sess(&["hello world about search", "assistant reply on indexing"]);
        let text = extract_indexable_text(&s, &ExtractOptions::default());
        assert!(text.contains("hello world"));
        assert!(text.contains("assistant reply"));
    }

    #[test]
    fn clean_strips_system_reminder() {
        let raw = "<system-reminder>\nAs you answer use tools.\n</system-reminder>\nlets ship wormdb binary locally";
        let c = clean_for_embed(raw);
        assert!(!c.to_ascii_lowercase().contains("system-reminder"));
        assert!(c.contains("wormdb binary"));
    }

    #[test]
    fn semantic_chunks_include_title_and_user() {
        let s = sess(&[
            "how do we bundle wormdb with the installer?",
            "We should use externalBin and vendor the exe.",
        ]);
        let chunks = chunk_for_embed(&s, &ExtractOptions::for_semantic());
        assert!(chunks.iter().any(|c| c.text.contains("Session:")));
        assert!(chunks
            .iter()
            .any(|c| c.text.contains("bundle wormdb") || c.text.contains("installer")));
        // No tool dumps by default
        assert!(chunks.iter().all(|c| !c.text.contains("tool Read:")));
    }

    #[test]
    fn chunks_are_numbered() {
        let s = sess(&[
            "aaa topic one here",
            "bbb topic two here",
            "ccc topic three",
        ]);
        let chunks = chunk_for_embed(&s, &ExtractOptions::default());
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].chunk_ix, 0);
    }
}
