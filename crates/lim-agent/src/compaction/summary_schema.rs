//! Structured summary schema for compaction (Phase 1).
//!
//! [`StructuredSummary`] is the target output of the summarization LLM call
//! and the payload carried in [`CompactionResult`](crate::context_window::CompactionResult).
//! It mirrors Pi's 9-field structured summary plus file-level hints that are
//! extracted locally (not by the LLM).
//!
//! ## Design
//!
//! - All fields optional / default-empty so partial LLM output is tolerated.
//! - `read_files` / `modified_files` are filled by [`extract_file_hints`](super::file_hints),
//!   NOT by the LLM — file paths are structured data and can be extracted
//!   deterministically from tool-call arguments.
//! - `render_for_projection` produces a fixed-format text block so the prompt
//!   cache prefix stays stable across compactions (only the body varies).

use serde::{Deserialize, Serialize};

/// Structured summary produced by the summarization LLM call.
///
/// Fields 1-9 are LLM-filled (Pi reference). Fields 10-12 are runtime-filled.
/// All fields `#[serde(default, skip_serializing_if)]` so:
/// - Old `CompactionResult` JSONL (no `summary` field) deserializes as `None`.
/// - Empty fields are omitted from JSONL to keep it compact.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredSummary {
    /// The user's overarching goal for this session (1-2 sentences).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,

    /// Hard constraints: tech stack, conventions, invariants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,

    /// Completed work (past tense, time order).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<String>,

    /// Work currently underway in the most recent turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_progress: Vec<String>,

    /// Items blocked on external input / failing CI / unresolved errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,

    /// Why option A was chosen over B, with rationale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_decisions: Vec<String>,

    /// Concrete next actions for the upcoming turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,

    /// Token counts, config values, error stacks, IDs that must survive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_context: Vec<String>,

    // ── Runtime-filled fields (NOT produced by the LLM) ───────────────
    /// Files read by `read` tool in the compacted region (deduped, first-seen order).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<String>,

    /// Files modified by `write`/`edit` tools in the compacted region.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,

    /// Prefix summary for a split turn (Phase 2; None in Phase 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_prefix_summary: Option<String>,
}

impl StructuredSummary {
    /// Render as a User-role message text for LLM projection.
    ///
    /// Format is FIXED (section order + headers) to maximize prompt cache
    /// stability — only the body content varies between compactions.
    /// Empty sections are omitted from the output to avoid noise.
    pub fn render_for_projection(&self) -> String {
        let mut sections: Vec<String> = Vec::new();

        if let Some(g) = &self.goal {
            sections.push(format!("Goal: {g}"));
        }
        if !self.constraints.is_empty() {
            sections.push(format!("Constraints:\n{}", bullet_list(&self.constraints)));
        }
        if !self.done.is_empty() {
            sections.push(format!("Done:\n{}", bullet_list(&self.done)));
        }
        if !self.in_progress.is_empty() {
            sections.push(format!("In-Progress:\n{}", bullet_list(&self.in_progress)));
        }
        if !self.blocked.is_empty() {
            sections.push(format!("Blocked:\n{}", bullet_list(&self.blocked)));
        }
        if !self.key_decisions.is_empty() {
            sections.push(format!("Key Decisions:\n{}", bullet_list(&self.key_decisions)));
        }
        if !self.next_steps.is_empty() {
            sections.push(format!("Next Steps:\n{}", bullet_list(&self.next_steps)));
        }
        if !self.critical_context.is_empty() {
            sections.push(format!("Critical Context:\n{}", bullet_list(&self.critical_context)));
        }
        if !self.read_files.is_empty() {
            sections.push(format!("Files Read:\n{}", bullet_list(&self.read_files)));
        }
        if !self.modified_files.is_empty() {
            sections.push(format!("Files Modified:\n{}", bullet_list(&self.modified_files)));
        }
        if let Some(tp) = &self.turn_prefix_summary {
            sections.push(format!("Turn Prefix: {tp}"));
        }

        if sections.is_empty() {
            String::new()
        } else {
            format!(
                "[Earlier conversation summary]\n{}\n[End of summary — conversation continues below]",
                sections.join("\n\n")
            )
        }
    }

    /// Parse LLM output as JSON. Three-level fallback:
    /// 1. Direct parse as `StructuredSummary`.
    /// 2. Extract ```json ... ``` code block, then parse.
    /// 3. Extract first `{...}` block via brace matching, then parse.
    ///
    /// Returns `None` if all three fail — caller falls back to free-text summary.
    pub fn parse_from_llm_output(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        // Level 1: direct parse.
        if let Ok(s) = serde_json::from_str::<StructuredSummary>(trimmed) {
            return Some(s);
        }
        // Level 2: ```json ... ``` code block.
        if let Some(json_str) = extract_json_code_block(trimmed)
            && let Ok(s) = serde_json::from_str::<StructuredSummary>(json_str)
        {
            return Some(s);
        }
        // Level 3: first {...} block via brace matching.
        if let Some(json_str) = extract_first_json_object(trimmed)
            && let Ok(s) = serde_json::from_str::<StructuredSummary>(json_str)
        {
            return Some(s);
        }
        None
    }

    /// Returns true if all LLM-filled fields are empty (goal is None, all Vecs empty).
    /// Used to decide whether to fall back to free-text summary.
    pub fn is_llm_output_empty(&self) -> bool {
        self.goal.is_none()
            && self.constraints.is_empty()
            && self.done.is_empty()
            && self.in_progress.is_empty()
            && self.blocked.is_empty()
            && self.key_decisions.is_empty()
            && self.next_steps.is_empty()
            && self.critical_context.is_empty()
    }
}

