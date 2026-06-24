/// Build the system prompt for a scene writer agent.
pub fn writer_system_prompt() -> String {
    "You are a skilled creative fiction writer. Write vivid, engaging prose \
     with strong characterization and sensory detail. Follow the scene outline \
     provided and stay true to the story's genre and world."
        .to_string()
}

/// Build the user prompt for writing a scene draft.
///
/// If `feedback` is provided, it will be included as rewrite guidance.
pub fn build_writer_prompt(scene_json: &serde_json::Value, feedback: Option<&str>) -> String {
    let mut prompt = format!(
        "Write a scene based on the following outline:\n\n{}\n\n\
         Write the complete scene draft. Focus on vivid prose, character voice, \
         and narrative tension.",
        serde_json::to_string_pretty(scene_json).unwrap_or_default(),
    );

    if let Some(fb) = feedback {
        prompt.push_str(&format!(
            "\n\nPrevious version feedback: {fb}\n\n\
             Address the feedback while maintaining the scene's core purpose."
        ));
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_prompt_includes_scene() {
        let scene = serde_json::json!({"setting": "A dark forest"});
        let prompt = build_writer_prompt(&scene, None);
        assert!(prompt.contains("dark forest"));
        assert!(!prompt.contains("feedback"));
    }

    #[test]
    fn writer_prompt_with_feedback() {
        let scene = serde_json::json!({"setting": "A dark forest"});
        let prompt = build_writer_prompt(&scene, Some("Add more tension"));
        assert!(prompt.contains("dark forest"));
        assert!(prompt.contains("Add more tension"));
    }

    #[test]
    fn system_prompt_is_not_empty() {
        let prompt = writer_system_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("writer"));
    }
}
