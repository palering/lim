use std::sync::Arc;
use std::time::SystemTime;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::{AgentConfig, AgentLoop};
use lim_agent::context_window::{ContextConfig, ContextWindowManager};
use lim_agent::prompt_builder::PromptBuilder;
use lim_agent::registry::ToolRegistry;
use lim_agent::{
    AgentFactory, AgentRoleSpec, AlwaysPassVerifier, Budget, CommandVerifier, DaemonCommand,
    DaemonLedger, DaemonServer, GoalResult, GoalStatus, IpcTransport, LedgerBackend, LlmVerifier,
    LoopRunner, ReviewerVerifier, StateLedger, Verifier,
};
use lim_core::content::ContentBlock;
use lim_core::context_edit::ContextEdit;
use lim_core::env::PlatformInfo;
use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::id::GoalId;
use lim_core::inject::InjectInfo;
use lim_core::message::AgentMessage;
use lim_executor::local::LocalExecutor;
use lim_llm::Provider;
use lim_llm::mock::{MockProvider, MockResponse, MockToolCall};
use lim_llm::providers::{LimConfig, ProviderType, create_provider};
use lim_session::replay::replay_transcript;
use lim_session::writer::SessionWriter;
use lim_session::{SessionDetails, SessionManager, SessionMeta};

/// Lim — a Rust-native agent runtime.
#[derive(Parser)]
#[command(name = "lim", version = lim_core::LIM_VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a lim workspace in the current directory.
    ///
    /// Creates `.lim/` with a `config.toml` template and an `AGENTS.md`
    /// template. Run this once per project before using other lim commands.
    ///
    /// ## Examples
    ///
    ///   lim init                          # minimal template
    ///   lim init --provider anthropic     # pre-fill anthropic provider
    ///   lim init --provider openai        # pre-fill openai provider
    ///   lim init --force                  # overwrite existing config.toml
    Init {
        /// Pre-fill a provider template (`anthropic` or `openai`).
        ///
        /// When omitted, the config template includes both providers
        /// commented out.
        #[arg(long)]
        provider: Option<String>,

        /// Overwrite an existing `.lim/config.toml`.
        ///
        /// Without this flag, `lim init` refuses to overwrite an existing
        /// config file to prevent data loss.
        #[arg(long)]
        force: bool,
    },

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

        /// Resume from a prior session JSONL file or session ID.
        ///
        /// Accepts either:
        /// - A path to a `.jsonl` file (e.g. `sessions/1234567890.jsonl`)
        /// - A session ID (26-char ULID-style string) — the sessions
        ///   directory is scanned for a matching file.
        ///
        /// When set, all messages from the prior session are loaded into
        /// context, and any `ContextEdited` events are replayed via
        /// `AgentLoop::with_user_edits`. The new message is appended as
        /// the next user turn. May be combined with `--fake` to script
        /// the new turn's assistant responses.
        #[arg(long)]
        resume: Option<String>,

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

    /// List saved sessions in the local `sessions/` directory.
    ///
    /// Scans `sessions/*.jsonl` and prints a table sorted by last-modified
    /// time (most recent first). Use `--details <PATH>` to see message count,
    /// token totals, and the first user message (used as a title).
    Sessions {
        /// Show detailed metadata for the given session path (reads the
        /// full JSONL to compute message count + token totals + title).
        #[arg(long)]
        details: Option<Utf8PathBuf>,

        /// Limit the number of sessions listed (default: 20).
        #[arg(long, default_value = "20")]
        limit: usize,
    },

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

    /// Loop Engineering: run an agent across multiple sessions toward a goal.
    ///
    /// Each session gets a fresh context window. State is persisted in
    /// `.lim/loop/<goal_id>/` so interrupted goals can be resumed.
    ///
    /// Examples:
    ///   lim loop --goal "fix all clippy warnings" --verify "cargo clippy"
    ///   lim loop --goal "write design doc" --verify llm --verifier-model claude-sonnet
    ///   lim loop --resume 01J...
    ///   lim loop --list
    ///   lim loop --status 01J...
    Loop {
        /// Start a new goal with this description.
        #[arg(long)]
        goal: Option<String>,

        /// Resume an interrupted goal by ID.
        #[arg(long)]
        resume: Option<String>,

        /// List all goals in the workspace.
        #[arg(long)]
        list: bool,

        /// Show status of a specific goal.
        #[arg(long)]
        status: Option<String>,

        /// Verifier mode. Three special values or a shell command:
        ///
        /// - A shell command (e.g. `"cargo clippy"`) — exit 0 = goal achieved
        ///   (CommandVerifier, deterministic, preferred).
        /// - `llm` — LLM-as-judge: a single LLM call with no tools, sees only
        ///   a progress summary. Lightweight fallback for goals without
        ///   deterministic verification.
        /// - `reviewer` — spawns an independent reviewer agent with a fresh
        ///   context window and tool access (can read files, run commands).
        ///   The "second agent code review" pattern: deeper than `llm` but
        ///   more expensive (full agent session per verify() call).
        ///
        /// If omitted, the loop runs one session and stops (AlwaysPass).
        ///
        /// For `llm` and `reviewer`, an independent evaluator provider/model
        /// is used (avoids the main agent's reasoning bias). Configure with
        /// `--verifier-provider`, `--verifier-model`, and optionally
        /// `--verifier-instructions`.
        #[arg(long)]
        verify: Option<String>,

        /// Maximum number of sessions to run.
        #[arg(long)]
        max_sessions: Option<usize>,

        /// Maximum cumulative tokens across all sessions.
        #[arg(long)]
        max_tokens: Option<u64>,

        /// Named provider instance from `.lim/config.toml`.
        #[arg(long)]
        provider: Option<String>,

        /// Model name to request.
        #[arg(long)]
        model: Option<String>,

        /// Provider instance for the LLM verifier (only used with
        /// `--verify llm`). Defaults to the main `--provider`.
        ///
        /// Using a different provider for verification follows the
        /// "independent evaluator" pattern — the verifier LLM should not
        /// share the main agent's context or bias.
        #[arg(long)]
        verifier_provider: Option<String>,

        /// Model for the LLM verifier (only used with `--verify llm`).
        /// Defaults to the main `--model`.
        ///
        /// Tip: use a cheaper/faster model for verification (e.g.
        /// `claude-haiku` or `gpt-4o-mini`) since each `verify()` call is
        /// one LLM request.
        #[arg(long)]
        verifier_model: Option<String>,

        /// Extra instructions for the LLM verifier (only used with
        /// `--verify llm`). Free-form text appended to the verifier prompt.
        ///
        /// Example: `--verifier-instructions "be strict about edge cases"`
        #[arg(long)]
        verifier_instructions: Option<String>,

        /// Trust level: `workspace` (default) or `permissive`.
        #[arg(long, default_value = "workspace")]
        trust: TrustArg,
    },

    /// Loop Engineering daemon: manage goals across multiple workspaces.
    ///
    /// The daemon mode (Stage 4 of the evolution roadmap) provides an
    /// independent storage backend (`DaemonLedger`) for goals that may span
    /// multiple workspaces. State is stored under `<storage-root>/` (default
    /// `.lim/daemon/`) and can be synced back to a workspace's `.lim/loop/`
    /// via `sync`.
    ///
    /// ## Current limitations (P3 scope)
    ///
    /// - `start` runs an in-process daemon (no socket/pipe IPC yet). Other
    ///   subcommands operate directly on the daemon ledger storage.
    /// - `submit` records the goal but does not yet spawn a `LoopRunner`.
    ///   Actual execution requires `lim loop --resume <ID>` in the bound
    ///   workspace after `sync`.
    ///
    /// ## Examples
    ///
    ///   lim loop-daemon start
    ///   lim loop-daemon submit --goal "refactor module X" --workspace /path/to/ws
    ///   lim loop-daemon list
    ///   lim loop-daemon status 01J...
    ///   lim loop-daemon sync 01J...
    LoopDaemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Manage goals in the daemon ledger (preferred alias for the
    /// goal-management subset of `lim loop-daemon`).
    ///
    /// `lim goal` covers submit / list / status / pause / resume / sync.
    /// Daemon lifecycle (start / shutdown) is handled by `lim serve
    /// --goal-daemon` (the deprecated `lim loop-daemon start` still works).
    ///
    /// ## Examples
    ///
    ///   lim goal submit --goal "refactor module X" --workspace /path/to/ws
    ///   lim goal list
    ///   lim goal status 01J...
    ///   lim goal sync 01J...
    Goal {
        #[command(subcommand)]
        action: GoalAction,
    },

    /// Run lim as a long-running server (MCP + A2A + goal daemon).
    ///
    /// This is the unified entry point for all "persistent" modes. It
    /// replaces `loop-daemon start` and provides MCP/A2A protocol
    /// servers plus the goal daemon as built-in capabilities.
    ///
    /// ## Examples
    ///
    ///   lim serve                              # foreground, all protocols + goal daemon
    ///   lim serve --protocol mcp               # MCP server only (testing)
    ///   lim serve --protocol a2a               # A2A agent only (testing)
    ///   lim serve --protocol none              # goal daemon only
    ///   lim serve --daemon                     # background mode
    ///   lim serve --system install             # install as system service
    Serve {
        /// Protocol to expose: `all` (default), `mcp`, `a2a`, or `none`.
        ///
        /// `all` enables both MCP and A2A (the default for a unified server).
        /// `mcp` / `a2a` select a single protocol (mainly for testing).
        /// `none` runs only the goal daemon.
        ///
        /// When omitted, falls back to `[serve].protocol` in the config file,
        /// then to `"all"`.
        #[arg(long)]
        protocol: Option<String>,

        /// Run in the background (detached from terminal).
        ///
        /// When neither `--daemon` nor `--system` is given, falls back to
        /// `[serve].mode` in the config file, then to foreground.
        #[arg(long)]
        daemon: bool,

        /// Manage system service installation (install/uninstall/start/stop/status/restart).
        #[arg(long, value_name = "ACTION")]
        system: Option<String>,

        /// Workspace path to bind the server to.
        #[arg(long)]
        workspace: Option<Utf8PathBuf>,

        /// Path to a config file (overrides default lookup).
        #[arg(long)]
        config: Option<Utf8PathBuf>,

        /// Endpoint to bind on (e.g. `unix://path`, `tcp://host:port`, `http://host:port`).
        #[arg(long)]
        endpoint: Option<String>,

        /// Auth token for A2A / network protocols.
        #[arg(long)]
        auth_token: Option<String>,
    },
}

