//! Context window management — compaction of message history to fit within
//! the model's context window.
//!
//! The core principle: the full `AgentMessage` history is NEVER modified.
//! Compaction happens only at the `LlmMessage` projection layer, right
//! before sending to the provider.

use std::sync::Arc;

use futures::StreamExt;
use tracing::debug;

use lim_core::content::ContentBlock;
use lim_core::error::Result;
use lim_core::message::{AgentMessage, LlmMessage, Role, convert_to_llm};

use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

/// Configuration for context window management.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Total context window size in tokens.
    pub context_window: u64,
    /// Tokens reserved for model output generation.
    pub max_output_tokens: u64,
    /// Fraction of available budget that triggers compaction (0.0–1.0).
    /// Default: 0.8 — compact when usage exceeds 80% of budget.
    pub trigger_ratio: f64,
    /// Number of recent user/assistant exchange pairs to preserve verbatim.
    /// Default: 4.
    pub keep_recent_pairs: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            max_output_tokens: 8_192,
            trigger_ratio: 0.8,
            keep_recent_pairs: 4,
        }
    }
}

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// The compacted messages ready for LLM consumption.
    pub messages: Vec<LlmMessage>,
    /// Whether compaction was actually performed.
    pub was_compacted: bool,
    /// Number of messages before compaction.
    pub messages_before: usize,
    /// Number of messages after compaction (summary + recent).
    pub messages_after: usize,
    /// Estimated token count before compaction.
    pub tokens_before: u64,
    /// Estimated token count after compaction.
    pub tokens_after: u64,
}

/// Manages the context window by compacting older messages when approaching
/// the model's token limit.
///
/// The full `AgentMessage` history is preserved — compaction only affects
/// the `LlmMessage` projection sent to the provider.
pub struct ContextWindowManager {
    config: ContextConfig,
}

impl ContextWindowManager {
    /// Create a new context window manager with the given configuration.
    pub fn new(config: ContextConfig) -> Self {
        Self { config }
    }

    /// Compact the message history to fit within the context window budget.
    ///
    /// If the estimated token count is within the budget, returns all messages
    /// converted via `convert_to_llm()`. Otherwise, summarizes older messages
    /// using the provider and returns a compacted message list.
    pub async fn compact(
        &self,
        messages: &[AgentMessage],
        provider: &Arc<dyn Provider>,
        model: &str,
    ) -> Result<CompactionResult> {
        let tokens_before = estimate_tokens(messages);
        let budget = self.available_budget();

        if tokens_before <= budget {
            let llm_messages = convert_to_llm(messages);
            return Ok(CompactionResult {
                messages_after: messages.len(),
                messages_before: messages.len(),
                tokens_after: tokens_before,
                tokens_before,
                messages: llm_messages,
                was_compacted: false,
            });
        }

        // Find the split point: system messages + old messages | recent messages
        let recent_start = self.find_recent_start(messages);

        if recent_start == 0 {
            // All messages are in the recent window — can't compact further
            let llm_messages = convert_to_llm(messages);
            return Ok(CompactionResult {
                messages_after: messages.len(),
                messages_before: messages.len(),
                tokens_after: tokens_before,
                tokens_before,
                messages: llm_messages,
                was_compacted: false,
            });
        }

        // Prepare compaction: extract system messages and build summary text
        let (system_msgs, old_text) = prepare_compaction(&messages[..recent_start]);

        // Generate summary of old messages (if any non-system messages exist)
        let summary_text = if old_text.is_empty() {
            String::new()
        } else {
            self.summarize(&old_text, Arc::clone(provider), model).await?
        };

        // Build the compacted message list
        let mut llm_messages: Vec<LlmMessage> = Vec::new();

        // 1. System messages first
        for msg in system_msgs {
            if let Some(llm_msg) = LlmMessage::from_agent_message(msg) {
                llm_messages.push(llm_msg);
            }
        }

        // 2. Summary as a User-role message
        if !summary_text.is_empty() {
            llm_messages.push(LlmMessage {
                role: Role::User,
                content: vec![ContentBlock::text(format!(
                    "[Earlier conversation summary]\n{summary_text}\n[End of summary — conversation continues below]"
                ))],
                tool_call_ids: vec![],
            });
        }

        // 3. Recent messages verbatim
        for msg in &messages[recent_start..] {
            if let Some(llm_msg) = LlmMessage::from_agent_message(msg) {
                llm_messages.push(llm_msg);
            }
        }

        let tokens_after = estimate_llm_tokens(&llm_messages);

        debug!(
            messages_before = messages.len(),
            messages_after = llm_messages.len(),
            tokens_before,
            tokens_after,
            "context compacted"
        );

        Ok(CompactionResult {
            was_compacted: true,
            messages_before: messages.len(),
            messages_after: llm_messages.len(),
            tokens_before,
            tokens_after,
            messages: llm_messages,
        })
    }

