//! Context editing — structured modifications to the message history.
//!
//! A `ContextEdit` describes what to exclude or replace in the message list
//! before sending to the LLM. Edits operate on the projection layer only;
//! the original `AgentMessage` history is never modified.
//!
//! Three producers share this data structure:
//! - **Loop edit**: automatic cleanup via hooks (transient, per-turn)
//! - **User edit**: manual selection via CLI/TUI (persistent, stored in JSONL)
//! - **Compaction**: token budget management via ContextWindowManager

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;
use crate::message::AgentMessage;

/// A structured edit to apply to a message list.
///
/// Indices refer to positions in the original `&[AgentMessage]` slice.
/// Operations are applied in order: excludes first, then replacements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextEdit {
    /// Message indices to exclude entirely (not sent to LLM).
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub exclude: HashSet<usize>,

    /// Message indices to replace with new content.
    /// The message's role and metadata are preserved; only content blocks change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<(usize, Vec<ContentBlock>)>,
}

impl ContextEdit {
    /// Create an empty edit (no changes).
    pub fn none() -> Self {
        Self::default()
    }

    /// Create an edit that excludes the given message indices.
    pub fn excluding(indices: impl IntoIterator<Item = usize>) -> Self {
        Self { exclude: indices.into_iter().collect(), replace: Vec::new() }
    }

    /// Add an exclusion.
    pub fn exclude(mut self, index: usize) -> Self {
        self.exclude.insert(index);
        self
    }

    /// Add a replacement.
    pub fn replace(mut self, index: usize, content: Vec<ContentBlock>) -> Self {
        self.replace.push((index, content));
        self
    }

    /// Returns true if this edit makes no changes.
    pub fn is_empty(&self) -> bool {
        self.exclude.is_empty() && self.replace.is_empty()
    }

    /// Validate that all indices are within bounds.
    pub fn validate(&self, message_count: usize) -> bool {
        let all_in_bounds = |idx: &usize| *idx < message_count;
        self.exclude.iter().all(all_in_bounds)
            && self.replace.iter().all(|(idx, _)| *idx < message_count)
    }
}

/// Apply a `ContextEdit` to a message list, producing a new list.
///
/// - Excluded messages are dropped entirely.
/// - Replaced messages keep their role/metadata but get new content blocks.
/// - All other messages are cloned as-is.
///
/// The original slice is not modified. Out-of-bounds indices are silently
/// ignored (the caller should validate beforehand if strict checking is needed).
pub fn apply_edit(messages: &[AgentMessage], edit: &ContextEdit) -> Vec<AgentMessage> {
    let replace_map: std::collections::HashMap<usize, &Vec<ContentBlock>> =
        edit.replace.iter().map(|(idx, content)| (*idx, content)).collect();

    messages
        .iter()
        .enumerate()
        .filter(|(i, _)| !edit.exclude.contains(i))
        .map(|(i, msg)| {
            if let Some(new_content) = replace_map.get(&i) {
                AgentMessage {
                    id: msg.id,
                    role: msg.role,
                    content: (*new_content).clone(),
                    tool_call_ids: msg.tool_call_ids.clone(),
                    is_error: msg.is_error,
                    parent_id: msg.parent_id,
                }
            } else {
                msg.clone()
            }
        })
        .collect()
}

