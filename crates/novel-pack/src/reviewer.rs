/// Build the system prompt for a scene reviewer agent.
pub fn reviewer_system_prompt() -> String {
    "You are a strict but fair fiction reviewer. Evaluate scene drafts for \
     prose quality, characterization, pacing, and adherence to the outline. \
     Respond with JSON: {\"score\": 0.0-1.0, \"issues\": [...], \"suggestions\": [...]}."
        .to_string()
}

/// Build the user prompt for reviewing a scene draft.
pub fn build_reviewer_prompt(draft_json: &serde_json::Value) -> String {
    format!(
        "Review the following scene draft:\n\n{}\n\n\
         Evaluate for prose quality, characterization, pacing, and outline adherence. \
         Respond with JSON: {{\"score\": 0.0-1.0, \"issues\": [...], \"suggestions\": [...]}}.",
        serde_json::to_string_pretty(draft_json).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_prompt_includes_draft() {
        let draft = serde_json::json!({"draft": "The wind howled through the trees."});
        let prompt = build_reviewer_prompt(&draft);
        assert!(prompt.contains("wind howled"));
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn system_prompt_requests_json() {
        let prompt = reviewer_system_prompt();
        assert!(prompt.contains("JSON"));
        assert!(prompt.contains("score"));
    }
}
