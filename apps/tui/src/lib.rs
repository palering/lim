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
mod event;
mod terminal;
mod ui;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use lim_agent::agent_loop::{AgentConfig, AgentLoop};
use lim_agent::registry::ToolRegistry;
use lim_llm::providers::config::LimConfig;
use lim_llm::Provider;

pub use app::{App, ApprovalMode};
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
    } = config;

    let agent_config = AgentConfig {
        model: model.clone(),
        ..AgentConfig::default()
    };

    let agent = AgentLoop::new(provider, registry, event_tx.clone(), cancel.clone())
        .with_config(agent_config);

    let mut app = App::new(agent, provider_label, model, lim_config, event_tx, cancel);
    app.run().await
}
