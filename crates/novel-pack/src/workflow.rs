use std::sync::Arc;

use lim_core::message::AgentMessage;
use lim_workflow::context::WorkflowContext;
use lim_workflow::graph::WorkflowGraph;
use lim_workflow::node::{Node, NodeKind, NodeOutput};

use crate::editor;
use crate::orchestrator;
use crate::reviewer;
use crate::types::{ChapterOutline, StoryBible};
use crate::writer;

/// Build the complete workflow DAG for generating one chapter.
///
/// The DAG structure:
/// - One `ReviewLoop` node per scene (all parallel, no predecessors)
/// - One `AgentRun` editor node (depends on all scene review loops)
/// - One `Transform` chapter node (depends on editor, stores artifact data)
pub fn build_chapter_workflow(
    story_bible: &StoryBible,
    chapter_outline: &ChapterOutline,
) -> WorkflowGraph {
    let scenes = orchestrator::split_scenes(story_bible, chapter_outline);

    let mut graph = WorkflowGraph::new();

    // 1. Create a ReviewLoop node for each scene
    let mut scene_node_ids: Vec<String> = Vec::new();

    for scene_data in &scenes {
        let node_id = format!("scene_{}_loop", scene_data.scene.scene_number);
        let scene_json = serde_json::to_value(scene_data).unwrap_or_default();

        let writer_prompt_fn: Arc<
            dyn Fn(&serde_json::Value, Option<&str>) -> String + Send + Sync,
        > = Arc::new(move |_input, feedback| {
            writer::build_writer_prompt(&scene_json, feedback)
        });

        let reviewer_prompt_fn: Arc<dyn Fn(&serde_json::Value) -> String + Send + Sync> =
            Arc::new(reviewer::build_reviewer_prompt);

        graph.add_node(Node::new(
            &node_id,
            NodeKind::ReviewLoop {
                writer_prompt_fn,
                reviewer_prompt_fn,
                pass_threshold: 0.7,
                max_retries: 2,
            },
        ));
        scene_node_ids.push(node_id);
    }

    // 2. Create the Editor node (AgentRun that combines all scene drafts)
    let editor_system_prompt = editor::editor_system_prompt();
    let scene_ids_for_editor = scene_node_ids.clone();

    let editor_message_fn: Arc<dyn Fn(&WorkflowContext) -> AgentMessage + Send + Sync> =
        Arc::new(move |ctx: &WorkflowContext| {
            // Collect all scene draft outputs from context
            let mut drafts = Vec::new();
            for scene_id in &scene_ids_for_editor {
                if let Some(NodeOutput::Data(v)) = ctx.get_output(scene_id).as_ref() {
                    drafts.push(v.clone());
                }
            }
            let combined = serde_json::json!({ "items": drafts });
            let prompt = editor::build_editor_prompt(&combined);
            AgentMessage::user(&prompt)
        });

    graph.add_node(Node::new(
        "editor",
        NodeKind::AgentRun {
            system_prompt: editor_system_prompt,
            initial_message_fn: editor_message_fn,
        },
    ));

    // 3. Create the Chapter transform node (stores final output as data)
    let chapter_number = chapter_outline.chapter_number;
    let chapter_transform: Arc<
        dyn Fn(&WorkflowContext, &NodeOutput) -> NodeOutput + Send + Sync,
    > = Arc::new(move |_ctx, editor_output| {
        let chapter_text = editor_output
            .extract_text()
            .unwrap_or_else(|| "[empty chapter]".to_string());

        NodeOutput::Data(serde_json::json!({
            "chapter_number": chapter_number,
            "text": chapter_text,
        }))
    });

    graph.add_node(Node::new(
        "chapter",
        NodeKind::Transform {
            transform: chapter_transform,
        },
    ));

    // 4. Add edges
    // All scene loops → editor (fan-in)
    for scene_id in &scene_node_ids {
        graph.add_edge(scene_id.as_str(), "editor");
    }
    // editor → chapter
    graph.add_edge("editor", "chapter");

    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Character, SceneOutline};

    fn test_bible() -> StoryBible {
        StoryBible {
            title: "Test Novel".to_string(),
            genre: "Fantasy".to_string(),
            characters: vec![Character {
                name: "Hero".to_string(),
                role: "Protagonist".to_string(),
                description: "Brave adventurer".to_string(),
            }],
            world_notes: "Magic realm".to_string(),
        }
    }

    fn test_outline() -> ChapterOutline {
        ChapterOutline {
            chapter_number: 1,
            summary: "The beginning".to_string(),
            scenes: vec![
                SceneOutline {
                    scene_number: 1,
                    setting: "Village".to_string(),
                    characters: vec!["Hero".to_string()],
                    purpose: "Introduction".to_string(),
                    key_events: vec!["Hero wakes up".to_string()],
                },
                SceneOutline {
                    scene_number: 2,
                    setting: "Forest".to_string(),
                    characters: vec!["Hero".to_string()],
                    purpose: "Adventure begins".to_string(),
                    key_events: vec!["Hero enters forest".to_string()],
                },
                SceneOutline {
                    scene_number: 3,
                    setting: "Cave".to_string(),
                    characters: vec!["Hero".to_string()],
                    purpose: "First challenge".to_string(),
                    key_events: vec!["Hero finds treasure".to_string()],
                },
            ],
        }
    }

    #[test]
    fn build_workflow_creates_correct_node_count() {
        let graph = build_chapter_workflow(&test_bible(), &test_outline());
        // 3 scene loops + 1 editor + 1 chapter = 5 nodes
        assert_eq!(graph.node_ids().len(), 5);
    }

    #[test]
    fn build_workflow_topological_order() {
        let graph = build_chapter_workflow(&test_bible(), &test_outline());
        let levels = graph.topological_order().unwrap();

        // Level 0: 3 scene loops (parallel)
        // Level 1: editor
        // Level 2: chapter
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 3);
        assert_eq!(levels[1], vec!["editor"]);
        assert_eq!(levels[2], vec!["chapter"]);
    }

    #[test]
    fn build_workflow_edges_are_correct() {
        let graph = build_chapter_workflow(&test_bible(), &test_outline());

        // editor should have 3 predecessors (the scene loops)
        let preds = graph.predecessors("editor");
        assert_eq!(preds.len(), 3);

        // chapter should have 1 predecessor (editor)
        let preds = graph.predecessors("chapter");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0], "editor");

        // scene loops should have no predecessors
        let preds = graph.predecessors("scene_1_loop");
        assert!(preds.is_empty());
    }

    #[test]
    fn build_single_scene_workflow() {
        let bible = test_bible();
        let outline = ChapterOutline {
            chapter_number: 1,
            summary: "Short chapter".to_string(),
            scenes: vec![SceneOutline {
                scene_number: 1,
                setting: "Room".to_string(),
                characters: vec!["Hero".to_string()],
                purpose: "Think".to_string(),
                key_events: vec!["Reflection".to_string()],
            }],
        };

        let graph = build_chapter_workflow(&bible, &outline);
        // 1 scene loop + 1 editor + 1 chapter = 3 nodes
        assert_eq!(graph.node_ids().len(), 3);

        let levels = graph.topological_order().unwrap();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1);
    }
}