    /// Available token budget for input messages.
    fn available_budget(&self) -> u64 {
        let available = self.config.context_window.saturating_sub(self.config.max_output_tokens);
        (available as f64 * self.config.trigger_ratio) as u64
    }

    /// Find the index where the recent conversation window starts.
    ///
    /// Scans backward through messages counting User messages to identify
    /// the last N conversation exchange pairs.
    fn find_recent_start(&self, messages: &[AgentMessage]) -> usize {
        let mut user_count = 0;
        let mut recent_start = messages.len();

        for i in (0..messages.len()).rev() {
            if messages[i].role == Role::User {
                user_count += 1;
                if user_count >= self.config.keep_recent_pairs {
                    recent_start = i;
                    break;
                }
            }
        }

        // If we didn't find enough User messages, everything is recent
        if user_count < self.config.keep_recent_pairs {
            return 0;
        }

        // Adjust boundary to keep tool call/result pairs together.
        // Walk back over consecutive tool-related messages so that
        // assistant(tool_call) + tool(result) groups are never split.
        while recent_start > 0 {
            let prev = &messages[recent_start - 1];
            if matches!(prev.role, Role::Assistant | Role::Tool)
                && (prev.role == Role::Tool || prev.has_tool_calls())
            {
                recent_start -= 1;
            } else {
                break;
            }
        }

        recent_start
    }

    /// Generate a summary of older messages using the provider.
    ///
    /// Takes pre-formatted conversation text and an owned Arc to the provider.
    /// The Arc is consumed to enable calling `provider.stream()` via auto-deref.
    async fn summarize(
        &self,
        conversation_text: &str,
        provider: Arc<dyn Provider>,
        model: &str,
    ) -> Result<String> {
        let summary_prompt = format!(
            "Summarize the following conversation history concisely. \
             Preserve: key facts, decisions made, tool outputs, file contents, \
             and context needed to continue the conversation. \
             Omit: pleasantries, redundant details.\n\n{conversation_text}"
        );

        let llm_msg = LlmMessage {
            role: Role::User,
            content: vec![ContentBlock::text(summary_prompt)],
            tool_call_ids: vec![],
        };

        let request = LlmRequest::new(model, vec![llm_msg]);
        let stream = provider.stream(request);
        let events: Vec<_> = collect_stream(stream).await;

        let mut summary = String::new();
        for event in events {
            if let Ok(StreamEvent::TextDelta { text }) = event {
                summary.push_str(&text);
            }
        }

        Ok(summary)
    }
}

/// Collect a BoxStream of results into a Vec.
async fn collect_stream<'a>(
    mut stream: futures::stream::BoxStream<'a, std::result::Result<StreamEvent, lim_llm::ProviderError>>,
) -> Vec<std::result::Result<StreamEvent, lim_llm::ProviderError>> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

/// Separate system messages from old messages and build summary text.
///
/// Returns `(system_messages, formatted_conversation_text)` where the text
/// contains all non-system messages formatted for the summarization prompt.
fn prepare_compaction(messages: &[AgentMessage]) -> (Vec<&AgentMessage>, String) {
    let mut system = Vec::new();
    let mut old_lines = Vec::new();
    for msg in messages {
        if msg.role == Role::System {
            system.push(msg);
        } else {
            old_lines.push(message_to_text(msg));
        }
    }
    (system, old_lines.join("\n\n"))
}

/// Format a message as human-readable text for the summarization prompt.
fn message_to_text(msg: &AgentMessage) -> String {
    let role = match msg.role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    };
    let content: Vec<String> = msg
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Thinking { text } => format!("[thinking: {text}]"),
            ContentBlock::ToolCall { name, arguments, .. } => {
                format!("[tool_call: {name}({arguments})]")
            }
            ContentBlock::Image { mime_type, .. } => {
                format!("[image: {mime_type}]")
            }
            ContentBlock::ArtifactRef { artifact_id, .. } => {
                format!("[artifact: {artifact_id}]")
            }
        })
        .collect();
    format!("{role}: {}", content.join(" "))
}

