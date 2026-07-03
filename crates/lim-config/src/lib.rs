//! lim-config: layered configuration with XDG Base Directory support.
//!
//! Provides the `ConfigLoader` which merges configuration from three layers:
//!
//! 1. **Global** — `~/.config/lim/config.toml` (XDG_CONFIG_HOME)
//! 2. **Project** — `<workspace>/.lim/config.toml` (overrides global)
//! 3. **CLI flags** — passed at call time (highest priority)
//!
//! ## XDG paths
//!
//! | Purpose | Path | Env override |
//! |---|---|---|
//! | Config | `~/.config/lim/` | `XDG_CONFIG_HOME` |
//! | Data | `~/.local/share/lim/` | `XDG_DATA_HOME` |
//! | Cache | `~/.cache/lim/` | `XDG_CACHE_HOME` |
//! | Runtime | `$XDG_RUNTIME_DIR/lim/` | `XDG_RUNTIME_DIR` |
//!
//! ## Containment Principle relaxation
//!
//! Before P4, lim enforced a strict Containment Principle: everything stayed
//! inside the project directory. P4 relaxes this for global configuration and
//! persistent data stores, as announced in `AGENTS.md`:
//!
//! > "This constraint will be relaxed when lim ships a real config system
//! > (e.g. `~/.config/lim/`) or persistent data store."
//!
//! Project-local state (`.lim/loop/`, `.lim/worktrees/`, `sessions/`) still
//! stays inside the project. Only global defaults and cross-project data
//! (global agent definitions, daemon state) move to XDG paths.

pub mod error;
pub mod loader;
pub mod paths;
pub mod schema;

pub use error::{ConfigError, ConfigResult};
pub use loader::ConfigLoader;
pub use paths::{ProjectPaths, XdgPaths};
pub use schema::WorkspaceConfig;