/// Subcommands for `lim loop-daemon`.
#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon server (in-process, blocks until shutdown).
    Start {
        /// Storage root for the daemon ledger (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Submit a new goal to the daemon ledger.
    Submit {
        /// Goal description.
        #[arg(long)]
        goal: String,
        /// Optional workspace path to bind the goal to.
        #[arg(long)]
        workspace: Option<Utf8PathBuf>,
        /// Optional verifier command (recorded for future execution).
        #[arg(long)]
        verify: Option<String>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// List all goals in the daemon ledger.
    List {
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Show status of a goal in the daemon ledger.
    Status {
        /// Goal ID.
        goal_id: String,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Pause a goal (sets status to Paused in the ledger).
    Pause {
        /// Goal ID.
        goal_id: String,
        /// Optional pause reason.
        #[arg(long)]
        reason: Option<String>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Resume a paused goal (sets status back to Running).
    Resume {
        /// Goal ID.
        goal_id: String,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Sync a goal's state to its bound workspace's `.lim/loop/`.
    Sync {
        /// Goal ID.
        goal_id: String,
        /// Workspace root to sync to (overrides the bound workspace).
        #[arg(long)]
        workspace: Option<Utf8PathBuf>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Shut down a running daemon (in-process; for future IPC mode).
    Shutdown,
}

/// Subcommands for `lim goal` — goal-management actions.
///
/// This is the preferred alias for the goal-management subset of
/// `lim loop-daemon` (submit / list / status / pause / resume / sync).
/// The daemon lifecycle actions (start / shutdown) stay under
/// `lim loop-daemon` (deprecated) and `lim serve --goal-daemon`.
#[derive(Subcommand)]
enum GoalAction {
    /// Submit a new goal to the daemon ledger.
    Submit {
        /// Goal description.
        #[arg(long)]
        goal: String,
        /// Optional workspace path to bind the goal to.
        #[arg(long)]
        workspace: Option<Utf8PathBuf>,
        /// Optional verifier command (recorded for future execution).
        #[arg(long)]
        verify: Option<String>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// List all goals in the daemon ledger.
    List {
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Show status of a goal in the daemon ledger.
    Status {
        /// Goal ID.
        goal_id: String,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Pause a goal (sets status to Paused in the ledger).
    Pause {
        /// Goal ID.
        goal_id: String,
        /// Optional pause reason.
        #[arg(long)]
        reason: Option<String>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Resume a paused goal (sets status back to Running).
    Resume {
        /// Goal ID.
        goal_id: String,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
    /// Sync a goal's state to its bound workspace's `.lim/loop/`.
    Sync {
        /// Goal ID.
        goal_id: String,
        /// Workspace root to sync to (overrides the bound workspace).
        #[arg(long)]
        workspace: Option<Utf8PathBuf>,
        /// Storage root (default: .lim/daemon/).
        #[arg(long)]
        storage: Option<Utf8PathBuf>,
    },
}

/// Fixture format for MockProvider scripted responses.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum FixtureEntry {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_calls")]
    ToolCalls { text: Option<String>, calls: Vec<FixtureToolCall> },
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

/// Bundled CLI options for the loop verifier.
///
/// Groups `--verify` (mode selector) with the three LLM-verifier tuning
/// flags so they can be threaded through `run_loop` / `build_loop_runner`
/// as a single value instead of four loose arguments.
#[derive(Clone, Default)]
struct VerifierCliConfig {
    /// Verifier mode: `Some("llm")` → LlmVerifier, `Some(cmd)` →
    /// CommandVerifier, `None` → AlwaysPassVerifier (one-shot).
    verify: Option<String>,
    /// Provider instance for the LLM verifier (defaults to main `--provider`).
    verifier_provider: Option<String>,
    /// Model for the LLM verifier (defaults to main `--model`).
    verifier_model: Option<String>,
    /// Extra prompt instructions appended to the verifier prompt.
    verifier_instructions: Option<String>,
}

/// Build a `LocalExecutor` based on the trust argument and detected platform.
///
/// `workspace`: restrict to detected workspace roots (git root preferred).
/// `permissive`: allow everything, print a warning to stderr.
fn build_executor(trust: TrustArg, platform: &PlatformInfo) -> Arc<LocalExecutor> {
    if trust.is_permissive() {
        eprintln!("⚠ WARNING: running in permissive trust mode — no file/command restrictions.");
        eprintln!("  Use `--trust workspace` (default) for production.");
        #[allow(deprecated)]
        Arc::new(LocalExecutor::permissive())
    } else {
        let roots: Vec<Utf8PathBuf> = platform.workspace_roots.clone();
        let root_display = roots.first().map(|r| r.as_str()).unwrap_or(".");
        eprintln!("Trust: workspace (root: {root_display})");
        Arc::new(LocalExecutor::workspace(roots))
    }
}

/// Print detected platform info to stderr (gives the user visibility into
/// shell selection, workspace root resolution, etc.).
fn print_platform_info(platform: &PlatformInfo) {
    eprintln!(
        "Platform: {} / {} | Shell: {} ({})",
        platform.os.as_str(),
        platform.arch.as_str(),
        platform.shell.as_str(),
        platform.shell.description(),
    );
    if let Some(root) = platform.workspace_roots.first() {
        eprintln!("Workspace: {root}");
    }
}

#[derive(Debug, Deserialize)]
struct FixtureToolCall {
    name: String,
    arguments: serde_json::Value,
}

fn load_fixture(path: &Utf8PathBuf) -> anyhow::Result<Vec<MockResponse>> {
    let content = std::fs::read_to_string(path.as_std_path()).map_err(|e| {
        lim_core::error::LimError::tool_execution(
            "fixture_loader",
            format!("failed to read fixture `{path}`: {e}"),
            e,
        )
    })?;

    let entries: Vec<FixtureEntry> = serde_json::from_str(&content).map_err(|e| {
        lim_core::error::LimError::tool_execution(
            "fixture_loader",
            format!("failed to parse fixture `{path}`: {e}"),
            e,
        )
    })?;

    let responses: Vec<MockResponse> = entries
        .into_iter()
        .map(|entry| match entry {
            FixtureEntry::Text { text } => MockResponse::Text(text),
            FixtureEntry::ToolCalls { text, calls } => MockResponse::ToolCalls {
                text,
                calls: calls.into_iter().map(|c| MockToolCall::new(c.name, c.arguments)).collect(),
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
        Commands::Init { provider, force } => {
            run_init(provider.as_deref(), force)?;
        }
        Commands::Run { provider, model, fake, message, output, resume, trust } => {
            run_session(provider, model, fake, resume, message, output, trust).await?;
        }
        Commands::Tui { provider, model, trust } => {
            run_tui(provider, model, trust).await?;
        }
        Commands::Providers => {
            list_providers();
        }
        Commands::Sessions { details, limit } => {
            if let Some(path) = details {
                show_session_details(&path).await?;
            } else {
                list_sessions(limit).await?;
            }
        }
        Commands::EditContext { session, exclude, replace, list } => {
            edit_context(session, exclude, replace, list).await?;
        }
        Commands::Loop {
            goal,
            resume,
            list,
            status,
            verify,
            max_sessions,
            max_tokens,
            provider,
            model,
            verifier_provider,
            verifier_model,
            verifier_instructions,
            trust,
        } => {
            let verifier_cfg = VerifierCliConfig {
                verify,
                verifier_provider,
                verifier_model,
                verifier_instructions,
            };
            run_loop(
                goal,
                resume,
                list,
                status,
                verifier_cfg,
                max_sessions,
                max_tokens,
                provider,
                model,
                trust,
            )
            .await?;
        }
        Commands::LoopDaemon { action } => {
            run_loop_daemon(action).await?;
        }
        Commands::Goal { action } => {
            run_goal(action).await?;
        }
        Commands::Serve { protocol, daemon, system, workspace, config, endpoint, auth_token } => {
            run_serve(
                protocol.as_deref(),
                daemon,
                system.as_deref(),
                workspace,
                config,
                endpoint,
                auth_token,
            )
            .await?;
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
        let endpoint = instance.full_url.as_deref().unwrap_or_else(|| instance.resolve_base_url());
        // Truncate long endpoints for display
        let endpoint_display: String = if endpoint.len() > 46 {
            format!("{}...", &endpoint[..43])
        } else {
            endpoint.to_string()
        };
        let model = instance.default_model.as_deref().unwrap_or("(none)");
        let marker =
            if Some(name.as_str()) == config.default_provider.as_deref() { " *" } else { "" };
        println!("{:<20} {:<18} {:<48} {}{}", name, type_str, endpoint_display, model, marker,);
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

/// Resolve a `--resume <value>` argument into a JSONL file path.
///
/// Heuristics:
/// - If `value` ends with `.jsonl` AND the file exists → use as-is (path).
/// - Otherwise, treat as a session ID and scan `sessions/` for a match.
/// - If neither matches, fall back to assuming it's a path (let the
///   subsequent read fail with a clear error).
async fn resolve_resume_path(value: &str) -> anyhow::Result<Utf8PathBuf> {
    // Quick path case: looks like a path with `.jsonl` suffix.
    if value.ends_with(".jsonl") {
        let path = Utf8PathBuf::from(value);
        if path.exists() {
            return Ok(path);
        }
        // Path-like but doesn't exist — let the caller fail with a clearer error.
        return Ok(path);
    }

    // Try parsing as a SessionId and look it up in sessions/.
    if let Ok(session_id) = value.parse::<lim_core::id::SessionId>() {
        let sm = SessionManager::new(Utf8PathBuf::from("sessions"));
        if let Some(meta) = sm.find(&session_id).await? {
            return Ok(meta.path);
        }
    }

    // Fall back: treat as a path (relative or absolute). The subsequent
    // read_all will produce a clear error if it doesn't exist.
    Ok(Utf8PathBuf::from(value))
}

/// `lim sessions` — list saved sessions.
async fn list_sessions(limit: usize) -> anyhow::Result<()> {
    let sm = SessionManager::new(Utf8PathBuf::from("sessions"));
    let sessions = sm.list().await?;

    if sessions.is_empty() {
        println!("No sessions found in `{}`.", sm.sessions_dir());
        println!("Run `lim run --message '...'` to create a session.");
        return Ok(());
    }

    let shown: Vec<&SessionMeta> = sessions.iter().take(limit).collect();
    println!("Found {} session(s) (showing {}):", sessions.len(), shown.len());
    println!();
    println!("{:<26} {:<22} {:>7} {:>10}  PATH", "SESSION_ID", "UPDATED", "EVENTS", "SIZE");
    for meta in &shown {
        let updated = format_system_time(meta.updated_at);
        let size_str = format_size(meta.size_bytes);
        println!(
            "{:<26} {:<22} {:>7} {:>10}  {}",
            meta.session_id, updated, meta.event_count, size_str, meta.path
        );
    }

    if sessions.len() > shown.len() {
        println!();
        println!("(Use `--limit {}` to show more)", sessions.len());
    }
    println!();
    println!("Resume a session with:  lim run --resume <SESSION_ID>");
    println!("View details with:     lim sessions --details <PATH>");
    Ok(())
}

/// `lim sessions --details <path>` — show detailed session metadata.
async fn show_session_details(path: &Utf8PathBuf) -> anyhow::Result<()> {
    let sm = SessionManager::new(Utf8PathBuf::from("sessions"));
    let details: SessionDetails = sm.details(path).await?;

    println!("Session: {}", details.meta.session_id);
    println!("Path:    {}", details.meta.path);
    println!("Created: {}", format_system_time(details.meta.created_at));
    println!("Updated: {}", format_system_time(details.meta.updated_at));
    println!("Size:    {}", format_size(details.meta.size_bytes));
    println!();
    println!("Title:         {}", details.title);
    println!("Messages:      {}", details.message_count);
    println!("Events:        {}", details.meta.event_count);
    println!("Input tokens:  {}", details.total_input_tokens);
    println!("Output tokens: {}", details.total_output_tokens);
    println!();
    println!("Resume with: lim run --resume {}", details.meta.session_id);
    Ok(())
}

/// Format a SystemTime as a human-readable timestamp (UTC).
fn format_system_time(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// Convert Unix seconds to (year, month, day, hour, minute, second).
///
/// Uses the proleptic Gregorian calendar. No leap seconds. Good enough for
/// display purposes; we avoid pulling in chrono for this one helper.
fn unix_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let secs_in_day = (secs % 86400) as u32;
    let h = secs_in_day / 3600;
    let mi = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;

    // Days since 1970-01-01 → (year, month, day).
    // Algorithm: Howard Hinnant's "days_from_civil" inverse.
    let z = days + 719468; // shift epoch to 0000-03-01
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (doy * 5 + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m, d, h, mi, s)
}

/// Format a byte count as a human-readable string (e.g. "1.2 KB", "3.4 MB").
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} KB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.1} GB", gb)
}

async fn run_session(
    cli_provider: Option<String>,
    cli_model: Option<String>,
    fake: Option<Utf8PathBuf>,
    resume: Option<String>,
    user_message: String,
    output_path: Option<Utf8PathBuf>,
    trust: TrustArg,
) -> anyhow::Result<()> {
    // Resolve resume: accept either a path or a session ID.
    let resume_path: Option<Utf8PathBuf> = match resume.as_ref() {
        Some(s) => Some(resolve_resume_path(s).await?),
        None => None,
    };

    // If resuming, load prior session events and replay messages + edits.
    // prior_messages will be prepended to the new turn's context.
    // user_edits will be re-applied on every turn (persistent edits).
    let (prior_messages, user_edits, resume_label) = match resume_path.as_ref() {
        Some(path) => {
            let events = lim_session::reader::SessionReader::read_all(path).await?;
            let messages = replay_transcript(&events);
            let edits = lim_session::replay::replay_edits(&events);
            let label =
                format!("{} ({} prior messages, {} edits)", path, messages.len(), edits.len());
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

    // Detect platform (OS / arch / shell / workspace) — drives executor
    // scoping and system-prompt injection. Done once at startup; the snapshot
    // is immutable for the rest of the session.
    let platform = PlatformInfo::detect();
    print_platform_info(&platform);

    // Create executor and tool registry
    let executor = build_executor(trust, &platform);
    let registry = Arc::new(ToolRegistry::new());
    lim_tools::register_builtins(&registry, executor)?;

    println!(
        "Registered {} tools: {}",
        registry.schemas().len(),
        registry.schemas().iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
    );

    // Set up event channel and cancellation
    let (event_tx, mut event_rx) = broadcast::channel(1000);
    let cancel = CancellationToken::new();

    // Inject platform info into the system prompt so the LLM can adapt its
    // syntax (POSIX vs PowerShell, Unix paths vs Windows paths, etc.).
    let inject = InjectInfo::new(platform);

    // Load project context files (AGENTS.md, etc.) based on config.
    // Non-fatal: if config loading fails, we just skip context files.
    let project_context = load_project_context().unwrap_or_else(|e| {
        tracing::debug!(error = %e, "failed to load project context, skipping");
        String::new()
    });

    // Build a cache-aware PromptBuilder. This produces a PromptLayout with
    // cache breakpoints so Anthropic prompt caching fires on the system
    // prompt prefix (Static + Project tiers). The flat `system_prompt`
    // string is still set as a fallback for providers that don't consume
    // PromptLayout (e.g. OpenAI uses it via `effective_system_prompt()`).
    let mut prompt_builder = PromptBuilder::new(inject.clone());
    if !project_context.is_empty() {
        prompt_builder = prompt_builder.with_project_context(project_context);
    }
    let system_prompt = inject.system_prompt_fragment();

    let config = AgentConfig {
        model: model_name,
        system_prompt: Some(system_prompt),
        ..AgentConfig::default()
    };
    let mut agent = AgentLoop::new(provider, registry, event_tx.clone(), cancel)
        .with_config(config)
        .with_prompt_builder(prompt_builder)
        .with_user_edits(user_edits)
        // Wire up auto-compact so long sessions stay within the model's
        // context budget. Default config (128K window, 0.8 trigger ratio).
        .with_context_window(ContextWindowManager::new(ContextConfig::default()));

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
                    // Surface cache hit stats on TurnEnd so the user sees
                    // cost savings per turn. Printed to stderr so it
                    // doesn't pollute the transcript on stdout.
                    if let AgentEvent::TurnEnd { usage, .. } = &envelope.event {
                        if usage.cached_tokens > 0 || usage.cache_creation_tokens > 0 {
                            let eff_in = usage.effective_input_tokens();
                            let pct = (usage.cached_tokens * 100).checked_div(eff_in).unwrap_or(0);
                            eprintln!(
                                "  [cache] in={eff_in} cached={} ({pct}%) write={} out={}",
                                usage.cached_tokens,
                                usage.cache_creation_tokens,
                                usage.output_tokens
                            );
                        } else {
                            eprintln!(
                                "  [usage] in={} out={}",
                                usage.input_tokens, usage.output_tokens
                            );
                        }
                    }
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

    assert_eq!(replayed.len(), messages.len(), "replay produced different message count!");
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
                        lim_core::content::ContentBlock::ToolCall { name, arguments, .. } => {
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
        .expect("system clock is before UNIX_EPOCH");
    format!("{}", now.as_secs())
}

/// Launch the interactive TUI.
async fn run_tui(
    cli_provider: Option<String>,
    cli_model: Option<String>,
    trust: TrustArg,
) -> anyhow::Result<()> {
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
    let provider =
        create_provider(instance).map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;

    // Detect platform — drives executor scoping. (TUI does its own platform
    // detection for system-prompt injection; here we only need it for the
    // executor's workspace roots.)
    let platform = PlatformInfo::detect();
    print_platform_info(&platform);

    // Create executor and tool registry
    let executor = build_executor(trust, &platform);
    let registry = Arc::new(ToolRegistry::new());
    lim_tools::register_builtins(&registry, executor)?;

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
) -> anyhow::Result<()> {
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
            let preview =
                if preview.len() > 60 { format!("{}...", &preview[..57]) } else { preview };
            println!("{i:<6} {role:<6} {preview}");
        }
    }

    if list {
        return Ok(());
    }

    // Build the edit from CLI flags.
    let mut edit = ContextEdit::none();

    if let Some(exclude_str) = exclude {
        let indices: Vec<usize> =
            exclude_str.split(',').filter_map(|s| s.trim().parse::<usize>().ok()).collect();
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
        return Err(lim_core::error::LimError::tool_execution_msg(
            "edit_context",
            "edit validation failed: out-of-bounds indices",
        )
        .into());
    }

    // Recover correlation IDs from the last event in the file.
    let last = events.last().expect("checked is_empty above");
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
        lim_core::error::LimError::tool_execution(
            "edit_context",
            format!("failed to serialize event: {e}"),
            e,
        )
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
        .map_err(|e| {
            lim_core::error::LimError::session_io(
                format!("failed to open session file for append: {e}"),
                e,
            )
        })?;
    file.write_all((line + "\n").as_bytes()).await.map_err(|e| {
        lim_core::error::LimError::session_io(format!("failed to append edit: {e}"), e)
    })?;
    file.flush().await.map_err(|e| {
        lim_core::error::LimError::session_io(format!("failed to flush edit: {e}"), e)
    })?;

    println!("\nAppended ContextEdited event (seq {next_seq}) to {session}");
    println!("Next `lim run --resume {session}` will apply this edit.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Loop Engineering (`lim loop`)
// ---------------------------------------------------------------------------

/// Dispatch for `lim loop` subcommand.
#[allow(clippy::too_many_arguments)]
async fn run_loop(
    goal: Option<String>,
    resume: Option<String>,
    list: bool,
    status: Option<String>,
    verifier_cfg: VerifierCliConfig,
    max_sessions: Option<usize>,
    max_tokens: Option<u64>,
    cli_provider: Option<String>,
    cli_model: Option<String>,
    trust: TrustArg,
) -> anyhow::Result<()> {
    if list {
        return list_loop_goals().await;
    }
    if let Some(goal_id_str) = status {
        return show_loop_goal_status(&goal_id_str).await;
    }
    if let Some(goal_text) = goal {
        println!("Starting goal: {goal_text}");
        print_verifier_summary(&verifier_cfg);
        println!();
        let runner = build_loop_runner(
            verifier_cfg,
            max_sessions,
            max_tokens,
            cli_provider,
            cli_model,
            trust,
        )
        .await?;
        let (goal_id, result) = runner.start_goal(&goal_text).await?;
        print_goal_result(&goal_id, result, runner.ledger()).await?;
        return Ok(());
    }
    if let Some(goal_id_str) = resume {
        let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
            lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
        })?;
        println!("Resuming goal: {goal_id}");
        print_verifier_summary(&verifier_cfg);
        println!();
        let runner = build_loop_runner(
            verifier_cfg,
            max_sessions,
            max_tokens,
            cli_provider,
            cli_model,
            trust,
        )
        .await?;
        let result = runner.resume_goal(&goal_id).await?;
        print_goal_result(&goal_id, result, runner.ledger()).await?;
        return Ok(());
    }

    eprintln!("Usage: lim loop --goal \"...\" | --resume <ID> | --list | --status <ID>");
    eprintln!("Run `lim loop --help` for details.");
    std::process::exit(1);
}

/// Print a one-line summary of the verifier configuration.
fn print_verifier_summary(cfg: &VerifierCliConfig) {
    match cfg.verify.as_deref() {
        Some("llm") => {
            let p = cfg.verifier_provider.as_deref().unwrap_or("(default)");
            let m = cfg.verifier_model.as_deref().unwrap_or("(default)");
            println!("Verifier: llm (provider={p}, model={m})");
        }
        Some("reviewer") => {
            let p = cfg.verifier_provider.as_deref().unwrap_or("(default)");
            let m = cfg.verifier_model.as_deref().unwrap_or("(default)");
            println!("Verifier: reviewer (provider={p}, model={m})");
        }
        Some(cmd) => println!("Verifier: {cmd}"),
        None => println!("Verifier: always-pass (one-shot)"),
    }
}

/// Load the `reviewer` role config from `.lim/agents/reviewer.toml` (or
/// global `~/.config/lim/agents/reviewer.toml` if `agents=hybrid`).
///
/// Returns an error if the config loader fails or the `reviewer` role is
/// not found. The caller is expected to fall back to `default_reviewer_spec`
/// on error (so non-"not found" errors are logged but not fatal).
fn load_reviewer_role_config() -> anyhow::Result<lim_config::schema::AgentRoleConfig> {
    use lim_config::{ConfigLoader, ProjectPaths, XdgPaths};

    // Detect paths. If detection fails, we can't load role configs — fall
    // back to default. This is non-fatal.
    let xdg = XdgPaths::detect().map_err(|e| anyhow::anyhow!("XDG path detection failed: {e}"))?;
    let project = ProjectPaths::detect()
        .map_err(|e| anyhow::anyhow!("project path detection failed: {e}"))?;

    let loader = ConfigLoader::new(xdg, project);
    let loaded = loader.load().map_err(|e| anyhow::anyhow!("config load failed: {e}"))?;

    loaded
        .agent_roles
        .get("reviewer")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("reviewer role config not found"))
}

/// Load project context files (AGENTS.md, etc.) and concatenate them into
/// a single string ready for `PromptBuilder::with_project_context`.
///
/// This uses the `[context]` section of the merged config. When no `[context]`
/// section exists, falls back to the default (AGENTS.md at workspace root).
/// When `[context] enabled = false`, returns empty string.
///
/// Returns an empty string if config or path detection fails (non-fatal).
fn load_project_context() -> anyhow::Result<String> {
    use lim_config::{ConfigLoader, ProjectPaths, XdgPaths, load_context_files};

    let xdg = XdgPaths::detect()?;
    let project = ProjectPaths::detect()?;
    let loader = ConfigLoader::new(xdg, project.clone());
    let loaded = loader.load()?;

    let segments = load_context_files(loaded.workspace.context.as_ref(), &project);

    // Concatenate all segments with clear separators so the LLM can
    // distinguish multiple project context files.
    if segments.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::new();
    for seg in &segments {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("--- ");
        out.push_str(&seg.label);
        out.push_str(" ---\n");
        out.push_str(&seg.content);
    }
    Ok(out)
}

/// Convert a config-layer `AgentRoleConfig` to a harness-layer `AgentRoleSpec`.
///
/// This is the boundary between `lim-config` (product/config layer) and
/// `lim-agent` (harness layer): we strip the fields that have already been
/// resolved by the caller (`role`, `provider`, `workspace_path`) and convert
/// `tools_whitelist` from `Vec` to `Option<Vec>` (empty → None).
fn role_config_to_spec(cfg: &lim_config::schema::AgentRoleConfig) -> AgentRoleSpec {
    AgentRoleSpec {
        system_prompt: cfg.system_prompt.clone(),
        model: cfg.model.clone(),
        tools_whitelist: if cfg.tools_whitelist.is_empty() {
            None
        } else {
            Some(cfg.tools_whitelist.clone())
        },
        max_turns: cfg.max_turns,
    }
}

/// The default reviewer spec used when no `.lim/agents/reviewer.toml` exists.
///
/// This preserves the pre-P4.5 behavior: a hard-coded reviewer system prompt
/// with the model resolved from CLI flags / provider default.
fn default_reviewer_spec(model: &str) -> AgentRoleSpec {
    AgentRoleSpec {
        system_prompt: Some(
            "You are an independent code reviewer. Inspect the workspace and \
             determine whether the stated goal has been achieved. You have \
             tool access (read files, run commands). Be thorough but concise. \
             Always end with a verdict on the first line: PASS, FAIL: <reason>, \
             or NEEDS_HUMAN."
                .to_string(),
        ),
        model: Some(model.to_string()),
        tools_whitelist: None,
        max_turns: None,
    }
}

/// Build a `LoopRunner` from CLI args. Shared by `--goal` and `--resume`.
async fn build_loop_runner(
    verifier_cfg: VerifierCliConfig,
    max_sessions: Option<usize>,
    max_tokens: Option<u64>,
    cli_provider: Option<String>,
    cli_model: Option<String>,
    trust: TrustArg,
) -> anyhow::Result<LoopRunner> {
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
    let provider =
        create_provider(instance).map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;

    println!("Provider: {provider_name}");
    if !model.is_empty() {
        println!("Model:    {model}");
    }

    // Platform + executor + tools
    let platform = PlatformInfo::detect();
    print_platform_info(&platform);
    let executor = build_executor(trust, &platform);
    let registry = Arc::new(ToolRegistry::new());
    lim_tools::register_builtins(&registry, executor)?;

    // StateLedger — cross-session state in .lim/loop/
    // Built before the factory so we can register loop-control tools that
    // share the ledger with the runner. The agent can call these tools to
    // introspect the goal it is currently working on (AI-native introspection).
    let project_root = Utf8PathBuf::from(".");
    let ledger = StateLedger::new(project_root.clone());
    // Register loop-control tools with a shared clone of the ledger.
    lim_tools::register_loop_control_tools(&registry, Arc::new(ledger.clone()))?;
    println!("Registered {} tools (incl. loop-control)", registry.schemas().len(),);

    // AgentFactory — each session gets a fresh AgentLoop + context window.
    // Clone the registry Arc so it remains available for the reviewer factory
    // (when `--verify reviewer` is used).
    let inject = InjectInfo::new(platform);

    // Load project context files (AGENTS.md, etc.) based on config.
    // Non-fatal: if config loading fails, we just skip context files.
    let project_context = load_project_context().unwrap_or_else(|e| {
        tracing::debug!(error = %e, "failed to load project context, skipping");
        String::new()
    });

    let mut prompt_builder = PromptBuilder::new(inject.clone());
    if !project_context.is_empty() {
        prompt_builder = prompt_builder.with_project_context(project_context);
    }
    let agent_config = AgentConfig {
        model,
        system_prompt: Some(inject.system_prompt_fragment()),
        ..AgentConfig::default()
    };
    let factory = AgentFactory::new(
        provider,
        registry.clone(),
        agent_config,
        Some(ContextConfig::default()),
        project_root.join("sessions"),
    )
    .with_prompt_builder(prompt_builder);

    // Verifier — four modes:
    //   1. `--verify reviewer`          → ReviewerVerifier (independent reviewer agent)
    //   2. `--verify llm`               → LlmVerifier (single LLM call, no tools)
    //   3. `--verify "<command>"`       → CommandVerifier (deterministic, preferred)
    //   4. (omitted)                    → AlwaysPassVerifier (one-shot)
    //
    // `reviewer` and `llm` both support an independent evaluator provider/model
    // (configured via --verifier-provider / --verifier-model) to avoid the
    // main agent's reasoning bias — the "independent evaluator" pattern from
    // Claude Code's Loop Engineering. `reviewer` goes further: it spawns a
    // full agent with tools and fresh context ("second agent code review").
    let verifier: Box<dyn Verifier> = match verifier_cfg.verify.as_deref() {
        Some("reviewer") => {
            // Independent reviewer factory — fresh context, own provider/model,
            // shares the tool registry so the reviewer can inspect the workspace.
            //
            // Role config resolution (P4.5):
            //   1. Try to load `.lim/agents/reviewer.toml` (or global) via ConfigLoader.
            //   2. If found, use `ReviewerVerifier::from_role_spec` — this lets users
            //      customize the reviewer's system_prompt / model / tools_whitelist
            //      declaratively without touching CLI flags.
            //   3. If not found, fall back to the hard-coded default reviewer config
            //      (backward compat — same behavior as before P4.5).
            //
            // CLI flags `--verifier-provider` / `--verifier-model` always override
            // the role config's provider/model (they are explicit user intent).
            let v_provider_name = config
                .resolve_provider_name(verifier_cfg.verifier_provider.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let v_instance = config.provider(v_provider_name).ok_or_else(|| {
                lim_core::error::LimError::Config(format!(
                    "verifier provider `{v_provider_name}` not found in .lim/config.toml"
                ))
            })?;
            let v_model = config
                .resolve_model(v_instance, verifier_cfg.verifier_model.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let v_provider = create_provider(v_instance)
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;

            println!("Reviewer provider: {v_provider_name}");
            if !v_model.is_empty() {
                println!("Reviewer model:    {v_model}");
            }

            // Try to load reviewer role config from `.lim/agents/reviewer.toml`
            // (or global `~/.config/lim/agents/reviewer.toml` if agents=hybrid).
            // If found, role config provides system_prompt / tools_whitelist /
            // max_turns. Model priority: CLI flag > role config > provider default.
            let role_spec = match load_reviewer_role_config() {
                Ok(role_cfg) => {
                    let model = if verifier_cfg.verifier_model.is_none() {
                        role_cfg.model.clone().unwrap_or_else(|| v_model.clone())
                    } else {
                        v_model.clone()
                    };
                    let spec = role_config_to_spec(&role_cfg);
                    AgentRoleSpec { model: Some(model), ..spec }
                }
                Err(e) => {
                    if !e.to_string().contains("not found") {
                        eprintln!("warning: failed to load reviewer role config: {e}");
                    }
                    default_reviewer_spec(&v_model)
                }
            };

            Box::new(ReviewerVerifier::from_role_spec(
                role_spec,
                v_provider,
                registry,
                Some(ContextConfig::default()),
                project_root.join("sessions"),
                verifier_cfg.verifier_instructions,
            ))
        }
        Some("llm") => {
            let v_provider_name = config
                .resolve_provider_name(verifier_cfg.verifier_provider.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let v_instance = config.provider(v_provider_name).ok_or_else(|| {
                lim_core::error::LimError::Config(format!(
                    "verifier provider `{v_provider_name}` not found in .lim/config.toml"
                ))
            })?;
            let v_model = config
                .resolve_model(v_instance, verifier_cfg.verifier_model.as_deref())
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;
            let v_provider = create_provider(v_instance)
                .map_err(|e| lim_core::error::LimError::Config(e.to_string()))?;

            println!("Verifier provider: {v_provider_name}");
            if !v_model.is_empty() {
                println!("Verifier model:    {v_model}");
            }

            Box::new(LlmVerifier::new(v_provider, v_model, verifier_cfg.verifier_instructions))
        }
        Some(cmd) => Box::new(CommandVerifier::new(cmd)),
        None => Box::new(AlwaysPassVerifier),
    };

    // Budget — None means unlimited
    let budget = Budget { max_sessions, max_total_tokens: max_tokens };

    // loop_dir enables LoopEvent persistence (`.lim/loop/<goal_id>/events.jsonl`)
    // and ReviewerVerifier event persistence (`.lim/loop/<goal_id>/reviews/`).
    let loop_dir = project_root.join(".lim").join("loop");
    Ok(LoopRunner::new(factory, ledger, verifier, budget).with_loop_dir(loop_dir))
}

/// Print the final result of a Goal Loop run.
async fn print_goal_result(
    goal_id: &GoalId,
    result: GoalResult,
    ledger: &dyn LedgerBackend,
) -> anyhow::Result<()> {
    println!("\n--- Goal Loop Result ---");
    println!("Goal ID: {goal_id}");
    match result {
        GoalResult::Success => println!("Status: SUCCESS"),
        GoalResult::BudgetExhausted => println!("Status: BUDGET EXHAUSTED"),
        GoalResult::NeedsHuman(reason) => println!("Status: NEEDS HUMAN — {reason}"),
        GoalResult::Failed(reason) => println!("Status: FAILED — {reason}"),
        GoalResult::Cancelled => println!("Status: CANCELLED"),
    }

    // Show final state
    let state = ledger.load(goal_id).await?;
    println!("Sessions run: {}", state.session_count);
    println!("Total tokens: {}", state.total_tokens);
    println!("State file:   .lim/loop/{goal_id}/state.json");
    println!();
    println!("Resume with:  lim loop --resume {goal_id}");
    println!("Status with:  lim loop --status {goal_id}");

    Ok(())
}

/// `lim loop --list` — list all goals in the workspace.
async fn list_loop_goals() -> anyhow::Result<()> {
    let ledger = StateLedger::new(Utf8PathBuf::from("."));
    let goals = ledger.list_goals().await?;

    if goals.is_empty() {
        println!("No goals found in .lim/loop/");
        println!("Start one with: lim loop --goal \"...\" --verify \"cargo test\"");
        return Ok(());
    }

    println!("Found {} goal(s):", goals.len());
    println!();
    println!("{:<26} {:<12} {:>8} {:>12}  GOAL", "GOAL_ID", "STATUS", "SESSIONS", "TOKENS");
    for (goal_id, state) in &goals {
        let status = format_goal_status_short(&state.status);
        let goal_preview = if state.goal.len() > 50 {
            format!("{}...", &state.goal[..47])
        } else {
            state.goal.clone()
        };
        println!(
            "{goal_id:<26} {status:<12} {:>8} {:>12}  {goal_preview}",
            state.session_count, state.total_tokens,
        );
    }
    println!();
    println!("Resume with:  lim loop --resume <GOAL_ID>");
    println!("Details with: lim loop --status <GOAL_ID>");
    Ok(())
}

/// `lim loop --status <ID>` — show detailed status of one goal.
async fn show_loop_goal_status(goal_id_str: &str) -> anyhow::Result<()> {
    let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
        lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
    })?;
    let ledger = StateLedger::new(Utf8PathBuf::from("."));
    let state = ledger.load(&goal_id).await?;

    println!("Goal ID:      {goal_id}");
    println!("Goal:         {}", state.goal);
    println!("Status:       {}", format_goal_status(&state.status));
    println!("Sessions:     {}", state.session_count);
    println!("Turns:        {}", state.turn_count);
    println!("Tokens:       {}", state.total_tokens);
    println!("Created:      {}", format_unix_seconds(state.created_at));
    println!("Updated:      {}", format_unix_seconds(state.updated_at));
    if let Some(sid) = state.last_session_id {
        println!("Last session: {sid}");
    }
    println!();
    println!("State file: .lim/loop/{goal_id}/state.json");
    println!("Goal file:  .lim/loop/{goal_id}/goal.md");
    if !state.progress.is_empty() {
        println!();
        println!("--- Progress ---");
        println!("{}", state.progress);
    }
    Ok(())
}

/// Format `GoalStatus` for display in `--status`.
fn format_goal_status(status: &GoalStatus) -> String {
    match status {
        GoalStatus::Running => "running".to_string(),
        GoalStatus::Paused => "paused".to_string(),
        GoalStatus::Success => "success".to_string(),
        GoalStatus::Failed(reason) => format!("failed: {reason}"),
        GoalStatus::NeedsHuman(reason) => format!("needs_human: {reason}"),
    }
}

/// Format `GoalStatus` for the table in `--list` (short, no reason text).
fn format_goal_status_short(status: &GoalStatus) -> &'static str {
    match status {
        GoalStatus::Running => "running",
        GoalStatus::Paused => "paused",
        GoalStatus::Success => "success",
        GoalStatus::Failed(_) => "failed",
        GoalStatus::NeedsHuman(_) => "needs_human",
    }
}

/// Format a Unix timestamp (seconds) as a human-readable UTC string.
fn format_unix_seconds(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = unix_to_ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

// ---------------------------------------------------------------------------
// Loop Engineering daemon (`lim loop-daemon ...`)
// ---------------------------------------------------------------------------

/// Default storage root for the daemon ledger (relative to CWD).
const DEFAULT_DAEMON_STORAGE: &str = ".lim/daemon";

/// Resolve a storage root from CLI arg or default.
fn resolve_storage_root(arg: Option<Utf8PathBuf>) -> Utf8PathBuf {
    arg.unwrap_or_else(|| Utf8PathBuf::from(DEFAULT_DAEMON_STORAGE))
}

/// Dispatch a `lim loop-daemon <action>` subcommand.
async fn run_loop_daemon(action: DaemonAction) -> anyhow::Result<()> {
    match action {
        DaemonAction::Start { storage } => run_daemon_start(storage).await,
        DaemonAction::Submit { goal, workspace, verify, storage } => {
            run_daemon_submit(goal, workspace, verify, storage).await
        }
        DaemonAction::List { storage } => run_daemon_list(storage).await,
        DaemonAction::Status { goal_id, storage } => run_daemon_status(goal_id, storage).await,
        DaemonAction::Pause { goal_id, reason, storage } => {
            run_daemon_pause(goal_id, reason, storage).await
        }
        DaemonAction::Resume { goal_id, storage } => run_daemon_resume(goal_id, storage).await,
        DaemonAction::Sync { goal_id, workspace, storage } => {
            run_daemon_sync(goal_id, workspace, storage).await
        }
        DaemonAction::Shutdown => {
            eprintln!("shutdown: not supported in the current in-process daemon mode");
            eprintln!(
                "(In a future IPC mode, this command will send a Shutdown command to the running daemon.)"
            );
            Ok(())
        }
    }
}

/// `lim goal <action>` — dispatch goal-management actions to the same
/// handlers as `lim loop-daemon`. This is the preferred alias; `lim
/// loop-daemon <action>` remains as a backward-compatible alias.
async fn run_goal(action: GoalAction) -> anyhow::Result<()> {
    match action {
        GoalAction::Submit { goal, workspace, verify, storage } => {
            run_daemon_submit(goal, workspace, verify, storage).await
        }
        GoalAction::List { storage } => run_daemon_list(storage).await,
        GoalAction::Status { goal_id, storage } => run_daemon_status(goal_id, storage).await,
        GoalAction::Pause { goal_id, reason, storage } => {
            run_daemon_pause(goal_id, reason, storage).await
        }
        GoalAction::Resume { goal_id, storage } => run_daemon_resume(goal_id, storage).await,
        GoalAction::Sync { goal_id, workspace, storage } => {
            run_daemon_sync(goal_id, workspace, storage).await
        }
    }
}

/// `lim loop-daemon start` — start the in-process daemon server.
///
/// Spawns the `DaemonServer` as a background task and prints a "running"
/// message. The server processes commands until a `Shutdown` is received or
/// the task is killed. Since there's no IPC transport yet, this is mainly
/// useful for integration tests; CLI clients should use the other subcommands
/// directly (they operate on the same storage).
///
/// **Deprecated**: use `lim serve --goal-daemon` instead, which provides the
/// same goal-management capability under the unified serve entry point.
async fn run_daemon_start(storage: Option<Utf8PathBuf>) -> anyhow::Result<()> {
    eprintln!(
        "[deprecated] `lim loop-daemon start` is deprecated; use `lim serve --goal-daemon` instead."
    );
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root.clone());
    let (server, handle) = DaemonServer::new(ledger);

    println!("Starting daemon server (in-process)");
    println!("  Storage: {storage_root}");
    println!(
        "  Status:  ready (no IPC transport — other `loop-daemon` subcommands operate directly on storage)"
    );
    println!();
    println!("Press Ctrl+C to shut down.");

    // Spawn the server task.
    let server_task = tokio::spawn(async move { server.run().await });

    // Wait for Ctrl+C.
    tokio::signal::ctrl_c().await?;

    // Send shutdown command (best-effort).
    let _ = handle.send(DaemonCommand::Shutdown).await;

    // Wait for the server task to finish.
    let _ = server_task.await;
    println!("\nDaemon server shut down.");
    Ok(())
}

/// `lim loop-daemon submit --goal "..." [--workspace ...] [--verify ...]`
async fn run_daemon_submit(
    goal: String,
    workspace: Option<Utf8PathBuf>,
    verify: Option<String>,
    storage: Option<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root.clone());

    let goal_id = ledger.create_goal(&goal).await?;

    println!("Submitted goal to daemon ledger:");
    println!("  Goal ID:  {goal_id}");
    println!("  Storage:  {storage_root}");
    println!("  Goal:     {goal}");
    if let Some(ws) = &workspace {
        ledger.bind_workspace(&goal_id, ws.clone(), true).await?;
        println!("  Workspace: {ws}");
    }
    if let Some(verify) = &verify {
        // NOTE: verifier_cmd is recorded but not yet executed by the daemon.
        // Future P4 work will integrate AgentFactory + LoopRunner into the daemon.
        println!(
            "  Verifier: {verify} (recorded; run `lim loop --resume {goal_id}` in the workspace to execute)"
        );
    }
    println!();
    println!("Next steps:");
    if workspace.is_some() {
        println!("  lim loop-daemon sync {goal_id}");
        println!("  cd <workspace> && lim loop --resume {goal_id}");
    } else {
        println!(
            "  (No workspace bound — use `lim loop-daemon sync --workspace <path> {goal_id}` to bind one.)"
        );
    }
    Ok(())
}

/// `lim loop-daemon list`
async fn run_daemon_list(storage: Option<Utf8PathBuf>) -> anyhow::Result<()> {
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root.clone());
    let goals = ledger.list_goals().await?;

    if goals.is_empty() {
        println!("No goals found in daemon ledger ({storage_root}).");
        return Ok(());
    }

    println!("Found {} goal(s) in daemon ledger:", goals.len());
    println!("  Storage: {storage_root}");
    println!();
    println!("{:<26} {:<12} {:>8} {:>12}  GOAL", "GOAL_ID", "STATUS", "SESSIONS", "TOKENS",);
    for (goal_id, state) in &goals {
        let status = format_goal_status_short(&state.status);
        let goal_preview = if state.goal.len() > 50 {
            format!("{}...", &state.goal[..47])
        } else {
            state.goal.clone()
        };
        println!(
            "{goal_id:<26} {status:<12} {:>8} {:>12}  {goal_preview}",
            state.session_count, state.total_tokens,
        );
    }
    Ok(())
}

/// `lim loop-daemon status <ID>`
async fn run_daemon_status(
    goal_id_str: String,
    storage: Option<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
        lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
    })?;
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root.clone());
    let state = ledger.load(&goal_id).await?;

    println!("Goal ID:      {goal_id}");
    println!("Goal:         {}", state.goal);
    println!("Status:       {}", format_goal_status(&state.status));
    println!("Sessions:     {}", state.session_count);
    println!("Turns:        {}", state.turn_count);
    println!("Tokens:       {}", state.total_tokens);
    println!("Created:      {}", format_unix_seconds(state.created_at));
    println!("Updated:      {}", format_unix_seconds(state.updated_at));
    if let Some(sid) = state.last_session_id {
        println!("Last session: {sid}");
    }
    println!("Storage:      {storage_root}/{goal_id}/");

    // Show workspace binding if any.
    if let Ok(Some(binding)) = ledger.workspace_binding(&goal_id).await {
        println!();
        println!("Workspace binding:");
        println!("  Root:    {}", binding.workspace_root);
        println!("  Primary: {}", binding.primary);
        println!("  Bound:   {}", format_unix_seconds(binding.bound_at));
    }
    Ok(())
}

/// `lim loop-daemon pause <ID> [--reason "..."]`
async fn run_daemon_pause(
    goal_id_str: String,
    reason: Option<String>,
    storage: Option<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
        lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
    })?;
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root);

    let mut state = ledger.load(&goal_id).await?;
    state.status = GoalStatus::Paused;
    ledger.save(&goal_id, &state).await?;

    println!("Paused goal: {goal_id}");
    if let Some(reason) = reason {
        println!("  Reason: {reason}");
    }
    println!("  Resume with: lim loop-daemon resume {goal_id}");
    Ok(())
}

/// `lim loop-daemon resume <ID>`
async fn run_daemon_resume(
    goal_id_str: String,
    storage: Option<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
        lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
    })?;
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root);

    let mut state = ledger.load(&goal_id).await?;
    if !matches!(state.status, GoalStatus::Paused) {
        return Err(lim_core::error::LimError::Config(format!(
            "goal {goal_id} is not paused (current: {})",
            format_goal_status(&state.status),
        ))
        .into());
    }
    state.status = GoalStatus::Running;
    ledger.save(&goal_id, &state).await?;

    println!("Resumed goal: {goal_id}");
    println!("  Note: this only flips the status flag. To actually run sessions,");
    println!(
        "  use `lim loop-daemon sync {goal_id}` then `lim loop --resume {goal_id}` in the workspace."
    );
    Ok(())
}

/// `lim loop-daemon sync <ID> [--workspace <path>]`
async fn run_daemon_sync(
    goal_id_str: String,
    workspace_override: Option<Utf8PathBuf>,
    storage: Option<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let goal_id: GoalId = goal_id_str.parse().map_err(|e| {
        lim_core::error::LimError::Config(format!("invalid goal ID `{goal_id_str}`: {e}"))
    })?;
    let storage_root = resolve_storage_root(storage);
    let ledger = DaemonLedger::new(storage_root.clone());

    let workspace_root = match workspace_override {
        Some(ws) => ws,
        None => match ledger.workspace_binding(&goal_id).await? {
            Some(binding) => binding.workspace_root,
            None => {
                return Err(lim_core::error::LimError::Config(format!(
                    "goal {goal_id} has no workspace binding; pass --workspace <path>",
                ))
                .into());
            }
        },
    };

    let dest = ledger.sync_to_workspace(&goal_id, &workspace_root).await?;

    println!("Synced goal: {goal_id}");
    println!("  From: {storage_root}/{goal_id}/");
    println!("  To:   {dest}");
    println!();
    println!("The goal is now visible to workspace-local tools:");
    println!("  cd {workspace_root}");
    println!("  lim loop --status {goal_id}");
    println!("  lim loop --resume {goal_id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// lim init (`lim init ...`)
// ---------------------------------------------------------------------------

/// `lim init` — initialize a lim workspace in the current directory.
///
/// Creates `.lim/` with a `config.toml` template and an `AGENTS.md` template.
/// The config template includes provider placeholders; `--provider` pre-fills
/// a specific provider section.
fn run_init(provider: Option<&str>, force: bool) -> anyhow::Result<()> {
    use std::fs;

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to get current directory: {e}"))?;
    let lim_dir = cwd.join(".lim");
    let config_path = lim_dir.join("config.toml");

    // Check for existing config (unless --force)
    if config_path.exists() && !force {
        eprintln!("Error: `.lim/config.toml` already exists. Use `--force` to overwrite.");
        std::process::exit(1);
    }

    // Create .lim/ directory
    fs::create_dir_all(&lim_dir)
        .map_err(|e| anyhow::anyhow!("failed to create `.lim/` directory: {e}"))?;

    // Generate config.toml template
    let config_content = generate_config_template(provider);
    fs::write(&config_path, &config_content)
        .map_err(|e| anyhow::anyhow!("failed to write `.lim/config.toml`: {e}"))?;

    eprintln!("Created: {}", config_path.display());

    // Generate AGENTS.md template (only if it doesn't exist)
    let agents_path = cwd.join("AGENTS.md");
    if !agents_path.exists() {
        let agents_content = generate_agents_template();
        fs::write(&agents_path, &agents_content)
            .map_err(|e| anyhow::anyhow!("failed to write `AGENTS.md`: {e}"))?;
        eprintln!("Created: {}", agents_path.display());
    } else {
        eprintln!("Skipped: AGENTS.md already exists");
    }

    eprintln!("\nWorkspace initialized. Next steps:");
    eprintln!("  1. Edit `.lim/config.toml` to fill in your API key");
    eprintln!("  2. Edit `AGENTS.md` to add project conventions");
    eprintln!("  3. Run `lim providers` to verify config loads");
    eprintln!("  4. Run `lim run --message \"hello\"` to test");

    Ok(())
}

/// Generate a config.toml template, optionally pre-filling a provider.
fn generate_config_template(provider: Option<&str>) -> String {
    match provider {
        Some("anthropic") => r#"# lim workspace configuration
# See: https://github.com/pale/lim for full docs

default_provider = "anthropic"

[providers.anthropic]
type = "anthropic"
# Set your API key here, or use api_key_env to read from an env var:
api_key = "sk-ant-..."
# api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

# Uncomment to add an OpenAI provider:
# [providers.openai]
# type = "openai-chat"
# api_key = "sk-..."
# default_model = "gpt-4o"

# Context files loaded into the system prompt.
# Default: AGENTS.md at workspace root.
# [context]
# enabled = true
# [[context.files]]
# name = "AGENTS.md"
# search = "workspace_root"

# lim serve configuration (protocol/mode defaults).
# [serve]
# protocol = "all"
# mode = "foreground"
# [serve.goal_daemon]
# enabled = true
# storage = ".lim/daemon"
"#
        .to_string(),

        Some("openai") => r#"# lim workspace configuration
# See: https://github.com/pale/lim for full docs

default_provider = "openai"

[providers.openai]
type = "openai-chat"
# Set your API key here, or use api_key_env to read from an env var:
api_key = "sk-..."
# api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"

# Uncomment to add an Anthropic provider:
# [providers.anthropic]
# type = "anthropic"
# api_key = "sk-ant-..."
# default_model = "claude-sonnet-4-20250514"

# Context files loaded into the system prompt.
# Default: AGENTS.md at workspace root.
# [context]
# enabled = true
# [[context.files]]
# name = "AGENTS.md"
# search = "workspace_root"

# lim serve configuration (protocol/mode defaults).
# [serve]
# protocol = "all"
# mode = "foreground"
# [serve.goal_daemon]
# enabled = true
# storage = ".lim/daemon"
"#
        .to_string(),

        _ => r#"# lim workspace configuration
# See: https://github.com/pale/lim for full docs

# Uncomment and fill in your provider:
# default_provider = "anthropic"

# [providers.anthropic]
# type = "anthropic"
# api_key = "sk-ant-..."
# api_key_env = "ANTHROPIC_API_KEY"
# default_model = "claude-sonnet-4-20250514"

# [providers.openai]
# type = "openai-chat"
# api_key = "sk-..."
# api_key_env = "OPENAI_API_KEY"
# default_model = "gpt-4o"

# Context files loaded into the system prompt.
# Default: AGENTS.md at workspace root.
# [context]
# enabled = true
# [[context.files]]
# name = "AGENTS.md"
# search = "workspace_root"

# lim serve configuration (protocol/mode defaults).
# [serve]
# protocol = "all"
# mode = "foreground"
# [serve.goal_daemon]
# enabled = true
# storage = ".lim/daemon"
"#
        .to_string(),
    }
}

/// Generate an AGENTS.md template.
fn generate_agents_template() -> String {
    r#"# Project Conventions

<!-- This file is loaded into the agent's system prompt by default.
     Edit it to describe your project's coding standards, architecture,
     and any rules the agent should follow. -->

## General

- Keep changes minimal and focused.
- Follow existing code style.

<!-- Add more sections as needed:
- Architecture
- Testing
- Git workflow
- Deployment
-->
"#
    .to_string()
}

// ---------------------------------------------------------------------------
// lim serve (`lim serve ...`)
// ---------------------------------------------------------------------------

/// `lim serve` — unified entry point for long-running server modes.
///
/// Parses CLI flags into [`lim_serve::ServeOptions`], constructs a
/// [`lim_serve::ServeRunner`], and runs it. Defaults for `--protocol` and
/// `--daemon` fall back to the `[serve]` config section when the flag is
/// omitted, then to built-in defaults (`all` / foreground).
#[allow(clippy::too_many_arguments)]
async fn run_serve(
    protocol: Option<&str>,
    daemon: bool,
    system: Option<&str>,
    workspace: Option<Utf8PathBuf>,
    config: Option<Utf8PathBuf>,
    endpoint: Option<String>,
    auth_token: Option<String>,
) -> anyhow::Result<()> {
    use lim_serve::{RunMode, ServeOptions, ServeRunner, SystemAction};

    // Load workspace config to resolve [serve] defaults. Non-fatal: if
    // config loading fails, we fall back to built-in defaults.
    let serve_cfg = load_serve_config().unwrap_or_else(|e| {
        tracing::debug!(error = %e, "failed to load [serve] config, using built-in defaults");
        None
    });

    // Resolve effective protocol: CLI flag > [serve].protocol > "all".
    let protocol_str = protocol
        .or_else(|| serve_cfg.as_ref().and_then(|c| c.protocol.as_deref()))
        .unwrap_or("all");
    let protocol_set = parse_protocol_set(protocol_str)?;

    // Resolve effective mode: --daemon > --system > [serve].mode > foreground.
    let mode = if daemon {
        RunMode::Daemon
    } else if let Some(action_str) = system {
        let action = SystemAction::parse(action_str).ok_or_else(|| {
            lim_core::error::LimError::Config(format!(
                "invalid --system action `{action_str}` (expected: install/uninstall/start/stop/status/restart)"
            ))
        })?;
        RunMode::System(action)
    } else if serve_cfg
        .as_ref()
        .and_then(|c| c.mode.as_deref())
        .map(|m| m.eq_ignore_ascii_case("daemon"))
        .unwrap_or(false)
    {
        RunMode::Daemon
    } else {
        RunMode::Foreground
    };

    let options = ServeOptions {
        protocol: protocol_set,
        mode,
        workspace: workspace.map(|p| p.as_std_path().to_path_buf()),
        config_path: config.map(|p| p.as_std_path().to_path_buf()),
        bind_endpoint: endpoint,
        auth_token,
    };

    eprintln!("Mode:     {}", format_run_mode(&options.mode));
    eprintln!("Protocol: {}", format_protocol_set(&options.protocol));

    // Whether the goal daemon should be active. Defaults to true.
    let goal_daemon_enabled = serve_cfg.as_ref().map(|c| c.goal_daemon.enabled).unwrap_or(true);
    eprintln!(
        "Goal daemon: {}",
        if goal_daemon_enabled { "enabled (built-in)" } else { "disabled" }
    );

    let runner = ServeRunner::new(options);
    runner.run().await?;
    Ok(())
}

/// Load the `[serve]` section from the workspace config, if any.
///
/// Returns `Ok(None)` when no config file is found or the `[serve]` section
/// is absent. Errors from config loading (parse failures etc.) are propagated
/// so the caller can decide whether to warn or abort.
fn load_serve_config() -> anyhow::Result<Option<lim_config::ServeConfig>> {
    use lim_config::{ConfigLoader, ProjectPaths, XdgPaths};
    let xdg = XdgPaths::detect()?;
    let project = ProjectPaths::detect()?;
    let loader = ConfigLoader::new(xdg, project);
    let loaded = loader.load()?;
    Ok(loaded.workspace.serve)
}

/// Parse the `--protocol` CLI string into a `ProtocolSet`.
fn parse_protocol_set(s: &str) -> anyhow::Result<lim_serve::ProtocolSet> {
    use lim_serve::{ProtocolKind, ProtocolSet};
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return Ok(ProtocolSet::All);
    }
    if s.eq_ignore_ascii_case("none") {
        return Ok(ProtocolSet::None);
    }
    match s.to_ascii_lowercase().as_str() {
        "mcp" => Ok(ProtocolSet::Single(ProtocolKind::Mcp)),
        "a2a" => Ok(ProtocolSet::Single(ProtocolKind::A2a)),
        other => Err(lim_core::error::LimError::Config(format!(
            "unknown protocol `{other}` (expected: all, mcp, a2a, none)"
        ))
        .into()),
    }
}

/// Format a `RunMode` for display.
fn format_run_mode(mode: &lim_serve::RunMode) -> String {
    use lim_serve::RunMode;
    match mode {
        RunMode::Foreground => "foreground".to_string(),
        RunMode::Daemon => "daemon".to_string(),
        RunMode::System(action) => format!("system({action:?})"),
    }
}

/// Format a `ProtocolSet` for display.
fn format_protocol_set(set: &lim_serve::ProtocolSet) -> String {
    use lim_serve::ProtocolSet;
    match set {
        ProtocolSet::None => "none".to_string(),
        ProtocolSet::Single(p) => format!("{p:?}").to_lowercase(),
        ProtocolSet::All => "all".to_string(),
    }
}