/// Estimate the token count for a set of agent messages.
///
/// Uses a simple heuristic: `text_length / 4` per content block plus a small
/// overhead per message for role markers and JSON structure.
/// Approximately ±20% accurate for English text.
fn estimate_tokens(messages: &[AgentMessage]) -> u64 {
    let mut total: u64 = 0;
    for msg in messages {
        // Overhead per message: role marker + JSON structure ≈ 10 tokens
        total += 10;
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    total += (text.len() as u64) / 4;
                }
                ContentBlock::Thinking { text } => {
                    total += (text.len() as u64) / 4;
                }
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => {
                    total += (name.len() as u64) / 4;
                    total += (arguments.to_string().len() as u64) / 4;
                }
                ContentBlock::Image { data, .. } => {
                    // Images are encoded as base64 in API calls
                    total += (data.len() as u64) * 4 / 3 / 4;
                }
                ContentBlock::ArtifactRef { label, .. } => {
                    total += label.as_ref().map_or(5, |l| (l.len() as u64) / 4);
                }
            }
        }
    }
    total
}

/// Estimate tokens for LLM messages (post-conversion).
fn estimate_llm_tokens(messages: &[LlmMessage]) -> u64 {
    let mut total: u64 = 0;
    for msg in messages {
        total += 10;
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    total += (text.len() as u64) / 4;
                }
                ContentBlock::Thinking { text } => {
                    total += (text.len() as u64) / 4;
                }
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => {
                    total += (name.len() as u64) / 4;
                    total += (arguments.to_string().len() as u64) / 4;
                }
                _ => {
                    total += 10;
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::id::ToolCallId;
    use lim_llm::mock::{MockProvider, MockResponse};
    use std::sync::Arc;

    // ── Token estimation ──────────────────────────────────────────

    #[test]
    fn token_estimation_within_margin() {
        // ~400 words ≈ ~500 tokens; our heuristic: ~2000/4 = 500 + 10 overhead = 510
        let text = "word ".repeat(400);
        let messages = vec![AgentMessage::user(&text)];
        let tokens = estimate_tokens(&messages);
        assert!(
            (400..=650).contains(&tokens),
            "expected ~500 tokens, got {tokens}"
        );
    }

    #[test]
    fn token_estimation_empty() {
        let tokens = estimate_tokens(&[]);
        assert_eq!(tokens, 0);
    }

    #[test]
    fn token_estimation_multiple_messages() {
        let messages = vec![
            AgentMessage::user("hello"),
            AgentMessage::assistant(vec![ContentBlock::text("world")]),
        ];
        let tokens = estimate_tokens(&messages);
        assert!(tokens > 20, "expected > 20 tokens for 2 messages, got {tokens}");
    }

    // ── Message formatting ────────────────────────────────────────

    #[test]
    fn message_to_text_user() {
        let msg = AgentMessage::user("hello world");
        let text = message_to_text(&msg);
        assert_eq!(text, "User: hello world");
    }

    #[test]
    fn message_to_text_tool_call() {
        let msg = AgentMessage::assistant(vec![ContentBlock::tool_call(
            ToolCallId::new(),
            "bash",
            serde_json::json!({"cmd": "ls"}),
        )]);
        let text = message_to_text(&msg);
        assert!(text.starts_with("Assistant: [tool_call: bash("));
    }

    #[test]
    fn message_to_text_image() {
        let msg = AgentMessage::assistant(vec![ContentBlock::Image {
            data: vec![0xFF],
            mime_type: "image/png".to_string(),
        }]);
        let text = message_to_text(&msg);
        assert_eq!(text, "Assistant: [image: image/png]");
    }

    // ── Compaction behavior ───────────────────────────────────────

    #[tokio::test]
    async fn no_compaction_under_threshold() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 100_000,
            max_output_tokens: 1_000,
            trigger_ratio: 0.8,
            keep_recent_pairs: 4,
        });

        let messages = vec![
            AgentMessage::user("hello"),
            AgentMessage::assistant(vec![ContentBlock::text("hi there")]),
        ];

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(!result.was_compacted);
        assert_eq!(result.messages.len(), 2);
    }

    #[tokio::test]
    async fn no_compaction_empty_messages() {
        let cwm = ContextWindowManager::new(ContextConfig::default());
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let result = cwm.compact(&[], &provider, "test-model").await.unwrap();

        assert!(!result.was_compacted);
        assert_eq!(result.messages.len(), 0);
    }

    #[tokio::test]
    async fn no_compaction_single_message() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 100_000,
            max_output_tokens: 1_000,
            ..ContextConfig::default()
        });
        let messages = vec![AgentMessage::user("hi")];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(!result.was_compacted);
        assert_eq!(result.messages.len(), 1);
    }

    #[tokio::test]
    async fn compaction_preserves_system_messages() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            trigger_ratio: 0.8,
            keep_recent_pairs: 1,
        });

        let mut messages = vec![AgentMessage::system("You are a helpful assistant")];
        // Add many old exchanges to push us over budget
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("old question {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(
                format!("old answer {i}"),
            )]));
        }
        // Recent exchange
        messages.push(AgentMessage::user("new question"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("new answer")]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(
                "Summary of old conversation".to_string(),
            )]));

        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
        // First message should be system
        assert_eq!(result.messages[0].role, Role::System);
    }

    #[tokio::test]
    async fn compaction_preserves_recent_pairs() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            trigger_ratio: 0.8,
            keep_recent_pairs: 2,
        });

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("question {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(
                format!("answer {i}"),
            )]));
        }

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(
                "Summary".to_string(),
            )]));

        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
        // summary + 2 recent pairs (2 user + 2 assistant = 4 messages) = 5
        assert_eq!(result.messages.len(), 5);
        // First is the summary (User role)
        assert_eq!(result.messages[0].role, Role::User);
        // Last four are the recent exchanges
        assert_eq!(result.messages[1].role, Role::User);
        assert_eq!(result.messages[2].role, Role::Assistant);
        assert_eq!(result.messages[3].role, Role::User);
        assert_eq!(result.messages[4].role, Role::Assistant);
    }

    #[tokio::test]
    async fn compaction_keeps_tool_pairs_together() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            trigger_ratio: 0.8,
            keep_recent_pairs: 1,
        });

        let tc_id = ToolCallId::new();
        let mut messages = Vec::new();
        // Old exchanges to push over budget
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("old {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(
                format!("old answer {i}"),
            )]));
        }
        // Recent: user → assistant(tool_call) → tool_result → assistant(text)
        messages.push(AgentMessage::user("use the tool"));
        messages.push(AgentMessage::assistant(vec![
            ContentBlock::text("let me check"),
            ContentBlock::tool_call(tc_id.clone(), "bash", serde_json::json!({"cmd": "ls"})),
        ]));
        messages.push(AgentMessage::tool_result(tc_id, "file1.txt\nfile2.txt", false));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text(
            "I found two files",
        )]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(
                "Summary of old stuff".to_string(),
            )]));

        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
        // The assistant(tool_call) + tool_result pair must not be split
        let has_tool_call = result.messages.iter().any(|m| {
            m.role == Role::Assistant
                && m.content.iter().any(|c| c.is_tool_call())
        });
        let has_tool_result = result.messages.iter().any(|m| m.role == Role::Tool);
        // If tool call is present, tool result must also be present
        if has_tool_call {
            assert!(
                has_tool_result,
                "tool_call without tool_result — pair was split!"
            );
        }
    }

    #[tokio::test]
    async fn compaction_generates_summary_via_provider() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            trigger_ratio: 0.8,
            keep_recent_pairs: 1,
        });

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("question {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(
                format!("answer {i}"),
            )]));
        }
        messages.push(AgentMessage::user("latest"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("response")]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(
                "The user asked many questions and got many answers.".to_string(),
            )]));

        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
        // Summary is the first message
        match &result.messages[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(
                    text.contains("[Earlier conversation summary]"),
                    "expected summary marker, got: {text}"
                );
                assert!(
                    text.contains("many questions"),
                    "expected summary content, got: {text}"
                );
                assert!(
                    text.contains("[End of summary"),
                    "expected end marker, got: {text}"
                );
            }
            _ => panic!("expected text content in summary"),
        }
    }

    #[tokio::test]
    async fn compaction_reduces_token_count() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            trigger_ratio: 0.8,
            keep_recent_pairs: 1,
        });

        let mut messages = Vec::new();
        for _i in 0..20 {
            messages.push(AgentMessage::user("word ".repeat(100)));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(
                "reply ".repeat(100),
            )]));
        }
        messages.push(AgentMessage::user("final"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("done")]));

        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(
                "Brief summary".to_string(),
            )]));

        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        assert!(result.was_compacted);
        assert!(result.tokens_after < result.tokens_before);
        assert!(result.messages_after < result.messages_before);
    }

    #[tokio::test]
    async fn compaction_few_user_messages_than_pairs() {
        // When there are fewer User messages than keep_recent_pairs,
        // no compaction should happen (everything is "recent").
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 1_000,
            max_output_tokens: 100,
            trigger_ratio: 0.8,
            keep_recent_pairs: 10,
        });

        let messages = vec![
            AgentMessage::user("only one question"),
            AgentMessage::assistant(vec![ContentBlock::text("only one answer")]),
        ];

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let result = cwm.compact(&messages, &provider, "test-model").await.unwrap();

        // Can't compact: all messages are "recent" (fewer pairs than keep_recent_pairs)
        assert!(!result.was_compacted);
        assert_eq!(result.messages.len(), 2);
    }

    #[test]
    fn default_config_values() {
        let config = ContextConfig::default();
        assert_eq!(config.context_window, 128_000);
        assert_eq!(config.max_output_tokens, 8_192);
        assert!((config.trigger_ratio - 0.8).abs() < f64::EPSILON);
        assert_eq!(config.keep_recent_pairs, 4);
    }

    #[test]
    fn available_budget_calculation() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 100_000,
            max_output_tokens: 10_000,
            trigger_ratio: 0.8,
            keep_recent_pairs: 4,
        });
        // (100_000 - 10_000) * 0.8 = 72_000
        assert_eq!(cwm.available_budget(), 72_000);
    }

    #[test]
    fn find_recent_start_with_tool_chain() {
        let cwm = ContextWindowManager::new(ContextConfig {
            context_window: 128_000,
            max_output_tokens: 8_192,
            trigger_ratio: 0.8,
            keep_recent_pairs: 1,
        });

        let tc_id = ToolCallId::new();
        let messages = vec![
            AgentMessage::system("sys"),                  // 0
            AgentMessage::user("old q"),                  // 1
            AgentMessage::assistant(vec![ContentBlock::text("old a")]), // 2
            AgentMessage::user("use tool"),                // 3
            AgentMessage::assistant(vec![                 // 4
                ContentBlock::tool_call(tc_id.clone(), "bash", serde_json::json!({})),
            ]),
            AgentMessage::tool_result(tc_id, "ok", false), // 5
            AgentMessage::assistant(vec![ContentBlock::text("done")]), // 6
        ];

        let start = cwm.find_recent_start(&messages);
        // keep_recent_pairs=1, so we look for 1 User message from the end.
        // Scanning back: messages[3] is User → recent_start = 3.
        // Boundary adjustment: messages[2] is Assistant without tool_calls → stop.
        assert_eq!(start, 3);
    }

    #[test]
    fn find_recent_start_preserves_tool_boundary() {
        let cwm = ContextWindowManager::new(ContextConfig {
            keep_recent_pairs: 1,
            ..ContextConfig::default()
        });

        let tc_id = ToolCallId::new();
        // Scenario: recent_start lands on a Tool message
        let messages = vec![
            AgentMessage::user("old"),                     // 0
            AgentMessage::assistant(vec![ContentBlock::text("old a")]), // 1
            AgentMessage::user("tool please"),             // 2
            AgentMessage::assistant(vec![                 // 3
                ContentBlock::tool_call(tc_id.clone(), "bash", serde_json::json!({})),
            ]),
            AgentMessage::tool_result(tc_id, "ok", false), // 4
            AgentMessage::assistant(vec![ContentBlock::text("done")]), // 5
        ];

        let start = cwm.find_recent_start(&messages);
        // Scanning back: messages[2] is User → recent_start = 2.
        // Boundary adjustment: messages[1] is Assistant without tool_calls → stop.
        // recent_start = 2, which starts at User("tool please").
        // The full chain [2,3,4,5] is kept together.
        assert_eq!(start, 2);
    }

    #[test]
    fn prepare_compaction_works() {
        let messages = vec![
            AgentMessage::system("be helpful"),
            AgentMessage::user("hi"),
            AgentMessage::assistant(vec![ContentBlock::text("hello")]),
        ];
        let (system, old_text) = prepare_compaction(&messages);
        assert_eq!(system.len(), 1);
        assert!(old_text.contains("User: hi"));
        assert!(old_text.contains("Assistant: hello"));
    }
}
