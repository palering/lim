//! lim-tui — Terminal UI frontend for lim.
//!
//! Interactive terminal interface for the lim agent runtime. Built on
//! ratatui + crossterm. The TUI subscribes to the agent's event broadcast
//! channel and renders streaming output without blocking the agent loop.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐   broadcast::Sender<EventEnvelope>
//! │  AgentLoop  │ ─────────────────────────────────┐
//! │ (tokio task)│                                   │
//! └─────────────┘                                   ▼
//!                                              ┌─────────┐
//! ┌─────────────┐   mpsc::Receiver<TuiEvent>   │   App   │  (state)
//! │   Input     │ ───────────────────────────► │ (update)│
//! │ (crossterm) │                              └────┬────┘
//! └─────────────┘                                   │
//!                                              ┌────▼────┐
//!                                              │  Render │  (60fps)
//!                                              └─────────┘
//! ```
//!
//! The main loop runs three concurrent tasks:
//! 1. **Input poller** — reads crossterm keyboard events, converts to `TuiEvent::Key`.
//! 2. **Agent event bridge** — subscribes to `broadcast::Receiver<EventEnvelope>`,
//!    converts to `TuiEvent::Agent`.
//! 3. **Render loop** — redraws the screen at ~60fps (or on event).

mod app;
mod approval;
mod event;
mod terminal;
mod ui;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::{AgentConfig, AgentLoop};
use lim_agent::registry::ToolRegistry;
use lim_llm::providers::config::LimConfig;
use lim_llm::Provider;

pub use app::App;
pub use approval::{
    ApprovalHooks, ApprovalMode, ApprovalRequest, ApprovalResponse, SharedToolLists, ToolLists,
};
pub use event::TuiEvent;

/// Configuration for the TUI session.
pub struct TuiConfig {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub provider_label: String,
    pub registry: Arc<ToolRegistry>,
    /// Full provider config, so the TUI can create new provider instances
    /// at runtime when the user runs `/provider <name>`.
    pub lim_config: LimConfig,
    pub event_tx: broadcast::Sender<lim_core::event::EventEnvelope>,
    pub cancel: CancellationToken,
    /// Trust level for the session — drives the Trust × ApprovalMode linkage.
    /// When `Permissive`, the approval layer enforces `Always` mode.
    pub trust_level: lim_core::TrustLevel,
}

/// Run the TUI. Takes ownership of the agent setup; the caller provides the
/// provider, registry, event channel, and cancellation token. The TUI spawns
/// the agent loop internally so it can drive it interactively.
pub async fn run(config: TuiConfig) -> Result<()> {
    let TuiConfig {
        provider,
        model,
        provider_label,
        registry,
        lim_config,
        event_tx,
        cancel,
        trust_level,
    } = config;

    let agent_config = AgentConfig {
        model: model.clone(),
        ..AgentConfig::default()
    };

    // Set up approval channel: ApprovalHooks sends ApprovalRequest to the TUI,
    // TUI responds via an embedded oneshot sender.
    let (approval_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(8);
    let shared_mode = Arc::new(Mutex::new(ApprovalMode::AllowNonScript));
    let shared_lists = Arc::new(Mutex::new(ToolLists::new()));
    // Build guard based on trust level — used for Permissive+Auto linkage.
    let guard: Arc<dyn lim_core::ToolGuard> = match trust_level {
        lim_core::TrustLevel::Permissive => Arc::new(lim_core::PermissiveGuard),
        lim_core::TrustLevel::Workspace => Arc::new(lim_core::WorkspaceGuard),
        lim_core::TrustLevel::Sandbox => Arc::new(lim_core::WorkspaceGuard), // Phase 4
    };
    let hooks = Arc::new(
        ApprovalHooks::new(
            approval_tx,
            shared_mode.clone(),
            Arc::clone(&registry),
            shared_lists.clone(),
        )
        .with_guard(guard),
    );

    let agent = AgentLoop::new(provider, registry, event_tx.clone(), cancel.clone())
        .with_config(agent_config)
        .with_hooks(hooks.clone());

    let mut app = App::new(
        agent,
        provider_label,
        model,
        lim_config,
        event_tx,
        cancel,
        approval_rx,
        shared_mode,
        shared_lists,
        hooks,
    );
    app.run().await
}
