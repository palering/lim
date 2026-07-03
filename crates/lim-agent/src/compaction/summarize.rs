//! Default compaction strategy: LLM summarization of older messages.
//!
//! Splits the history into "old" (summarized) and "recent" (kept verbatim)
//! at the configured `keep_recent_pairs` boundary, then makes a hidden LLM
//! call to produce a summary. If the call fails or returns empty, falls
//! back to keeping the old messages verbatim (losing context is worse than
//! exceeding the budget).
//!
//! ## Provider / model overrides
//!
//! By default, the strategy uses the same provider + model as the main
//! agent loop. To save cost, configure a smaller / cheaper model:
//!
//! ```ignore
//! let strategy = SummarizeStrategy::default()
//!     .with_provider(small_provider)
//!     .with_model("gpt-4o-mini");
//! ```

use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, warn};

use lim_core::content::ContentBlock;
use lim_core::error::Result;
use lim_core::message::{AgentMessage, LlmMessage, Role, convert_to_llm};

use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

use crate::compaction::{
    CompactRequest, CompactionStrategy, StructuredSummary, extract_file_hints,
};
use crate::context_window::{CompactionResult, CompactionTriggerReason, ContextConfig};
use crate::context_window::{estimate_llm_tokens, estimate_tokens, find_recent_start};

/// Fixed instruction for the structured-summary LLM call.
///
/// This text is `const` so it is identical across compactions and gets
/// included in the prompt cache prefix (alongside the reused system prompt).
/// Only the dynamic `<conversation>` body varies between calls.
const SUMMARY_INSTRUCTION: &str = "You are compacting an AI agent's conversation history.\n\
Output a STRICT JSON object matching this schema. No prose, no markdown fences.\n\n\
{\n  \
\"goal\": \"The user's overarching goal for this session (1-2 sentences, or null)\",\n  \
\"constraints\": [\"Hard constraints: tech stack, conventions, invariants\"],\n  \
\"done\": [\"Completed work, past tense, time order\"],\n  \
\"in_progress\": [\"Work currently underway in the most recent turn\"],\n  \
\"blocked\": [\"Items blocked on external input / failing CI / unresolved errors\"],\n  \
\"key_decisions\": [\"Why option A was chosen over B, with rationale\"],\n  \
\"next_steps\": [\"Concrete next actions for the upcoming turn\"],\n  \
\"critical_context\": [\"Token counts, config values, error stacks, IDs that must survive\"]\n\
}\n\n\
Rules:\n\
- Omit fields you cannot fill (they default to null/empty).\n\
- Be concrete: \"fixed NPE in auth.rs:142\" not \"fixed a bug\".\n\
- read_files / modified_files are filled by the runtime, DO NOT output them.\n\
- Output ONLY the JSON object. No explanation, no code fences.";

/// Default compaction strategy: summarize old messages via an LLM call,
/// keep recent N conversation pairs verbatim.
///
/// See [module docs](self) for the provider/model override API.
#[derive(Default)]
pub struct SummarizeStrategy {
    /// Optional secondary provider used for the summarization LLM call.
    /// `None` = use `main_provider` from the request.
    summarize_provider: Option<Arc<dyn Provider>>,
    /// Optional model name for the summarization call.
    /// `None` = use `main_model` from the request.
    summarize_model: Option<String>,
}

impl SummarizeStrategy {
    /// Use a different (e.g. cheaper / faster) provider for summarization.
    ///
    /// The provider must already be configured (e.g. via `.lim/config.toml`).
    /// The caller is responsible for lifecycle — the strategy holds an
    /// `Arc` clone.
    pub fn with_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.summarize_provider = Some(provider);
        self
    }

    /// Use a different model name for summarization (e.g. "gpt-4o-mini").
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.summarize_model = Some(model.into());
        self
    }
}

impl CompactionStrategy for SummarizeStrategy {
    fn compact<'a>(
        &'a self,
        req: CompactRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CompactionResult>> + Send + 'a>>
    {
        Box::pin(async move {
            summarize_compact(
                req.messages,
                req.config,
                req.main_provider,
                req.main_model,
                self.summarize_provider.as_ref(),
                self.summarize_model.as_deref(),
            )
            .await
        })
    }
}

