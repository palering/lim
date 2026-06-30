use std::sync::Arc;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::{AgentConfig, AgentLoop};
use lim_agent::registry::ToolRegistry;
use lim_core::content::ContentBlock;
use lim_core::context_edit::ContextEdit;
use lim_core::error::Result;
use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::message::AgentMessage;
use lim_executor::local::LocalExecutor;
use lim_llm::mock::{MockProvider, MockResponse, MockToolCall};
use lim_llm::providers::{
    create_provider, LimConfig, ProviderType,
};
use lim_llm::Provider;
use lim_session::replay::replay_transcript;
use lim_session::writer::SessionWriter;

/// Lim — a Rust-native agent runtime.
#[derive(Parser)]
#[command(name = "lim", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an agent session (non-interactive, single message).
    Run {
        /// Named provider instance from `.lim/config.toml` (e.g. `openai-default`).
        ///
        /// If omitted, uses `default_provider` from config. Ignored when `--fake` is used.
        #[arg(long)]
        provider: Option<String>,

        /// Model name to request. Overrides the provider's `default_model`.
        ///
        /// Supports aliases defined in `[providers.<name>.aliases]`.
        #[arg(long)]
        model: Option<String>,

        /// Path to a fixture JSON file for MockProvider (no real LLM calls).
        ///
        /// When set, `--provider` and `--model` are ignored.
        #[arg(long)]
        fake: Option<Utf8PathBuf>,

        /// Initial user message.
        #[arg(long, default_value = "Hello")]
        message: String,

        /// Path to write the JSONL event log (defaults to sessions/<timestamp>.jsonl).
        #[arg(long)]
        output: Option<Utf8PathBuf>,

        /// Resume from a prior session JSONL file.
        ///
        /// When set, all messages from the prior session are loaded into
        /// context, and any `ContextEdited` events are replayed via
        /// `AgentLoop::with_user_edits`. The new message is appended as
        /// the next user turn. May be combined with `--fake` to script
        /// the new turn's assistant responses.
        #[arg(long)]
        resume: Option<Utf8PathBuf>,

        /// Trust level: `workspace` (default, restricts file/command access
        /// to the project dir) or `permissive` (allows everything, dangerous).
        ///
        /// See docs/draft/trust-and-approval.md for the full trust model.
        #[arg(long, default_value = "workspace")]
        trust: TrustArg,
    },

    /// Launch the interactive TUI (terminal user interface).
    ///
    /// Starts an interactive session with streaming output, tool call display,
    /// and slash commands. The agent runs in the background; events are
    /// rendered in real-time.
    Tui {
        /// Named provider instance from `.lim/config.toml`.
        #[arg(long)]
        provider: Option<String>,

        /// Model name to request. Overrides the provider's `default_model`.
        #[arg(long)]
        model: Option<String>,

        /// Trust level: `workspace` (default) or `permissive`.
        #[arg(long, default_value = "workspace")]
        trust: TrustArg,
    },

    /// List configured providers from `.lim/config.toml`.
    Providers,

    /// Edit the context of a saved session (appends a `ContextEdited` event).
    ///
    /// Reads the session JSONL, applies the requested edits, and appends a
    /// single `ContextEdited` event describing the modification. The next
    /// `lim run --resume <session>` will pick up the edit via `replay_edits`.
    EditContext {
        /// Path to the session JSONL file.
        #[arg(long)]
        session: Utf8PathBuf,

        /// Comma-separated message indices to exclude from future LLM calls.
        /// e.g. `--exclude 1,3,5`
        #[arg(long)]
        exclude: Option<String>,

        /// Replace a message's text content: `INDEX:NEW_TEXT`.
        /// Can be specified multiple times.
        /// e.g. `--replace 2:[summarized]`
        #[arg(long = "replace")]
        replace: Vec<String>,

        /// List messages with indices (no edit applied).
        #[arg(long)]
        list: bool,
    },
}

/// Fixture format for MockProvider scripted responses.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum FixtureEntry {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_calls")]
    ToolCalls {
        text: Option<String>,
        calls: Vec<FixtureToolCall>,
    },
}

/// CLI trust level argument.
///
/// - `workspace` (default): restrict file access to the project directory,
///   block dangerous commands, reject cross-workspace `git -C`.
/// - `permissive`: allow everything (dangerous, for testing only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TrustArg {
    #[value(name = "workspace")]
    Workspace,
    #[value(name = "permissive")]
    Permissive,
}

impl TrustArg {
    fn is_permissive(self) -> bool {
        matches!(self, Self::Permissive)
    }
}

