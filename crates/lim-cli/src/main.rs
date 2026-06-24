use std::sync::Arc;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::AgentLoop;
use lim_agent::registry::ToolRegistry;
use lim_core::error::Result;
use lim_core::message::AgentMessage;
use lim_executor::local::LocalExecutor;
use lim_llm::mock::{MockProvider, MockResponse, MockToolCall};
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
    /// Run an agent session.
    Run {
        /// Path to a fixture JSON file for FakeProvider.
        #[arg(long)]
        fake: Utf8PathBuf,

        /// Initial user message (defaults to "Hello").
        #[arg(long, default_value = "Hello")]
        message: String,

        /// Path to write the JSONL event log (defaults to sessions/<timestamp>.jsonl).
        #[arg(long)]
        output: Option<Utf8PathBuf>,
    },

    /// Generate a chapter using the novel-pack workflow.
    Chapter {
        /// Path to the story bible JSON file.
        #[arg(long)]
        bible: Utf8PathBuf,

        /// Path to the chapter outline JSON file.
        #[arg(long)]
        outline: Utf8PathBuf,

        /// Path to the MockProvider responses JSON file.
        #[arg(long)]
        responses: Utf8PathBuf,

        /// Output directory for artifacts (defaults to sessions/artifacts/).
        #[arg(long)]
        output: Option<Utf8PathBuf>,
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
            fake,
            message,
            output,
        } => {
            run_session(fake, message, output).await?;
        }
        Commands::Chapter {
            bible,
            outline,
            responses,
            output,
        } => {
            run_chapter(bible, outline, responses, output).await?;
        }
    }

    Ok(())
}

async fn run_session(
    fixture_path: Utf8PathBuf,
    user_message: String,
    output_path: Option<Utf8PathBuf>,
) -> Result<()> {
    // Load fixture
    let responses = load_fixture(&fixture_path)?;
    println!("Loaded {} scripted responses from {fixture_path}", responses.len());

    // Create provider
    let provider = Arc::new(MockProvider::new(responses));

    // Create executor and tool registry
    let executor = Arc::new(LocalExecutor::permissive());
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

    // Create agent loop
    let mut agent = AgentLoop::new(provider, registry, event_tx.clone(), cancel);

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

    let messages = agent.run(user_msg).await?;

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

/// Load a JSON file and deserialize it.
fn load_json<T: serde::de::DeserializeOwned>(path: &Utf8PathBuf) -> Result<T> {
    let content = std::fs::read_to_string(path.as_std_path()).map_err(|e| {
        lim_core::error::LimError::ToolExecution {
            name: "file_loader".to_string(),
            message: format!("failed to read `{path}`: {e}"),
        }
    })?;
    serde_json::from_str(&content).map_err(|e| {
        lim_core::error::LimError::ToolExecution {
            name: "json_parser".to_string(),
            message: format!("failed to parse `{path}`: {e}"),
        }
    })
}

async fn run_chapter(
    bible_path: Utf8PathBuf,
    outline_path: Utf8PathBuf,
    responses_path: Utf8PathBuf,
    output_dir: Option<Utf8PathBuf>,
) -> Result<()> {
    // Load inputs
    let bible: novel_pack::StoryBible = load_json(&bible_path)?;
    let outline: novel_pack::ChapterOutline = load_json(&outline_path)?;
    let responses: Vec<MockResponse> = load_fixture(&responses_path)?;

    println!("Story: \"{}\" ({})", bible.title, bible.genre);
    println!(
        "Chapter {}: {} ({} scenes)",
        outline.chapter_number,
        outline.summary,
        outline.scenes.len()
    );
    println!("Loaded {} scripted responses", responses.len());

    // Build workflow graph
    let graph = novel_pack::build_chapter_workflow(&bible, &outline);
    let levels = graph.topological_order()?;
    println!(
        "Workflow: {} nodes in {} levels",
        graph.node_ids().len(),
        levels.len()
    );

    // Create provider and registry
    let provider = Arc::new(MockProvider::new(responses));
    let registry = Arc::new(ToolRegistry::new());

    // Create artifact store
    let artifact_dir = output_dir.unwrap_or_else(|| Utf8PathBuf::from("sessions/artifacts"));
    std::fs::create_dir_all(artifact_dir.as_std_path()).ok();
    let artifact_store = Arc::new(lim_workflow::ArtifactStore::new(artifact_dir.clone()));

    // Create workflow event channel and cancellation token
    let (wf_event_tx, _wf_event_rx) = broadcast::channel(100);
    let cancel = CancellationToken::new();

    // Create executor
    let executor = lim_workflow::WorkflowExecutor::new(
        provider,
        registry,
        artifact_store.clone(),
        cancel,
        wf_event_tx,
    );

    // Create initial context
    let context = lim_workflow::WorkflowContext::new();

    // Execute workflow
    println!("\n--- Executing workflow ---\n");
    let final_context = executor.execute(&graph, context).await?;

    // Print results
    println!("\n--- Workflow Results ---");

    // Print scene drafts
    for scene in &outline.scenes {
        let node_id = format!("scene_{}_loop", scene.scene_number);
        if let Some(output) = final_context.get_output(&node_id) {
            println!("\nScene {} ({}):", scene.scene_number, scene.setting);
            if let lim_workflow::NodeOutput::Data(v) = &output
                && let Some(draft) = v.get("draft").and_then(|d| d.as_str())
            {
                let preview = if draft.len() > 200 {
                    format!("{}...", &draft[..200])
                } else {
                    draft.to_string()
                };
                println!("  Draft: {preview}");
            }
        }
    }

    // Print chapter output
    if let Some(chapter_output) = final_context.get_output("chapter") {
        println!("\n--- Final Chapter ---");
        if let lim_workflow::NodeOutput::Data(v) = &chapter_output
            && let Some(text) = v.get("text").and_then(|t| t.as_str())
        {
            println!("{text}");
        }
    }

    // List artifacts
    let artifacts = artifact_store.list().await?;
    println!("\n--- Artifacts ---");
    println!("{} artifacts stored in {artifact_dir}/", artifacts.len());
    for artifact in &artifacts {
        println!("  {} ({})", artifact.id, artifact.kind);
    }

    println!("\nChapter generation complete.");
    Ok(())
}
