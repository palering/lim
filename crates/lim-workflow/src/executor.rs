#![allow(clippy::type_complexity)]

use std::sync::Arc;

use futures::future::join_all;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use lim_agent::agent_loop::{AgentConfig, AgentLoop};
use lim_agent::registry::ToolRegistry;
use lim_core::error::Result;
use lim_core::id::ArtifactId;
use lim_core::message::AgentMessage;
use lim_llm::provider::Provider;

use crate::artifact::{Artifact, ArtifactStore};
use crate::context::WorkflowContext;
use crate::events::WorkflowEvent;
use crate::graph::WorkflowGraph;
use crate::node::{NodeKind, NodeOutput};
use crate::types::NodeId;

/// Executes a workflow DAG.
///
/// Processes nodes in topological order, running nodes within the same
/// level concurrently via `join_all`.
pub struct WorkflowExecutor {
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    artifact_store: Arc<ArtifactStore>,
    cancel: CancellationToken,
    event_tx: broadcast::Sender<WorkflowEvent>,
}

impl WorkflowExecutor {
    /// Create a new [`WorkflowExecutor`] with the given dependencies.
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        artifact_store: Arc<ArtifactStore>,
        cancel: CancellationToken,
        event_tx: broadcast::Sender<WorkflowEvent>,
    ) -> Self {
        Self { provider, registry, artifact_store, cancel, event_tx }
    }

    /// Execute a workflow graph and return the final context.
    pub async fn execute(
        &self,
        graph: &WorkflowGraph,
        context: WorkflowContext,
    ) -> Result<WorkflowContext> {
        let _ = self.event_tx.send(WorkflowEvent::WorkflowStart);
        info!("workflow execution started");

        let levels = graph.topological_order()?;

        for (level_idx, level) in levels.iter().enumerate() {
            debug!(level = level_idx, nodes = ?level, "executing level");

            if self.cancel.is_cancelled() {
                warn!("workflow cancelled before level {level_idx}");
                break;
            }

            // Execute all nodes in this level concurrently.
            // Clone Arc handles + owned data so futures don't borrow stack locals.
            let futures: Vec<_> = level
                .iter()
                .map(|node_id| {
                    let node = graph.node(node_id).expect("node must exist");
                    let nid = node_id.clone();
                    let kind = node.kind.clone();
                    let ctx = context.clone();
                    let predecessors = graph.predecessors(node_id);
                    let pred_outputs = ctx.outputs(&predecessors);
                    let provider = Arc::clone(&self.provider);
                    let registry = Arc::clone(&self.registry);
                    let cancel = self.cancel.clone();
                    let event_tx = self.event_tx.clone();
                    let artifact_store = Arc::clone(&self.artifact_store);

                    async move {
                        WorkflowExecutor::execute_node(
                            &nid,
                            kind,
                            &ctx,
                            &pred_outputs,
                            provider,
                            registry,
                            artifact_store,
                            cancel,
                            event_tx,
                        )
                        .await
                    }
                })
                .collect();

            let results = join_all(futures).await;

            // Store outputs in context
            for (node_id, result) in level.iter().zip(results) {
                match result {
                    Ok(output) => {
                        context.set_output(node_id.clone(), output);
                    }
                    Err(e) => {
                        warn!(node_id, error = %e, "node execution failed");
                        return Err(e);
                    }
                }
            }
        }

        let _ = self.event_tx.send(WorkflowEvent::WorkflowEnd);
        info!("workflow execution completed");

        Ok(context)
    }

    /// Execute a single node.
    #[allow(clippy::too_many_arguments)]
    async fn execute_node(
        node_id: &NodeId,
        kind: NodeKind,
        ctx: &WorkflowContext,
        pred_outputs: &[NodeOutput],
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        artifact_store: Arc<ArtifactStore>,
        cancel: CancellationToken,
        event_tx: broadcast::Sender<WorkflowEvent>,
    ) -> Result<NodeOutput> {
        let _ = event_tx.send(WorkflowEvent::NodeStart { node_id: node_id.clone() });

        let output = match kind {
            NodeKind::AgentRun { system_prompt, initial_message_fn } => {
                Self::run_agent_node(
                    node_id,
                    &system_prompt,
                    initial_message_fn,
                    ctx,
                    provider,
                    registry,
                    cancel,
                )
                .await?
            }

            NodeKind::ParallelFanOut { splitter } => {
                let input = combine_outputs(pred_outputs);
                let branches = splitter(ctx, &input);
                for (label, data) in branches {
                    ctx.set_data(format!("{node_id}:{label}"), data);
                }
                NodeOutput::Empty
            }

            NodeKind::FanIn { combiner } => combiner(ctx, pred_outputs),

            NodeKind::ReviewLoop {
                writer_prompt_fn,
                reviewer_prompt_fn,
                pass_threshold,
                max_retries,
            } => {
                Self::run_review_loop(
                    node_id,
                    writer_prompt_fn,
                    reviewer_prompt_fn,
                    pass_threshold,
                    max_retries,
                    ctx,
                    pred_outputs,
                    provider,
                    registry,
                    artifact_store,
                    cancel,
                    event_tx.clone(),
                )
                .await?
            }

            NodeKind::Transform { transform } => {
                let input = combine_outputs(pred_outputs);
                transform(ctx, &input)
            }
        };

        let summary = format!("{output:?}");
        let summary = if summary.len() > 100 { format!("{}...", &summary[..100]) } else { summary };
        let _ = event_tx
            .send(WorkflowEvent::NodeEnd { node_id: node_id.clone(), output_summary: summary });

        Ok(output)
    }

    /// Run an agent node.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_node(
        node_id: &NodeId,
        system_prompt: &str,
        initial_message_fn: Arc<dyn Fn(&WorkflowContext) -> AgentMessage + Send + Sync>,
        ctx: &WorkflowContext,
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        cancel: CancellationToken,
    ) -> Result<NodeOutput> {
        debug!(node_id, "running agent node");

        let (event_tx, _event_rx) = broadcast::channel(100);
        let config = AgentConfig {
            max_turns: 10,
            system_prompt: Some(system_prompt.to_string()),
            model: String::new(),
        };

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel).with_config(config);

        let initial_message = initial_message_fn(ctx);
        let messages = agent.run(initial_message).await?;

        Ok(NodeOutput::AgentMessages(messages))
    }

    /// Run a review loop node.
    #[allow(clippy::too_many_arguments)]
    async fn run_review_loop(
        node_id: &NodeId,
        writer_prompt_fn: Arc<dyn Fn(&serde_json::Value, Option<&str>) -> String + Send + Sync>,
        reviewer_prompt_fn: Arc<dyn Fn(&serde_json::Value) -> String + Send + Sync>,
        pass_threshold: f64,
        max_retries: usize,
        _ctx: &WorkflowContext,
        pred_outputs: &[NodeOutput],
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        artifact_store: Arc<ArtifactStore>,
        cancel: CancellationToken,
        event_tx: broadcast::Sender<WorkflowEvent>,
    ) -> Result<NodeOutput> {
        debug!(node_id, "running review loop");

        // Get scene data from predecessors
        let input = combine_outputs(pred_outputs);
        let scene_data = match &input {
            NodeOutput::Data(v) => v.clone(),
            _ => serde_json::Value::Null,
        };

        let mut current_draft = String::new();
        let mut feedback: Option<String> = None;

        for attempt in 0..=max_retries {
            if cancel.is_cancelled() {
                break;
            }

            // 1. Writer: generate draft
            let writer_prompt = writer_prompt_fn(&scene_data, feedback.as_deref());
            let writer_output = Self::run_simple_agent(
                &format!("{node_id}:writer:{attempt}"),
                "You are a creative writer. Write according to the instructions.",
                &writer_prompt,
                Arc::clone(&provider),
                Arc::clone(&registry),
                cancel.clone(),
            )
            .await?;

            current_draft =
                writer_output.extract_text().unwrap_or_else(|| "[empty draft]".to_string());

            // 2. Reviewer: evaluate draft
            let draft_value = serde_json::json!({
                "scene": scene_data,
                "draft": current_draft,
            });
            let reviewer_prompt = reviewer_prompt_fn(&draft_value);
            let reviewer_output = Self::run_simple_agent(
                &format!("{node_id}:reviewer:{attempt}"),
                "You are a strict but fair reviewer. Evaluate the draft and respond with JSON: {\"score\": 0.0-1.0, \"issues\": [...], \"suggestions\": [...]}.",
                &reviewer_prompt,
                Arc::clone(&provider),
                Arc::clone(&registry),
                cancel.clone(),
            )
            .await?;

            // 3. Parse review
            let review_text = reviewer_output.extract_text().unwrap_or_default();

            let review = parse_review_response(&review_text);

            let _ = event_tx.send(WorkflowEvent::ReviewResult {
                node_id: node_id.clone(),
                passed: review.passed,
                score: review.score,
                attempt,
            });

            info!(node_id, attempt, score = review.score, passed = review.passed, "review result");

            // Store review report as artifact
            let report_id = ArtifactId::new();
            let report_artifact = Artifact {
                id: report_id,
                kind: "review_report".to_string(),
                content: serde_json::json!({
                    "score": review.score,
                    "passed": review.passed,
                    "issues": review.issues,
                    "suggestions": review.suggestions,
                    "attempt": attempt,
                    "node_id": node_id,
                }),
                created_at: unix_timestamp(),
                parent: None,
                metadata: serde_json::json!({"node_id": node_id}),
            };
            artifact_store.store(&report_artifact).await?;

            let _ = event_tx.send(WorkflowEvent::ArtifactCreated {
                artifact_id: report_id,
                kind: "review_report".to_string(),
                node_id: node_id.clone(),
            });

            if review.passed || review.score >= pass_threshold {
                break;
            }

            // Build feedback for next attempt
            feedback = Some(format!(
                "Previous draft scored {}. Issues: {}. Suggestions: {}",
                review.score,
                review.issues.join("; "),
                review.suggestions.join("; "),
            ));
        }

        // Store final draft as artifact
        let draft_id = ArtifactId::new();
        let draft_artifact = Artifact {
            id: draft_id,
            kind: "scene_draft".to_string(),
            content: serde_json::json!({
                "text": current_draft,
                "node_id": node_id,
            }),
            created_at: unix_timestamp(),
            parent: None,
            metadata: serde_json::json!({"node_id": node_id}),
        };
        artifact_store.store(&draft_artifact).await?;

        let _ = event_tx.send(WorkflowEvent::ArtifactCreated {
            artifact_id: draft_id,
            kind: "scene_draft".to_string(),
            node_id: node_id.clone(),
        });

        Ok(NodeOutput::Data(serde_json::json!({
            "draft": current_draft,
            "artifact_id": draft_id.to_string(),
        })))
    }

    /// Run a simple single-turn agent (for writer/reviewer in review loops).
    async fn run_simple_agent(
        _label: &str,
        system_prompt: &str,
        user_message: &str,
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        cancel: CancellationToken,
    ) -> Result<NodeOutput> {
        let (event_tx, _event_rx) = broadcast::channel(100);
        let config = AgentConfig {
            max_turns: 1,
            system_prompt: Some(system_prompt.to_string()),
            model: String::new(),
        };

        let mut agent = AgentLoop::new(provider, registry, event_tx, cancel).with_config(config);

        let messages = agent.run(AgentMessage::user(user_message)).await?;
        Ok(NodeOutput::AgentMessages(messages))
    }
}