/// Core implementation, decoupled from the trait so it can be unit-tested
/// without going through `Box::pin`.
async fn summarize_compact(
    messages: &[AgentMessage],
    config: &ContextConfig,
    main_provider: &Arc<dyn Provider>,
    main_model: &str,
    override_provider: Option<&Arc<dyn Provider>>,
    override_model: Option<&str>,
) -> Result<CompactionResult> {
    let tokens_before = estimate_tokens(messages);

    // Find the split point: system + old | recent (kept verbatim).
    let recent_start = find_recent_start(messages, config.keep_recent_pairs);

    if recent_start == 0 {
        // All messages are in the recent window — can't compact further.
        let llm_messages = convert_to_llm(messages);
        return Ok(CompactionResult {
            messages_after: messages.len(),
            messages_before: messages.len(),
            tokens_after: tokens_before,
            tokens_before,
            messages: llm_messages,
            was_compacted: false,
            summary_model: String::new(),
            summary_prompt_tokens: 0,
            summary_completion_tokens: 0,
            summary: None,
            trigger_reason: CompactionTriggerReason::AutoThreshold,
        });
    }

    let (system_msgs, old_text) = prepare_compaction(&messages[..recent_start]);

    // Pick the provider/model for the summarization call.
    let provider: &Arc<dyn Provider> = override_provider.unwrap_or(main_provider);
    let model: &str = override_model.unwrap_or(main_model);

    // Cache-safe forking: extract the system prompt text from system messages
    // so the summarization request shares the same prefix as the main
    // conversation. This maximizes prompt cache hits.
    let system_prompt_text: Option<String> = if system_msgs.is_empty() {
        None
    } else {
        let text: Vec<&str> = system_msgs
            .iter()
            .filter_map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        lim_core::content::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .next()
            })
            .collect();
        let joined = text.join("\n\n");
        if joined.is_empty() { None } else { Some(joined) }
    };

    // Generate summary of old messages (if any non-system messages exist).
    // On failure / empty response, fall back to verbatim old messages
    // (losing context is worse than exceeding the budget).
    //
    // Phase 1: try to parse LLM output as a `StructuredSummary`. If it
    // parses, fill the runtime-only `read_files` / `modified_files` from
    // local extraction and use `render_for_projection()` as the inserted
    // message text (cache-stable). If it doesn't parse, fall back to the
    // raw LLM text as a free-text summary (existing behavior).
    let file_hints = extract_file_hints(&messages[..recent_start]);

    let (summary_text, structured_summary, summary_usage) = if old_text.is_empty() {
        (String::new(), None, lim_core::usage::Usage::default())
    } else {
        match summarize(&old_text, Arc::clone(provider), model, system_prompt_text.as_deref(), &[])
            .await
        {
            Ok((raw_text, usage)) if !raw_text.trim().is_empty() => {
                match StructuredSummary::parse_from_llm_output(&raw_text) {
                    Some(mut s) if !s.is_llm_output_empty() => {
                        s.read_files = file_hints.read_files;
                        s.modified_files = file_hints.modified_files;
                        let rendered = s.render_for_projection();
                        (rendered, Some(s), usage)
                    }
                    _ => {
                        warn!(
                            prompt_tokens = usage.input_tokens,
                            completion_tokens = usage.output_tokens,
                            "summary LLM output not parseable as StructuredSummary; using free-text fallback"
                        );
                        (raw_text, None, usage)
                    }
                }
            }
            Ok((_, usage)) => {
                warn!(
                    prompt_tokens = usage.input_tokens,
                    completion_tokens = usage.output_tokens,
                    "summary provider returned empty response; falling back to verbatim old messages"
                );
                (String::new(), None, usage)
            }
            Err(e) => {
                warn!(error = %e, "summary provider failed; falling back to verbatim old messages");
                (String::new(), None, lim_core::usage::Usage::default())
            }
        }
    };

    // If summary failed, keep old messages verbatim instead of dropping them.
    let effective_recent_start =
        if summary_text.is_empty() && !old_text.is_empty() { 0 } else { recent_start };

    // Build the compacted message list.
    let mut llm_messages: Vec<LlmMessage> = Vec::new();

    // 1. System messages first.
    for msg in system_msgs {
        if let Some(llm_msg) = LlmMessage::from_agent_message(msg) {
            llm_messages.push(llm_msg);
        }
    }

    // 2. Summary as a User-role message.
    if !summary_text.is_empty() {
        llm_messages.push(LlmMessage {
            role: Role::User,
            content: vec![ContentBlock::text(format!(
                "[Earlier conversation summary]\n{summary_text}\n[End of summary — conversation continues below]"
            ))],
            tool_call_ids: vec![],
        });
    }

    // 3. Recent messages verbatim (or all messages if summary fell back).
    for msg in &messages[effective_recent_start..] {
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
        "context compacted via SummarizeStrategy"
    );

    Ok(CompactionResult {
        was_compacted: true,
        messages_before: messages.len(),
        messages_after: llm_messages.len(),
        tokens_before,
        tokens_after,
        messages: llm_messages,
        // Even when summary falls back, populate the model so the JSONL
        // reader knows which model was attempted. Token counts come from
        // the provider's usage report (zeros on error).
        summary_model: model.to_string(),
        summary_prompt_tokens: summary_usage.input_tokens,
        summary_completion_tokens: summary_usage.output_tokens,
        summary: structured_summary,
        trigger_reason: CompactionTriggerReason::AutoThreshold,
    })
}

