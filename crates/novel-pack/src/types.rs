use serde::{Deserialize, Serialize};

/// The overarching story setting and character definitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryBible {
    pub title: String,
    pub genre: String,
    pub characters: Vec<Character>,
    pub world_notes: String,
}

/// A character in the story.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Character {
    pub name: String,
    pub role: String,
    pub description: String,
}

/// Outline for a single chapter, containing multiple scenes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterOutline {
    pub chapter_number: u32,
    pub summary: String,
    pub scenes: Vec<SceneOutline>,
}

/// Outline for a single scene within a chapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneOutline {
    pub scene_number: u32,
    pub setting: String,
    pub characters: Vec<String>,
    pub purpose: String,
    pub key_events: Vec<String>,
}

/// Structured review report from the reviewer agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub score: f64,
    pub passed: bool,
    pub issues: Vec<String>,
    pub suggestions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bible() -> StoryBible {
        StoryBible {
            title: "Test Novel".to_string(),
            genre: "Fantasy".to_string(),
            characters: vec![
                Character {
                    name: "Alice".to_string(),
                    role: "Protagonist".to_string(),
                    description: "A curious adventurer".to_string(),
                },
                Character {
                    name: "Bob".to_string(),
                    role: "Mentor".to_string(),
                    description: "A wise old wizard".to_string(),
                },
            ],
            world_notes: "A magical realm".to_string(),
        }
    }

    fn sample_outline() -> ChapterOutline {
        ChapterOutline {
            chapter_number: 1,
            summary: "The beginning".to_string(),
            scenes: vec![
                SceneOutline {
                    scene_number: 1,
                    setting: "A quiet village".to_string(),
                    characters: vec!["Alice".to_string()],
                    purpose: "Introduce protagonist".to_string(),
                    key_events: vec!["Alice finds a mysterious letter".to_string()],
                },
                SceneOutline {
                    scene_number: 2,
                    setting: "The forest edge".to_string(),
                    characters: vec!["Alice".to_string(), "Bob".to_string()],
                    purpose: "Meet the mentor".to_string(),
                    key_events: vec!["Bob offers guidance".to_string()],
                },
            ],
        }
    }

    #[test]
    fn serialize_story_bible() {
        let bible = sample_bible();
        let json = serde_json::to_string(&bible).unwrap();
        let parsed: StoryBible = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, "Test Novel");
        assert_eq!(parsed.characters.len(), 2);
    }

    #[test]
    fn serialize_chapter_outline() {
        let outline = sample_outline();
        let json = serde_json::to_string(&outline).unwrap();
        let parsed: ChapterOutline = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chapter_number, 1);
        assert_eq!(parsed.scenes.len(), 2);
        assert_eq!(parsed.scenes[0].setting, "A quiet village");
    }

    #[test]
    fn serialize_review_report() {
        let report = ReviewReport {
            score: 0.85,
            passed: true,
            issues: vec!["pacing".to_string()],
            suggestions: vec!["add more detail".to_string()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: ReviewReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.score, 0.85);
        assert!(parsed.passed);
    }
}
