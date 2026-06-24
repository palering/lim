/// Build the system prompt for the chapter editor agent.
pub fn editor_system_prompt() -> String {
    "You are a skilled fiction editor. You will receive multiple scene drafts \
     for a single chapter. Your job is to weave them into a cohesive, flowing \
     chapter. Smooth transitions between scenes, ensure consistent tone and \
     voice, and fix any continuity issues. Output the complete chapter text."
        .to_string()
}

/// Build the user prompt for the editor, containing all scene drafts.
pub fn build_editor_prompt(scene_drafts: &serde_json::Value) -> String {
    format!(
        "Here are the scene drafts for this chapter. Combine them into a single, \
         cohesive chapter with smooth transitions:\n\n{}\n\n\
         Output the complete chapter text.",
        serde_json::to_string_pretty(scene_drafts).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_prompt_includes_drafts() {
        let drafts = serde_json::json!({
            "items": [
                {"text": "Scene one content."},
                {"text": "Scene two content."},
            ]
        });
        let prompt = build_editor_prompt(&drafts);
        assert!(prompt.contains("Scene one"));
        assert!(prompt.contains("Scene two"));
        assert!(prompt.contains("cohesive"));
    }

    #[test]
    fn system_prompt_describes_role() {
        let prompt = editor_system_prompt();
        assert!(prompt.contains("editor"));
        assert!(prompt.contains("chapter"));
    }
}