/// Resolve workspace roots: git root if available, else current dir.
///
/// See docs/draft/trust-and-approval.md "workspace_roots resolution".
fn resolve_workspace_roots() -> Utf8PathBuf {
    // Try git root first
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Ok(out) = output
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let trimmed = s.trim();
        if !trimmed.is_empty()
            && let Ok(p) = Utf8PathBuf::from_path_buf(std::path::PathBuf::from(trimmed))
        {
            return p;
        }
    }
    // Fallback to current dir
    Utf8PathBuf::from(".")
}

/// Build a `LocalExecutor` based on the trust argument.
///
/// `workspace`: restrict to resolved workspace roots.
/// `permissive`: allow everything, print a warning to stderr.
fn build_executor(trust: TrustArg) -> Arc<LocalExecutor> {
    if trust.is_permissive() {
        eprintln!("⚠ WARNING: running in permissive trust mode — no file/command restrictions.");
        eprintln!("  Use `--trust workspace` (default) for production.");
        #[allow(deprecated)]
        Arc::new(LocalExecutor::permissive())
    } else {
        let root = resolve_workspace_roots();
        eprintln!("Trust: workspace (root: {root})");
        Arc::new(LocalExecutor::workspace(vec![root]))
    }
}

#[derive(Debug, Deserialize)]
struct FixtureToolCall {
    name: String,
    arguments: serde_json::Value,
}

fn load_fixture(path: &Utf8PathBuf) -> Result<Vec<MockResponse>> {
    let content = std::fs::read_to_string(path.as_std_path()).map_err(|e| {
        lim_core::error::LimError::ToolExecution {
            name: "fixture_loader".to_string(),
            message: format!("failed to read fixture `{path}`: {e}"),
        }
    })?;

    let entries: Vec<FixtureEntry> = serde_json::from_str(&content).map_err(|e| {
        lim_core::error::LimError::ToolExecution {
            name: "fixture_loader".to_string(),
            message: format!("failed to parse fixture `{path}`: {e}"),
        }
    })?;

    let responses: Vec<MockResponse> = entries
        .into_iter()
        .map(|entry| match entry {
            FixtureEntry::Text { text } => MockResponse::Text(text),
            FixtureEntry::ToolCalls { text, calls } => MockResponse::ToolCalls {
                text,
                calls: calls
                    .into_iter()
                    .map(|c| MockToolCall::new(c.name, c.arguments))
                    .collect(),
            },
        })
        .collect();

    Ok(responses)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            provider,
            model,
            fake,
            message,
            output,
            resume,
            trust,
        } => {
            run_session(provider, model, fake, resume, message, output, trust).await?;
        }
        Commands::Tui {
            provider,
            model,
            trust,
        } => {
            run_tui(provider, model, trust).await?;
        }
        Commands::Providers => {
            list_providers();
        }
        Commands::EditContext {
            session,
            exclude,
            replace,
            list,
        } => {
            edit_context(session, exclude, replace, list).await?;
        }
    }

    Ok(())
}

fn list_providers() {
    let config = LimConfig::load_from_default_path();
    if config.providers.is_empty() {
        println!("No providers configured.");
        println!("Create `.lim/config.toml` to add a provider. Example:");
        println!();
        println!("  default_provider = \"openai-default\"");
        println!();
        println!("  [providers.openai-default]");
        println!("  type = \"openai-responses\"");
        println!("  api_key = \"sk-...\"");
        println!("  default_model = \"gpt-4o\"");
        return;
    }

    let default = config.default_provider.as_deref().unwrap_or("(none)");
    println!("default_provider: {default}");
    println!();
    println!("{:<20} {:<18} {:<48} DEFAULT_MODEL", "NAME", "TYPE", "ENDPOINT");
    for (name, instance) in &config.providers {
        let type_str = type_short(&instance.r#type);
        // Show full_url if set (bypasses base_url + suffix), else base_url.
        let endpoint = instance
            .full_url
            .as_deref()
            .unwrap_or_else(|| instance.resolve_base_url());
        // Truncate long endpoints for display
        let endpoint_display: String = if endpoint.len() > 46 {
            format!("{}...", &endpoint[..43])
        } else {
            endpoint.to_string()
        };
        let model = instance.default_model.as_deref().unwrap_or("(none)");
        let marker = if Some(name.as_str()) == config.default_provider.as_deref() {
            " *"
        } else {
            ""
        };
        println!(
            "{:<20} {:<18} {:<48} {}{}",
            name,
            type_str,
            endpoint_display,
            model,
            marker,
        );
    }
}

fn type_short(t: &ProviderType) -> &'static str {
    match t {
        ProviderType::OpenAIChat => "openai-chat",
        ProviderType::OpenAIResponses => "openai-responses",
        ProviderType::Anthropic => "anthropic",
        ProviderType::Mock => "mock",
    }
}