fn bullet_list(items: &[String]) -> String {
    items.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n")
}

/// Extract the content of a ```json ... ``` fenced code block.
fn extract_json_code_block(s: &str) -> Option<&str> {
    let start_marker = "```json";
    let start = s.find(start_marker)? + start_marker.len();
    let rest = &s[start..];
    let end = rest.find("```")?;
    Some(rest[..end].trim())
}

/// Extract the first balanced `{...}` object from the string via brace matching.
/// Handles nested braces and string literals (basic escape handling).
fn extract_first_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut start_idx = None;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start_idx = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0
                    && let Some(start) = start_idx
                {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_returns_empty() {
        let s = StructuredSummary::default();
        assert_eq!(s.render_for_projection(), "");
    }

    #[test]
    fn render_with_goal_only() {
        let s = StructuredSummary { goal: Some("Fix the bug".into()), ..Default::default() };
        let rendered = s.render_for_projection();
        assert!(rendered.contains("Goal: Fix the bug"));
        assert!(rendered.starts_with("[Earlier conversation summary]"));
        assert!(rendered.contains("[End of summary"));
    }

    #[test]
    fn render_with_all_fields() {
        let s = StructuredSummary {
            goal: Some("Refactor".into()),
            constraints: vec!["MSRV 1.95".into()],
            done: vec!["Added field".into()],
            in_progress: vec!["Writing tests".into()],
            blocked: vec!["Waiting on CI".into()],
            key_decisions: vec!["Chose Option A".into()],
            next_steps: vec!["Run cargo test".into()],
            critical_context: vec!["token count: 12345".into()],
            read_files: vec!["src/main.rs".into()],
            modified_files: vec!["src/lib.rs".into()],
            turn_prefix_summary: None,
        };
        let rendered = s.render_for_projection();
        assert!(rendered.contains("Goal: Refactor"));
        assert!(rendered.contains("Constraints:"));
        assert!(rendered.contains("  - MSRV 1.95"));
        assert!(rendered.contains("Files Read:"));
        assert!(rendered.contains("  - src/main.rs"));
        assert!(rendered.contains("Files Modified:"));
        assert!(rendered.contains("  - src/lib.rs"));
    }

    #[test]
    fn parse_direct_json() {
        let raw = r#"{"goal":"test goal","done":["task1"]}"#;
        let s = StructuredSummary::parse_from_llm_output(raw).expect("must parse");
        assert_eq!(s.goal, Some("test goal".into()));
        assert_eq!(s.done, vec!["task1".to_string()]);
    }

    #[test]
    fn parse_json_code_block() {
        let raw = "Here is the summary:\n```json\n{\"goal\":\"x\"}\n```\nDone.";
        let s = StructuredSummary::parse_from_llm_output(raw).expect("must parse");
        assert_eq!(s.goal, Some("x".into()));
    }

    #[test]
    fn parse_embedded_json_with_prose() {
        let raw = "Sure! Here's the summary:\n{\"goal\":\"y\",\"next_steps\":[\"step1\"]}\nHope it helps!";
        let s = StructuredSummary::parse_from_llm_output(raw).expect("must parse");
        assert_eq!(s.goal, Some("y".into()));
        assert_eq!(s.next_steps, vec!["step1".to_string()]);
    }

    #[test]
    fn parse_nested_json_object() {
        let raw = r#"{"goal":"z","constraints":["a","b"],"done":[]}"#;
        let s = StructuredSummary::parse_from_llm_output(raw).expect("must parse");
        assert_eq!(s.goal, Some("z".into()));
        assert_eq!(s.constraints, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_returns_none_for_pure_prose() {
        let raw = "I'm sorry, I can't produce JSON.";
        assert!(StructuredSummary::parse_from_llm_output(raw).is_none());
    }

    #[test]
    fn parse_handles_braces_in_strings() {
        let raw = r#"{"goal":"function {foo}","done":["x"]}"#;
        let s = StructuredSummary::parse_from_llm_output(raw).expect("must parse");
        assert_eq!(s.goal, Some("function {foo}".into()));
    }

    #[test]
    fn is_llm_output_empty_true_for_default() {
        assert!(StructuredSummary::default().is_llm_output_empty());
    }

    #[test]
    fn is_llm_output_empty_false_when_goal_set() {
        let s = StructuredSummary { goal: Some("x".into()), ..Default::default() };
        assert!(!s.is_llm_output_empty());
    }

    #[test]
    fn is_llm_output_empty_false_when_runtime_fields_set() {
        // read_files is a runtime field, but is_llm_output_empty only checks
        // LLM-filled fields. read_files being non-empty should NOT make
        // is_llm_output_empty return false.
        let s = StructuredSummary { read_files: vec!["a.rs".into()], ..Default::default() };
        assert!(
            s.is_llm_output_empty(),
            "runtime-only fields should not affect is_llm_output_empty"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let s = StructuredSummary {
            goal: Some("test".into()),
            done: vec!["a".into(), "b".into()],
            read_files: vec!["x.rs".into()],
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let parsed: StructuredSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(s, parsed);
    }

    #[test]
    fn serde_omits_empty_fields() {
        let s = StructuredSummary::default();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "{}");
    }
}