/// Compose multiple edits into a single edit.
///
/// Later edits take precedence for overlapping replacements.
/// Exclusions are unioned.
pub fn compose_edits(edits: &[ContextEdit]) -> ContextEdit {
    let mut result = ContextEdit::none();
    for edit in edits {
        result.exclude.extend(&edit.exclude);
        for (idx, content) in &edit.replace {
            // Remove any prior replacement for this index
            result.replace.retain(|(i, _)| i != idx);
            result.replace.push((*idx, content.clone()));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    fn make_messages(count: usize) -> Vec<AgentMessage> {
        (0..count)
            .map(|i| {
                if i % 2 == 0 {
                    AgentMessage::user(format!("message {i}"))
                } else {
                    AgentMessage::assistant(vec![ContentBlock::text(format!("reply {i}"))])
                }
            })
            .collect()
    }

    #[test]
    fn empty_edit_preserves_messages() {
        let messages = make_messages(5);
        let edit = ContextEdit::none();
        let result = apply_edit(&messages, &edit);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].role, Role::User);
        assert_eq!(result[1].role, Role::Assistant);
    }

    #[test]
    fn exclude_drops_messages() {
        let messages = make_messages(5);
        let edit = ContextEdit::excluding([1, 3]);
        let result = apply_edit(&messages, &edit);
        assert_eq!(result.len(), 3);
        // Messages 0, 2, 4 remain
        assert_eq!(result[0].role, Role::User); // was index 0
        assert_eq!(result[1].role, Role::User); // was index 2
        assert_eq!(result[2].role, Role::User); // was index 4
    }

    #[test]
    fn replace_changes_content() {
        let messages = make_messages(3);
        let edit = ContextEdit::none().replace(1, vec![ContentBlock::text("[summarized]")]);
        let result = apply_edit(&messages, &edit);
        assert_eq!(result.len(), 3);
        // Message 1 should have replaced content
        match &result[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "[summarized]"),
            _ => panic!("expected text content"),
        }
        // Role is preserved
        assert_eq!(result[1].role, Role::Assistant);
    }

    #[test]
    fn exclude_and_replace_combined() {
        let messages = make_messages(5);
        let edit = ContextEdit::excluding([0, 4]).replace(2, vec![ContentBlock::text("[edited]")]);
        let result = apply_edit(&messages, &edit);
        assert_eq!(result.len(), 3); // 5 - 2 excluded
        // Index 2 was replaced
        match &result[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "[edited]"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn out_of_bounds_indices_ignored() {
        let messages = make_messages(3);
        let edit = ContextEdit::excluding([10, 20]).replace(99, vec![ContentBlock::text("nope")]);
        let result = apply_edit(&messages, &edit);
        assert_eq!(result.len(), 3); // nothing changed
    }

    #[test]
    fn validate_checks_bounds() {
        let edit = ContextEdit::excluding([0, 5]);
        assert!(edit.validate(6));
        assert!(!edit.validate(5)); // index 5 is out of bounds for 5 messages

        let edit2 = ContextEdit::none().replace(3, vec![]);
        assert!(edit2.validate(4));
        assert!(!edit2.validate(3));
    }

    #[test]
    fn is_empty_works() {
        assert!(ContextEdit::none().is_empty());
        assert!(!ContextEdit::excluding([0]).is_empty());
        assert!(!ContextEdit::none().replace(0, vec![]).is_empty());
    }

    #[test]
    fn compose_edits_unions_exclusions() {
        let e1 = ContextEdit::excluding([0, 1]);
        let e2 = ContextEdit::excluding([2, 3]);
        let composed = compose_edits(&[e1, e2]);
        assert_eq!(composed.exclude.len(), 4);
    }

    #[test]
    fn compose_edits_later_replace_wins() {
        let e1 = ContextEdit::none().replace(0, vec![ContentBlock::text("first")]);
        let e2 = ContextEdit::none().replace(0, vec![ContentBlock::text("second")]);
        let composed = compose_edits(&[e1, e2]);
        assert_eq!(composed.replace.len(), 1);
        match &composed.replace[0].1[0] {
            ContentBlock::Text { text } => assert_eq!(text, "second"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn context_edit_serde_roundtrip() {
        let edit = ContextEdit::excluding([1, 3]).replace(2, vec![ContentBlock::text("edited")]);
        let json = serde_json::to_string(&edit).unwrap();
        let parsed: ContextEdit = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.exclude.len(), 2);
        assert_eq!(parsed.replace.len(), 1);
    }

    #[test]
    fn apply_preserves_message_ids() {
        let messages = make_messages(3);
        let original_ids: Vec<_> = messages.iter().map(|m| m.id).collect();

        let edit = ContextEdit::none().replace(1, vec![ContentBlock::text("new")]);
        let result = apply_edit(&messages, &edit);

        // All IDs preserved (replace doesn't change ID)
        for (i, msg) in result.iter().enumerate() {
            assert_eq!(msg.id, original_ids[i]);
        }
    }
}