async fn run_session(
    cli_provider: Option<String>,
    cli_model: Option<String>,
    fake: Option<Utf8PathBuf>,
    resume: Option<Utf8PathBuf>,
    user_message: String,
    output_path: Option<Utf8PathBuf>,
    trust: TrustArg,
) -> Result<()> {
    // If resuming, load prior session events and replay messages + edits.
    // prior_messages will be prepended to the new turn's context.
    // user_edits will be re-applied on every turn (persistent edits).
    let (prior_messages, user_edits, resume_label) = match resume.as_ref() {
        Some(path) => {
            let events = lim_session::reader::SessionReader::read_all(path).await?;
            let messages = replay_transcript(&events);
            let edits = lim_session::replay::replay_edits(&events);
            let label = format!("{} ({} prior messages, {} edits)",
                path, messages.len(), edits.len());
            (messages, edits, Some(label))
        }
        None => (Vec::new(), Vec::new(), None),
    };

    // Resolve provider + model + create provider
    let (provider, model_name, provider_label) = match fake {
        Some(fixture_path) => {
            let responses = load_fixture(&fixture_path)?;
            println!("Loaded {} scripted responses from {fixture_path}", responses.len());
            let p: Arc<dyn Provider> = Arc::new(MockProvider::new(responses));
            (p, String::new(), "mock".to_string())
        }
        None => {
            let config = LimConfig::load_from_default_path();
            let provider_name = config
                .resolve_provider_name(cli_provider.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let instance = config.provider(provider_name).ok_or_else(|| {
                lim_core::error::LimError::Config(format!(
                    "provider `{provider_name}` not found in .lim/config.toml"
                ))
            })?;
            let model = config
                .resolve_model(instance, cli_model.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let p = create_provider(instance)
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            (p, model, provider_name.to_string())
        }
    };

    println!("Provider: {provider_label}");
    if !model_name.is_empty() {
        println!("Model:    {model_name}");
    }
    if let Some(label) = resume_label {
        println!("Resume:   {label}");
    }

    // Create executor and tool registry
    let executor = build_executor(trust);
    let mut registry = ToolRegistry::new();
    lim_tools::register_builtins(&mut registry, executor);
    let registry = Arc::new(registry);

    println!(
        "Registered {} tools: {}",
        registry.schemas().len(),
        registry
            .schemas()
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Set up event channel and cancellation
    let (event_tx, mut event_rx) = broadcast::channel(1000);
    let cancel = CancellationToken::new();

    let config = AgentConfig {
        model: model_name,
        ..AgentConfig::default()
    };
    let mut agent = AgentLoop::new(provider, registry, event_tx.clone(), cancel)
        .with_config(config)
        .with_user_edits(user_edits);

    // Determine output path
    let session_path = output_path.unwrap_or_else(|| {
        let timestamp = chrono_lite_timestamp();
        Utf8PathBuf::from(format!("sessions/{timestamp}.jsonl"))
    });

    // Ensure parent directory exists
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent.as_std_path()).ok();
    }

    // Spawn event logger task
    let session_path_clone = session_path.clone();
    let logger_handle = tokio::spawn(async move {
        let mut writer = SessionWriter::create(&session_path_clone).await?;
        loop {
            match event_rx.recv().await {
                Ok(envelope) => {
                    writer.write_event(&envelope).await?;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event logger lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok::<(), lim_core::error::LimError>(())
    });

    // Create user message and run
    let user_msg = AgentMessage::user(&user_message);
    println!("\n--- User ---");
    println!("{user_message}\n");

    // If we have prior messages, continue from them; otherwise start fresh.
    let messages = if prior_messages.is_empty() {
        agent.run(user_msg).await?
    } else {
        agent.run_continue(prior_messages, user_msg).await?
    };

    // Save IDs before dropping agent (which releases its broadcast sender)
    let session_id = agent.session_id();
    let branch_id = agent.branch_id();
    drop(agent);

    // Close the event channel so the logger finishes
    drop(event_tx);
    let _ = logger_handle.await;

    // Print transcript
    print_transcript(&messages);

    // Verify JSONL was written and replay works
    let events = lim_session::reader::SessionReader::read_all(&session_path).await?;
    let replayed = replay_transcript(&events);
    println!("\n--- Session Info ---");
    println!("Session ID: {session_id}");
    println!("Branch ID:  {branch_id}");
    println!("Events logged: {}", events.len());
    println!("Messages in transcript: {}", messages.len());
    println!("Messages from replay:   {}", replayed.len());
    println!("Event log: {session_path}");

    assert_eq!(
        replayed.len(),
        messages.len(),
        "replay produced different message count!"
    );
    println!("\nReplay verification: OK");

    Ok(())
}

fn print_transcript(messages: &[AgentMessage]) {
    for msg in messages {
        match msg.role {
            lim_core::message::Role::User => {
                // Already printed above
            }
            lim_core::message::Role::Assistant => {
                println!("--- Assistant ---");
                for block in &msg.content {
                    match block {
                        lim_core::content::ContentBlock::Text { text } => {
                            println!("{text}");
                        }
                        lim_core::content::ContentBlock::ToolCall {
                            name, arguments, ..
                        } => {
                            println!("[tool call: {name}({})]", arguments);
                        }
                        lim_core::content::ContentBlock::Thinking { text } => {
                            println!("[thinking: {text}]");
                        }
                        _ => {}
                    }
                }
                println!();
            }
            lim_core::message::Role::Tool => {
                println!("--- Tool Result ---");
                for block in &msg.content {
                    if let lim_core::content::ContentBlock::Text { text } = block {
                        if text.len() > 200 {
                            println!("{}... (truncated)", &text[..200]);
                        } else {
                            println!("{text}");
                        }
                    }
                }
                println!();
            }
            lim_core::message::Role::System => {}
        }
    }
}

/// Generate a simple timestamp for session file naming without chrono dependency.
fn chrono_lite_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    format!("{}", now.as_secs())
}

/// Launch the interactive TUI.
async fn run_tui(
    cli_provider: Option<String>,
    cli_model: Option<String>,
    trust: TrustArg,
) -> Result<()> {
    // Resolve provider + model from config
    let config = LimConfig::load_from_default_path();
    let provider_name = config
        .resolve_provider_name(cli_provider.as_deref())
        .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
    let instance = config.provider(provider_name).ok_or_else(|| {
        lim_core::error::LimError::Config(format!(
            "provider `{provider_name}` not found in .lim/config.toml"
        ))
    })?;
    let model = config
        .resolve_model(instance, cli_model.as_deref())
        .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
    let provider = create_provider(instance)
        .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;

    // Create executor and tool registry
    let executor = build_executor(trust);
    let mut registry = ToolRegistry::new();
    lim_tools::register_builtins(&mut registry, executor);
    let registry = Arc::new(registry);

    // Set up event channel and cancellation
    let (event_tx, _event_rx) = broadcast::channel(1000);
    let cancel = CancellationToken::new();

    // Set up session JSONL logging
    let timestamp = chrono_lite_timestamp();
    let session_path = Utf8PathBuf::from(format!("sessions/{timestamp}.jsonl"));
    std::fs::create_dir_all("sessions").ok();
    let session_path_clone = session_path.clone();
    let mut logger_rx = event_tx.subscribe();
    let logger_handle = tokio::spawn(async move {
        let mut writer = SessionWriter::create(&session_path_clone).await?;
        loop {
            match logger_rx.recv().await {
                Ok(envelope) => {
                    writer.write_event(&envelope).await?;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event logger lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok::<(), lim_core::error::LimError>(())
    });

    let tui_config = lim_tui::TuiConfig {
        provider,
        model: model.clone(),
        provider_label: provider_name.to_string(),
        registry,
        lim_config: config,
        event_tx,
        cancel: cancel.clone(),
        trust_level: if trust.is_permissive() {
            lim_core::TrustLevel::Permissive
        } else {
            lim_core::TrustLevel::Workspace
        },
    };

    // Run the TUI — this consumes event_tx; when the TUI exits, the channel
    // closes and the logger task finishes.
    lim_tui::run(tui_config).await?;

    // Clean up: cancel any pending agent work, wait for logger to flush
    cancel.cancel();
    let _ = logger_handle.await;

    eprintln!("Session log: {session_path}");

    Ok(())
}

/// `lim edit-context` — append a `ContextEdited` event to a saved session.
///
/// Reads the session JSONL to recover (session_id, branch_id, trace_id, seq),
/// builds a `ContextEdit` from `--exclude` / `--replace` flags, and appends a
/// single `EventEnvelope { event: AgentEvent::ContextEdited, .. }` line.
/// The next `lim run --resume <session>` will pick up the edit via
/// `replay_edits`.
///
/// `--list` short-circuits and prints the messages with indices instead of
/// writing an event.
async fn edit_context(
    session: Utf8PathBuf,
    exclude: Option<String>,
    replace: Vec<String>,
    list: bool,
) -> Result<()> {
    let events = lim_session::reader::SessionReader::read_all(&session).await?;
    if events.is_empty() {
        eprintln!("Session file `{session}` is empty or does not exist");
        return Ok(());
    }

    let messages = replay_transcript(&events);

    // Always print the message index map — useful with or without --list.
    println!("Session: {session}");
    println!("Messages: {}", messages.len());
    println!("Events:   {}", events.len());
    if !messages.is_empty() {
        println!();
        println!("{:<6} {:<6} PREVIEW", "IDX", "ROLE");
        for (i, msg) in messages.iter().enumerate() {
            let role = match msg.role {
                lim_core::message::Role::User => "user",
                lim_core::message::Role::Assistant => "asst",
                lim_core::message::Role::Tool => "tool",
                lim_core::message::Role::System => "sys",
            };
            let preview: String = msg
                .content
                .iter()
                .find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let preview = if preview.len() > 60 {
                format!("{}...", &preview[..57])
            } else {
                preview
            };
            println!("{i:<6} {role:<6} {preview}");
        }
    }

    if list {
        return Ok(());
    }

    // Build the edit from CLI flags.
    let mut edit = ContextEdit::none();

    if let Some(exclude_str) = exclude {
        let indices: Vec<usize> = exclude_str
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .collect();
        for idx in &indices {
            edit = edit.exclude(*idx);
        }
        if !indices.is_empty() {
            println!("\nExcluding indices: {:?}", indices);
        }
    }

    for spec in &replace {
        // Format: "INDEX:NEW_TEXT"
        match spec.split_once(':') {
            Some((idx_str, text)) => {
                let idx: usize = match idx_str.trim().parse() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("Invalid index in --replace `{spec}` (expected `INDEX:TEXT`)");
                        continue;
                    }
                };
                let text = text.trim();
                if text.is_empty() {
                    eprintln!("Empty replacement text in --replace `{spec}`");
                    continue;
                }
                edit = edit.replace(idx, vec![ContentBlock::text(text)]);
                println!("Replacing message {idx}'s content with: {text}");
            }
            None => {
                eprintln!("Malformed --replace `{spec}` (expected `INDEX:TEXT`)");
            }
        }
    }

    if edit.is_empty() {
        println!("\nNo edits specified. Use --exclude or --replace, or --list to just view.");
        return Ok(());
    }

    if !edit.validate(messages.len()) {
        eprintln!(
            "\nERROR: edit contains out-of-bounds indices (message count = {}).",
            messages.len()
        );
        eprintln!("Valid indices: 0..{}", messages.len().saturating_sub(1));
        return Err(lim_core::error::LimError::ToolExecution {
            name: "edit_context".to_string(),
            message: "edit validation failed: out-of-bounds indices".to_string(),
        });
    }

    // Recover correlation IDs from the last event in the file.
    let last = events.last().unwrap();
    let next_seq = last.seq + 1;
    let session_id = last.session_id;
    let branch_id = last.branch_id;
    let trace_id = last.trace_id;

    let envelope = EventEnvelope::new(
        next_seq,
        session_id,
        branch_id,
        trace_id,
        AgentEvent::ContextEdited { edit: edit.clone() },
    );

    let line = serde_json::to_string(&envelope).map_err(|e| {
        lim_core::error::LimError::ToolExecution {
            name: "edit_context".to_string(),
            message: format!("failed to serialize event: {e}"),
        }
    })?;

    // Append to the session file. We intentionally do NOT use SessionWriter
    // here because it truncates on open — that would wipe the prior session.
    // Appending a single line is the correct behavior for adding a new event
    // to an existing log.
    let mut file = tokio::fs::OpenOptions::new()
        .create(false)
        .append(true)
        .open(session.as_std_path())
        .await
        .map_err(|e| lim_core::error::LimError::SessionIo(format!(
            "failed to open session file for append: {e}"
        )))?;
    file.write_all((line + "\n").as_bytes()).await.map_err(|e| {
        lim_core::error::LimError::SessionIo(format!("failed to append edit: {e}"))
    })?;
    file.flush().await.map_err(|e| {
        lim_core::error::LimError::SessionIo(format!("failed to flush edit: {e}"))
    })?;

    println!("\nAppended ContextEdited event (seq {next_seq}) to {session}");
    println!("Next `lim run --resume {session}` will apply this edit.");

    Ok(())
}
