//! Scheduler: spawns and tracks multiple agent tasks.
//!
//! The `Scheduler` is a thin orchestration layer over `AgentLoop` and
//! `AgentTransport`. It does NOT implement smart task assignment (that's
//! a future P4 concern) — it just provides a clean way to:
//!
//! 1. Spawn an agent as a tokio task with a cancellation token
//! 2. Track its lifecycle (`Running` → `Done` / `Cancelled` / `Failed`)
//! 3. Cancel agents individually or all-at-once
//!
//! ## Design Notes
//!
//! The scheduler deliberately does NOT own `AgentLoop` instances directly.
//! Instead, the caller provides a `SpawnOptions` struct with a closure that
//! constructs the agent task. This keeps the scheduler agnostic to the
//! agent's configuration (provider, tools, hooks, etc.) and avoids coupling
//! to `AgentFactory` (which is owned by `loop_runner`).

use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use lim_core::error::Result;

use super::transport::AgentId;

/// Options for spawning an agent task.
///
/// The caller provides a `run` closure that contains the agent's main loop
/// (e.g. `agent.run(initial_msg).await`). The closure receives a
/// `CancellationToken` so the scheduler can cancel the agent.
pub struct SpawnOptions {
    /// Human-readable ID for the agent (e.g. `"researcher"`, `"impl-1"`).
    pub id: AgentId,
    /// The agent's main task. Receives a cancellation token.
    ///
    /// The type is intentionally complex (boxed `FnOnce` returning a pinned
    /// boxed future) because that's the idiomatic way to store an
    /// `async move { ... }` closure as a struct field. We allow
    /// `clippy::type_complexity` here rather than introducing a type alias
    /// that would obscure the signature.
    #[allow(clippy::type_complexity)]
    pub run: Box<
        dyn FnOnce(
                CancellationToken,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
            + Send,
    >,
}

/// Lifecycle status of a spawned agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent task is currently running.
    Running,
    /// Agent completed successfully.
    Done,
    /// Agent was cancelled via its cancellation token.
    Cancelled,
    /// Agent task failed with an error.
    Failed,
}

/// Handle to a spawned agent task.
pub struct AgentHandle {
    /// The agent's ID (also the key in `Scheduler::agents`).
    pub id: AgentId,
    /// Cancellation token — call `.cancel()` to stop the agent.
    pub cancel: CancellationToken,
    /// JoinHandle for the underlying tokio task.
    pub join: JoinHandle<Result<()>>,
    /// Last known status (updated by `Scheduler::status`).
    pub status: AgentStatus,
}

/// Multi-agent scheduler: spawns, tracks, and cancels agent tasks.
///
/// ## Example
///
/// ```ignore
/// let mut scheduler = Scheduler::new();
/// let opts = SpawnOptions {
///     id: AgentId::new("researcher"),
///     run: Box::new(|cancel| Box::pin(async move {
///         // ... agent.run(msg).await ...
///         Ok(())
///     })),
/// };
/// scheduler.spawn(opts).await?;
/// let status = scheduler.status(&AgentId::new("researcher")).await;
/// scheduler.cancel_all().await;
/// ```
pub struct Scheduler {
    agents: Arc<Mutex<std::collections::HashMap<AgentId, AgentHandle>>>,
}

impl Scheduler {
    /// Create a new empty scheduler.
    pub fn new() -> Self {
        Self { agents: Arc::new(Mutex::new(std::collections::HashMap::new())) }
    }

    /// Spawn an agent task.
    pub async fn spawn(&self, opts: SpawnOptions) -> Result<()> {
        let id = opts.id.clone();
        let id_for_task = id.clone();
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        info!(agent = %id, "spawning agent task");

        let future = (opts.run)(cancel_for_task);
        let join = tokio::spawn(async move {
            let result = future.await;
            match &result {
                Ok(()) => debug!(agent = %id_for_task, "agent task completed"),
                Err(e) => warn!(agent = %id_for_task, error = %e, "agent task failed"),
            }
            result
        });

        let handle = AgentHandle { id, cancel, join, status: AgentStatus::Running };
        self.agents.lock().await.insert(opts.id, handle);
        Ok(())
    }

    /// Get the status of an agent.
    ///
    /// If the agent task has finished but the status hasn't been polled yet,
    /// this method updates it by checking the `JoinHandle`.
    pub async fn status(&self, id: &AgentId) -> Option<AgentStatus> {
        let mut agents = self.agents.lock().await;
        let handle = agents.get_mut(id)?;
        if handle.status == AgentStatus::Running {
            // Check if the task has finished without blocking.
            if handle.join.is_finished() {
                // We can't get the result here (would require `await` on
                // `join` which needs `&mut`), so just mark as Done. The
                // actual result is logged by the spawned task.
                handle.status = AgentStatus::Done;
            }
        }
        Some(handle.status)
    }