/// Combine multiple node outputs into a single Data value.
fn combine_outputs(outputs: &[NodeOutput]) -> NodeOutput {
    if outputs.is_empty() {
        return NodeOutput::Empty;
    }
    if outputs.len() == 1 {
        return outputs[0].clone();
    }

    // Multiple outputs: combine as JSON array of texts or values
    let values: Vec<serde_json::Value> = outputs
        .iter()
        .map(|o| match o {
            NodeOutput::Data(v) => v.clone(),
            NodeOutput::AgentMessages(msgs) => {
                let text: String = msgs
                    .iter()
                    .filter(|m| m.role == lim_core::message::Role::Assistant)
                    .flat_map(|m| {
                        m.content.iter().filter_map(|b| {
                            if let lim_core::content::ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                serde_json::json!({"text": text})
            }
            NodeOutput::Artifacts(ids) => {
                let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                serde_json::json!({"artifacts": id_strs})
            }
            NodeOutput::Empty => serde_json::Value::Null,
        })
        .collect();

    NodeOutput::Data(serde_json::json!({ "items": values }))
}

/// Parse a review response from the reviewer agent.
///
/// Expects JSON like: {"score": 0.8, "issues": [...], "suggestions": [...]}
/// Falls back to a default "passed" review if parsing fails.
fn parse_review_response(text: &str) -> ReviewParseResult {
    // Try to extract JSON from the response
    let json_str = extract_json(text);
    if let Some(json) = json_str
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
    {
        let score = value.get("score").and_then(|v| v.as_f64()).unwrap_or(0.5);
        let issues = value
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let suggestions = value
            .get("suggestions")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        return ReviewParseResult { score, passed: score >= 0.7, issues, suggestions };
    }

    // Fallback: treat as passed with moderate score
    ReviewParseResult { score: 0.7, passed: true, issues: vec![], suggestions: vec![] }
}

/// Try to extract a JSON object from text.
fn extract_json(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth = 0;
    for (i, ch) in text[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Get current Unix timestamp as a string.
fn unix_timestamp() -> String {
    use std::time::SystemTime;
    // SAFETY: SystemTime::now() is guaranteed to be after UNIX_EPOCH on any
    // correctly configured system; if a clock skew makes it earlier, panicking
    // is the desired behavior since we cannot generate a meaningful timestamp.
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock is before UNIX_EPOCH")
        .as_secs()
        .to_string()
}

struct ReviewParseResult {
    score: f64,
    passed: bool,
    issues: Vec<String>,
    suggestions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_review_json() {
        let text = r#"{"score": 0.85, "issues": ["pacing"], "suggestions": ["add more detail"]}"#;
        let result = parse_review_response(text);
        assert_eq!(result.score, 0.85);
        assert!(result.passed);
        assert_eq!(result.issues, vec!["pacing"]);
        assert_eq!(result.suggestions, vec!["add more detail"]);
    }

    #[test]
    fn parse_review_with_surrounding_text() {
        let text = r#"Here is my review: {"score": 0.6, "issues": ["dialogue"], "suggestions": ["more natural"]} Hope this helps!"#;
        let result = parse_review_response(text);
        assert_eq!(result.score, 0.6);
        assert!(!result.passed);
    }

    #[test]
    fn parse_review_fallback_on_garbage() {
        let text = "This draft is great but I can't format JSON";
        let result = parse_review_response(text);
        assert!(result.passed);
        assert_eq!(result.score, 0.7);
    }

    #[test]
    fn extract_json_simple() {
        let text = r#"prefix {"key": "value"} suffix"#;
        let json = extract_json(text).unwrap();
        assert_eq!(json, r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_nested() {
        let text = r#"{"a": {"b": 1}}"#;
        let json = extract_json(text).unwrap();
        assert_eq!(json, r#"{"a": {"b": 1}}"#);
    }

    #[test]
    fn combine_empty_outputs() {
        let result = combine_outputs(&[]);
        assert!(matches!(result, NodeOutput::Empty));
    }

    #[test]
    fn combine_single_output() {
        let outputs = vec![NodeOutput::Data(serde_json::json!(42))];
        let result = combine_outputs(&outputs);
        match result {
            NodeOutput::Data(v) => assert_eq!(v, serde_json::json!(42)),
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn combine_multiple_outputs() {
        let outputs = vec![
            NodeOutput::Data(serde_json::json!("a")),
            NodeOutput::Data(serde_json::json!("b")),
        ];
        let result = combine_outputs(&outputs);
        match result {
            NodeOutput::Data(v) => {
                let items = v.get("items").unwrap().as_array().unwrap();
                assert_eq!(items.len(), 2);
            }
            _ => panic!("expected Data"),
        }
    }
}
