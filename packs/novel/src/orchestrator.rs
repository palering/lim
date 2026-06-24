use crate::types::{ChapterOutline, SceneOutline, StoryBible};

/// Split a chapter outline into individual scene data packets.
///
/// Phase 2 simplification: directly uses the scenes from the outline
/// rather than having an agent decompose the chapter.
pub fn split_scenes(bible: &StoryBible, outline: &ChapterOutline) -> Vec<SceneData> {
    outline
        .scenes
        .iter()
        .map(|scene| SceneData {
            story_title: bible.title.clone(),
            story_genre: bible.genre.clone(),
            world_notes: bible.world_notes.clone(),
            chapter_number: outline.chapter_number,
            chapter_summary: outline.summary.clone(),
            scene: scene.clone(),
        })
        .collect()
}

/// All the context a writer needs for a single scene.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SceneData {
    pub story_title: String,
    pub story_genre: String,
    pub world_notes: String,
    pub chapter_number: u32,
    pub chapter_summary: String,
    pub scene: SceneOutline,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Character, StoryBible};

    #[test]
    fn split_scenes_from_outline() {
        let bible = StoryBible {
            title: "My Novel".to_string(),
            genre: "Sci-Fi".to_string(),
            characters: vec![Character {
                name: "Zara".to_string(),
                role: "Hero".to_string(),
                description: "Brave explorer".to_string(),
            }],
            world_notes: "Space station".to_string(),
        };
        let outline = ChapterOutline {
            chapter_number: 1,
            summary: "First contact".to_string(),
            scenes: vec![
                SceneOutline {
                    scene_number: 1,
                    setting: "Bridge".to_string(),
                    characters: vec!["Zara".to_string()],
                    purpose: "Detect signal".to_string(),
                    key_events: vec!["Alarm sounds".to_string()],
                },
                SceneOutline {
                    scene_number: 2,
                    setting: "Airlock".to_string(),
                    characters: vec!["Zara".to_string()],
                    purpose: "Investigate".to_string(),
                    key_events: vec!["Door opens".to_string()],
                },
            ],
        };

        let scenes = split_scenes(&bible, &outline);
        assert_eq!(scenes.len(), 2);
        assert_eq!(scenes[0].story_title, "My Novel");
        assert_eq!(scenes[0].scene.setting, "Bridge");
        assert_eq!(scenes[1].scene.setting, "Airlock");
        assert_eq!(scenes[0].chapter_number, 1);
    }
}
