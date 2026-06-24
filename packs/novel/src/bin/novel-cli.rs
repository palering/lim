use std::sync::Arc;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use lim_agent::registry::ToolRegistry;
use lim_core::error::{LimError, Result};
use lim_llm::mock::{MockProvider, MockResponse};
use lim_workflow::context::WorkflowContext;
use lim_workflow::executor::WorkflowExecutor;
use lim_workflow::node::NodeOutput;

use novel_pack::{ChapterOutline, StoryBible, build_chapter_workflow};

/// novel-cli — creative writing CLI powered by the lim runtime.
#[derive(Parser)]
#[command(name = "novel-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
        LimError::ToolExecution {
            name: "fixture_loader".to_string(),
            message: format!("failed to read fixture `{path}`: {e}"),
        }
    })?;

    let entries: Vec<FixtureEntry> = serde_json::from_str(&content).map_err(|e| {
        LimError::ToolExecution {
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
                    .map(|c| {
                        lim_llm::mock::MockToolCall::new(c.name, c.arguments)
                    })
                    .collect(),
            },
        })
        .collect();

    Ok(responses)
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Utf8PathBuf) -> Result<T> {
    let content = std::fs::read_to_string(path.as_std_path()).map_err(|e| {
        LimError::ToolExecution {
            name: "file_loader".to_string(),
            message: format!("failed to read `{path}`: {e}"),
        }
    })?;
    serde_json::from_str(&content).map_err(|e| {
        LimError::ToolExecution {
            name: "json_parser".to_string(),
            message: format!("failed to parse `{path}`: {e}"),
        }
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
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

async fn run_chapter(
    bible_path: Utf8PathBuf,
    outline_path: Utf8PathBuf,
    responses_path: Utf8PathBuf,
    output_dir: Option<Utf8PathBuf>,
) -> Result<()> {
    let bible: StoryBible = load_json(&bible_path)?;
    let outline: ChapterOutline = load_json(&outline_path)?;
    let responses: Vec<MockResponse> = load_fixture(&responses_path)?;

    println!("Story: \"{}\" ({})", bible.title, bible.genre);
    println!(
        "Chapter {}: {} ({} scenes)",
        outline.chapter_number,
        outline.summary,
        outline.scenes.len()
    );
    println!("Loaded {} scripted responses", responses.len());

    let graph = build_chapter_workflow(&bible, &outline);
    let levels = graph.topological_order()?;
    println!(
        "Workflow: {} nodes in {} levels",
        graph.node_ids().len(),
        levels.len()
    );

    let provider = Arc::new(MockProvider::new(responses));
    let registry = Arc::new(ToolRegistry::new());

    let artifact_dir = output_dir.unwrap_or_else(|| Utf8PathBuf::from("sessions/artifacts"));
    std::fs::create_dir_all(artifact_dir.as_std_path()).ok();
    let artifact_store = Arc::new(lim_workflow::ArtifactStore::new(artifact_dir.clone()));

    let (wf_event_tx, _wf_event_rx) = broadcast::channel(100);
    let cancel = CancellationToken::new();

    let executor = WorkflowExecutor::new(
        provider,
        registry,
        artifact_store.clone(),
        cancel,
        wf_event_tx,
    );

    let context = WorkflowContext::new();

    println!("\n--- Executing workflow ---\n");
    let final_context = executor.execute(&graph, context).await?;

    println!("\n--- Workflow Results ---");

    for scene in &outline.scenes {
        let node_id = format!("scene_{}_loop", scene.scene_number);
        if let Some(output) = final_context.get_output(&node_id) {
            println!("\nScene {} ({}):", scene.scene_number, scene.setting);
            if let NodeOutput::Data(v) = &output
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

    if let Some(chapter_output) = final_context.get_output("chapter") {
        println!("\n--- Final Chapter ---");
        if let NodeOutput::Data(v) = &chapter_output
            && let Some(text) = v.get("text").and_then(|t| t.as_str())
        {
            println!("{text}");
        }
    }

    let artifacts = artifact_store.list().await?;
    println!("\n--- Artifacts ---");
    println!("{} artifacts stored in {artifact_dir}/", artifacts.len());
    for artifact in &artifacts {
        println!("  {} ({})", artifact.id, artifact.kind);
    }

    println!("\nChapter generation complete.");
    Ok(())
}