    /// Cancel a single agent by ID.
    pub async fn cancel(&self, id: &AgentId) -> bool {
        let mut agents = self.agents.lock().await;
        if let Some(handle) = agents.get_mut(id) {
            handle.cancel.cancel();
            handle.status = AgentStatus::Cancelled;
            info!(agent = %id, "agent cancelled");
            true
        } else {
            false
        }
    }

    /// Cancel all running agents.
    pub async fn cancel_all(&self) {
        let mut agents = self.agents.lock().await;
        for (id, handle) in agents.iter_mut() {
            if handle.status == AgentStatus::Running {
                handle.cancel.cancel();
                handle.status = AgentStatus::Cancelled;
                info!(agent = %id, "agent cancelled (cancel_all)");
            }
        }
    }

    /// Wait for an agent to finish (blocks).
    pub async fn join(&self, id: &AgentId) -> Option<Result<()>> {
        // We need to take the handle out of the map temporarily to await
        // the JoinHandle (which requires `&mut`), then put it back.
        let handle = {
            let mut agents = self.agents.lock().await;
            agents.remove(id)
        };
        let handle = handle?;
        let result = handle.join.await.ok()?;
        // Update status based on result
        let status = match &result {
            Ok(()) => AgentStatus::Done,
            Err(_) => AgentStatus::Failed,
        };
        let mut agents = self.agents.lock().await;
        let new_handle = AgentHandle {
            id: handle.id.clone(),
            cancel: handle.cancel,
            join: tokio::spawn(async { Ok(()) }), // dummy handle (already joined)
            status,
        };
        agents.insert(handle.id.clone(), new_handle);
        Some(result)
    }

    /// List all agent IDs currently tracked by the scheduler.
    pub async fn list(&self) -> Vec<AgentId> {
        self.agents.lock().await.keys().cloned().collect()
    }

    /// Number of agents currently tracked.
    pub async fn len(&self) -> usize {
        self.agents.lock().await.len()
    }

    /// Is the scheduler tracking zero agents?
    pub async fn is_empty(&self) -> bool {
        self.agents.lock().await.is_empty()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn spawn_and_cancel_agent() {
        let scheduler = Scheduler::new();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = Arc::clone(&cancelled);

        let opts = SpawnOptions {
            id: AgentId::new("worker-1"),
            run: Box::new(move |cancel| {
                Box::pin(async move {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            cancelled_clone.store(true, Ordering::SeqCst);
                            Ok(())
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                            Ok(())
                        }
                    }
                })
            }),
        };

        scheduler.spawn(opts).await.unwrap();
        assert_eq!(scheduler.len().await, 1);

        // Cancel the agent
        assert!(scheduler.cancel(&AgentId::new("worker-1")).await);
        // Give the task a moment to process the cancellation
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(cancelled.load(Ordering::SeqCst));
        let status = scheduler.status(&AgentId::new("worker-1")).await;
        assert_eq!(status, Some(AgentStatus::Cancelled));
    }

    #[tokio::test]
    async fn cancel_all_cancels_every_agent() {
        let scheduler = Scheduler::new();

        for name in ["a", "b", "c"] {
            let opts = SpawnOptions {
                id: AgentId::new(name),
                run: Box::new(|cancel| {
                    Box::pin(async move {
                        let _ = cancel.cancelled().await;
                        Ok(())
                    })
                }),
            };
            scheduler.spawn(opts).await.unwrap();
        }

        assert_eq!(scheduler.len().await, 3);
        scheduler.cancel_all().await;

        for name in ["a", "b", "c"] {
            let status = scheduler.status(&AgentId::new(name)).await;
            assert_eq!(status, Some(AgentStatus::Cancelled));
        }
    }

    #[tokio::test]
    async fn list_returns_all_agent_ids() {
        let scheduler = Scheduler::new();

        for name in ["x", "y", "z"] {
            let opts = SpawnOptions {
                id: AgentId::new(name),
                run: Box::new(|_| Box::pin(async { Ok(()) })),
            };
            scheduler.spawn(opts).await.unwrap();
        }

        let mut ids = scheduler.list().await;
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(ids.len(), 3);
    }

    #[tokio::test]
    async fn join_waits_for_completion() {
        let scheduler = Scheduler::new();
        let opts = SpawnOptions {
            id: AgentId::new("quick"),
            run: Box::new(|_| Box::pin(async { Ok(()) })),
        };
        scheduler.spawn(opts).await.unwrap();

        let result = scheduler.join(&AgentId::new("quick")).await;
        assert!(matches!(result, Some(Ok(()))));
    }
}