/// Separate system messages from old messages and build summary text.
///
/// Returns `(system_messages, formatted_conversation_text)`.
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
            ContentBlock::Image { mime_type, .. } => format!("[image: {mime_type}]"),
            ContentBlock::ArtifactRef { artifact_id, .. } => format!("[artifact: {artifact_id}]"),
        })
        .collect();
    format!("{role}: {}", content.join(" "))
}

/// Generate a summary of older messages using the provider.
///
/// **Cache-safe forking** (Claude Code pattern): the summarization request
/// reuses the same system prompt and tool definitions as the main
/// conversation. This maximizes prompt cache hits — from the provider's
/// perspective, the request looks like the previous turn with an extra
/// compaction message appended, so the cached prefix is reused and only
/// the new compaction instruction is billed as fresh input tokens.
///
/// **Note**: this makes a hidden LLM call that does NOT emit TurnStart /
/// MessageStart / MessageEnd events. The `debug!` log + the `summary_*`
/// fields on the subsequent `ContextCompacted` event are the observable
/// trace. See docs/architecture/behavior-trace.md §8.1.
async fn summarize(
    conversation_text: &str,
    provider: Arc<dyn Provider>,
    model: &str,
    // System prompt from the main conversation (for cache hit).
    system_prompt: Option<&str>,
    // Tool schemas from the main conversation (for cache hit).
    tools: &[lim_core::tool::ToolSchema],
) -> Result<(String, lim_core::usage::Usage)> {
    let summary_prompt =
        format!("{SUMMARY_INSTRUCTION}\n\n<conversation>\n{conversation_text}\n</conversation>");

    let llm_msg = LlmMessage {
        role: Role::User,
        content: vec![ContentBlock::text(summary_prompt)],
        tool_call_ids: vec![],
    };

    let mut request = LlmRequest::new(model, vec![llm_msg]);
    // Cache-safe forking: reuse the main conversation's system prompt and
    // tools so the provider can hit the prompt cache prefix.
    if let Some(sys) = system_prompt {
        request = request.with_system(sys);
    }
    if !tools.is_empty() {
        request = request.with_tools(tools.to_vec());
    }

    debug!(
        model = model,
        conversation_chars = conversation_text.len(),
        has_system = system_prompt.is_some(),
        tool_count = tools.len(),
        "context compaction: starting cache-safe LLM summary call"
    );
    let stream = provider.stream(request);
    let events: Vec<_> = collect_stream(stream).await;

    let mut summary = String::new();
    let mut had_error = false;
    let mut usage = lim_core::usage::Usage::default();
    for event in events {
        match event {
            Ok(StreamEvent::TextDelta { text }) => summary.push_str(&text),
            Ok(StreamEvent::Usage(u)) => usage = u,
            Ok(_) => {}
            Err(e) => {
                had_error = true;
                warn!(error = %e, "context compaction: stream error during summary");
            }
        }
    }

    if had_error && summary.is_empty() {
        warn!("context compaction: summary empty due to stream errors");
    }

    Ok((summary, usage))
}

