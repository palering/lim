//! File-level hint extraction from tool calls.
//!
//! [`extract_file_hints`] scans the compacted (old) region of a conversation
//! and extracts file paths from `read`/`write`/`edit` tool-call arguments.
//! The result populates [`StructuredSummary::read_files`] / [`modified_files`].
//!
//! ## Why local extraction (not LLM)?
//!
//! File paths are structured data inside `ContentBlock::ToolCall::arguments`
//! (a `serde_json::Value`). Extracting them locally is:
//! - **Deterministic**: no LLM hallucination risk.
//! - **Free**: no extra LLM call cost.
//! - **Accurate**: the path is exactly what the tool received.
//!
//! ## Scope
//!
//! Only built-in `read`/`write`/`edit` tools are scanned. `shell` commands
//! (`sed -i`, `echo >`) are NOT parsed — the path extraction is unreliable
//! and false positives are more harmful than false negatives. MCP/external
//! tools are Phase 2 (may use `ToolCategory` + path-arg declaration).

use std::collections::HashSet;

use lim_core::content::ContentBlock;
use lim_core::message::AgentMessage;

/// File-level hints extracted from tool calls in the compacted region.
#[derive(Debug, Default, Clone)]
pub struct FileHints {
    /// Files read by the `read` tool (deduped, first-seen order).
    pub read_files: Vec<String>,
    /// Files modified by `write`/`edit` tools (deduped, first-seen order).
    pub modified_files: Vec<String>,
}

/// Scan tool calls in `old_messages` and extract file paths.
///
/// Scans only `ContentBlock::ToolCall` blocks (on Assistant messages).
/// Tool-result messages don't carry path info. The same file may appear in
/// both `read_files` and `modified_files` — the semantics differ ("viewed"
/// vs "changed").
pub fn extract_file_hints(old_messages: &[AgentMessage]) -> FileHints {
    let mut read_set = HashSet::new();
    let mut mod_set = HashSet::new();
    let mut hints = FileHints::default();

    for msg in old_messages {
        for block in &msg.content {
            if let ContentBlock::ToolCall { name, arguments, .. } = block {
                match name.as_str() {
                    "read" => {
                        if let Some(p) = arguments.get("path").and_then(|v| v.as_str())
                            && read_set.insert(p.to_string())
                        {
                            hints.read_files.push(p.to_string());
                        }
                    }
                    "write" | "edit" => {
                        if let Some(p) = arguments.get("path").and_then(|v| v.as_str())
                            && mod_set.insert(p.to_string())
                        {
                            hints.modified_files.push(p.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_core::id::ToolCallId;
    use lim_core::message::Role;

    fn tool_call(name: &str, args: serde_json::Value) -> AgentMessage {
        AgentMessage::assistant(vec![ContentBlock::tool_call(ToolCallId::new(), name, args)])
    }

    #[test]
    fn empty_messages_returns_empty_hints() {
        let hints = extract_file_hints(&[]);
        assert!(hints.read_files.is_empty());
        assert!(hints.modified_files.is_empty());
    }

    #[test]
    fn extracts_read_files() {
        let msgs = vec![
            tool_call("read", serde_json::json!({"path": "src/main.rs"})),
            tool_call("read", serde_json::json!({"path": "src/lib.rs"})),
        ];
        let hints = extract_file_hints(&msgs);
        assert_eq!(hints.read_files, vec!["src/main.rs", "src/lib.rs"]);
        assert!(hints.modified_files.is_empty());
    }

    #[test]
    fn extracts_modified_files_from_write_and_edit() {
        let msgs = vec![
            tool_call("write", serde_json::json!({"path": "a.txt", "content": "x"})),
            tool_call(
                "edit",
                serde_json::json!({"path": "b.rs", "old_string": "a", "new_string": "b"}),
            ),
        ];
        let hints = extract_file_hints(&msgs);
        assert_eq!(hints.modified_files, vec!["a.txt", "b.rs"]);
        assert!(hints.read_files.is_empty());
    }

    #[test]
    fn dedupes_repeated_reads() {
        let msgs = vec![
            tool_call("read", serde_json::json!({"path": "x.rs"})),
            tool_call("read", serde_json::json!({"path": "x.rs"})),
            tool_call("read", serde_json::json!({"path": "x.rs"})),
        ];
        let hints = extract_file_hints(&msgs);
        assert_eq!(hints.read_files, vec!["x.rs"]);
    }

    #[test]
    fn same_file_in_both_lists_when_read_and_modified() {
        let msgs = vec![
            tool_call("read", serde_json::json!({"path": "shared.rs"})),
            tool_call(
                "edit",
                serde_json::json!({"path": "shared.rs", "old_string": "a", "new_string": "b"}),
            ),
        ];
        let hints = extract_file_hints(&msgs);
        assert_eq!(hints.read_files, vec!["shared.rs"]);
        assert_eq!(hints.modified_files, vec!["shared.rs"]);
    }

    #[test]
    fn ignores_shell_commands() {
        // shell tool has `command`, not `path` — must not appear in either list.
        let msgs =
            vec![tool_call("shell", serde_json::json!({"command": "sed -i 's/a/b/' file.rs"}))];
        let hints = extract_file_hints(&msgs);
        assert!(hints.read_files.is_empty());
        assert!(hints.modified_files.is_empty());
    }

    #[test]
    fn ignores_unknown_tools() {
        let msgs = vec![tool_call("mcp_custom", serde_json::json!({"path": "x.rs"}))];
        let hints = extract_file_hints(&msgs);
        assert!(hints.read_files.is_empty());
        assert!(hints.modified_files.is_empty());
    }

    #[test]
    fn ignores_missing_path_arg() {
        // Malformed tool call without path.
        let msgs = vec![tool_call("read", serde_json::json!({"not_path": "x.rs"}))];
        let hints = extract_file_hints(&msgs);
        assert!(hints.read_files.is_empty());
    }

    #[test]
    fn preserves_first_seen_order() {
        let msgs = vec![
            tool_call("read", serde_json::json!({"path": "z.rs"})),
            tool_call("read", serde_json::json!({"path": "a.rs"})),
            tool_call("read", serde_json::json!({"path": "z.rs"})), // dup
            tool_call("read", serde_json::json!({"path": "m.rs"})),
        ];
        let hints = extract_file_hints(&msgs);
        assert_eq!(hints.read_files, vec!["z.rs", "a.rs", "m.rs"]);
    }

    #[test]
    fn skips_non_tool_call_blocks() {
        // Mix of text and tool-call blocks on the same assistant message.
        let msg = AgentMessage::assistant(vec![
            ContentBlock::text("Let me read"),
            ContentBlock::tool_call(ToolCallId::new(), "read", serde_json::json!({"path": "x.rs"})),
        ]);
        let hints = extract_file_hints(&[msg]);
        assert_eq!(hints.read_files, vec!["x.rs"]);
    }

    #[test]
    fn ignores_user_and_tool_role_messages() {
        // Only Assistant messages with ToolCall blocks carry path info.
        let msgs = vec![
            AgentMessage::user("please read x.rs"),
            AgentMessage::tool_result(ToolCallId::new(), "contents of x.rs", false),
        ];
        let hints = extract_file_hints(&msgs);
        assert!(hints.read_files.is_empty());
        assert!(hints.modified_files.is_empty());
        let _ = Role::User; // silence unused import warning if Role unused
    }
}