/// Collect a BoxStream of results into a Vec.
async fn collect_stream<'a>(
    mut stream: futures::stream::BoxStream<
        'a,
        std::result::Result<StreamEvent, lim_llm::ProviderError>,
    >,
) -> Vec<std::result::Result<StreamEvent, lim_llm::ProviderError>> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::id::ToolCallId;
    use lim_llm::mock::{MockProvider, MockResponse};

    #[test]
    fn message_to_text_user() {
        let msg = AgentMessage::user("hello world");
        assert_eq!(message_to_text(&msg), "User: hello world");
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

    #[tokio::test]
    async fn summarize_strategy_compacts_when_over_budget() {
        let cwm_config = ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            compact_buffer_tokens: 0,
            keep_recent_pairs: 1,
        };
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![MockResponse::Text(
            "Summary of old conversation".to_string(),
        )]));

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("old question {i}")));
            messages
                .push(AgentMessage::assistant(vec![ContentBlock::text(format!("old answer {i}"))]));
        }
        messages.push(AgentMessage::user("new question"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("new answer")]));

        let req = CompactRequest {
            messages: &messages,
            config: &cwm_config,
            main_provider: &provider,
            main_model: "test-model",
            observed_input_tokens: None,
        };

        let strategy = SummarizeStrategy::default();
        let result = strategy.compact(req).await.unwrap();

        assert!(result.was_compacted);
        assert!(result.messages_after < result.messages_before);
        assert_eq!(result.summary_model, "test-model");
    }

    #[tokio::test]
    async fn summarize_strategy_falls_back_when_all_recent() {
        // Only 1 user message — fewer than keep_recent_pairs=4 — everything
        // is "recent", no compaction possible.
        let cwm_config = ContextConfig::default();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));

        let messages = vec![
            AgentMessage::user("only question"),
            AgentMessage::assistant(vec![ContentBlock::text("only answer")]),
        ];

        let req = CompactRequest {
            messages: &messages,
            config: &cwm_config,
            main_provider: &provider,
            main_model: "test-model",
            observed_input_tokens: None,
        };

        let strategy = SummarizeStrategy::default();
        let result = strategy.compact(req).await.unwrap();

        assert!(!result.was_compacted);
        assert_eq!(result.messages.len(), 2);
    }

    #[tokio::test]
    async fn summarize_strategy_uses_override_model() {
        let cwm_config = ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            compact_buffer_tokens: 0,
            keep_recent_pairs: 1,
        };
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("summary".to_string())]));

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("q {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(format!("a {i}"))]));
        }
        messages.push(AgentMessage::user("latest"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("resp")]));

        let req = CompactRequest {
            messages: &messages,
            config: &cwm_config,
            main_provider: &provider,
            main_model: "gpt-4o",
            observed_input_tokens: None,
        };

        let strategy = SummarizeStrategy::default().with_model("gpt-4o-mini");
        let result = strategy.compact(req).await.unwrap();

        assert!(result.was_compacted);
        // The override model must be reflected in the result, not the main one.
        assert_eq!(result.summary_model, "gpt-4o-mini");
    }

    /// Phase 1: when the LLM returns a valid JSON `StructuredSummary`, the
    /// strategy should parse it, fill `read_files`/`modified_files` from
    /// local extraction, and use `render_for_projection()` as the inserted
    /// message text. The `CompactionResult.summary` field should be `Some`.
    #[tokio::test]
    async fn summarize_strategy_parses_structured_summary_phase1() {
        let cwm_config = ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            compact_buffer_tokens: 0,
            keep_recent_pairs: 1,
        };
        // LLM returns a valid JSON summary.
        let llm_json = r#"{"goal":"Fix bug in auth","done":["added field","wrote tests"],"next_steps":["run cargo test"]}"#;
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text(llm_json.to_string())]));

        // Build messages that include tool calls in the old region so
        // file_hints extraction has something to find.
        let mut messages = Vec::new();
        messages.push(AgentMessage::user("old question"));
        messages.push(AgentMessage::assistant(vec![
            ContentBlock::text("let me read the file"),
            ContentBlock::tool_call(
                ToolCallId::new(),
                "read",
                serde_json::json!({"path":"src/main.rs"}),
            ),
        ]));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("now editing")]));
        messages.push(AgentMessage::assistant(vec![ContentBlock::tool_call(
            ToolCallId::new(),
            "edit",
            serde_json::json!({"path":"src/lib.rs"}),
        )]));
        // Pad to exceed budget.
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("q {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(format!("a {i}"))]));
        }
        messages.push(AgentMessage::user("latest"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("resp")]));

        let req = CompactRequest {
            messages: &messages,
            config: &cwm_config,
            main_provider: &provider,
            main_model: "test-model",
            observed_input_tokens: None,
        };

        let strategy = SummarizeStrategy::default();
        let result = strategy.compact(req).await.unwrap();

        assert!(result.was_compacted);
        let summary = result.summary.expect("Phase 1 should produce a StructuredSummary");
        assert_eq!(summary.goal, Some("Fix bug in auth".to_string()));
        assert_eq!(summary.done, vec!["added field".to_string(), "wrote tests".to_string()]);
        assert_eq!(summary.next_steps, vec!["run cargo test".to_string()]);
        // Runtime-filled fields from local extraction.
        assert!(summary.read_files.contains(&"src/main.rs".to_string()));
        assert!(summary.modified_files.contains(&"src/lib.rs".to_string()));

        // The inserted summary message should use render_for_projection()
        // (contains the "[Earlier conversation summary]" marker).
        let summary_msg = result
            .messages
            .iter()
            .find(|m| {
                m.role == Role::User
                    && m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("[Earlier conversation summary]")))
            })
            .expect("should find a summary message");
        let text = summary_msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("Goal: Fix bug in auth"));
        assert!(text.contains("Files Read:"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("Files Modified:"));
        assert!(text.contains("src/lib.rs"));
    }

    /// Phase 1: when the LLM returns unparseable prose (not JSON), the
    /// strategy should fall back to using the raw text as the summary
    /// message and `CompactionResult.summary` should be `None`.
    #[tokio::test]
    async fn summarize_strategy_falls_back_to_free_text_when_unparseable() {
        let cwm_config = ContextConfig {
            context_window: 400,
            max_output_tokens: 50,
            compact_buffer_tokens: 0,
            keep_recent_pairs: 1,
        };
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![MockResponse::Text(
            "Just plain prose summary, no JSON here.".to_string(),
        )]));

        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(AgentMessage::user(format!("q {i}")));
            messages.push(AgentMessage::assistant(vec![ContentBlock::text(format!("a {i}"))]));
        }
        messages.push(AgentMessage::user("latest"));
        messages.push(AgentMessage::assistant(vec![ContentBlock::text("resp")]));

        let req = CompactRequest {
            messages: &messages,
            config: &cwm_config,
            main_provider: &provider,
            main_model: "test-model",
            observed_input_tokens: None,
        };

        let strategy = SummarizeStrategy::default();
        let result = strategy.compact(req).await.unwrap();

        assert!(result.was_compacted);
        assert!(result.summary.is_none(), "free-text fallback should set summary to None");
        // The raw prose should still appear as the summary message.
        let has_summary_msg = result.messages.iter().any(|m| {
            m.role == Role::User
                && m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("Just plain prose")))
        });
        assert!(has_summary_msg, "free-text summary should be inserted as a User message");
    }
}
