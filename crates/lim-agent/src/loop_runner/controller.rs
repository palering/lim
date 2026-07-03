//! Controller registry: declarative goal templates (CRD analogy).
//!
//! A `Controller` is a recipe that turns a user's high-level intent (e.g.
//! "fix all clippy warnings") into the concrete triple `(goal, verifier,
//! budget)` that `LoopRunner` needs. This is analogous to a Kubernetes
//! Controller + CRD pair: the CRD defines the shape of a desired state, the
//! Controller reconciles it.
//!
//! ## Example
//!
//! ```ignore
//! let registry = ControllerRegistry::new()
//!     .register(FixClippyController)
//!     .register(WriteTestsController::new("cargo test"));
//!
//! // Look up by name and materialize
//! let recipe = registry.resolve("fix-clippy", "")?;
//! let (goal_id, result) = runner.start_goal(&recipe.goal).await?;
//! ```

use std::sync::Arc;

use crate::loop_runner::budget::Budget;

/// A recipe produced by a `Controller` — the concrete inputs for `LoopRunner`.
#[derive(Debug, Clone)]
pub struct GoalRecipe {
    /// The goal description to feed to `LoopRunner::start_goal`.
    pub goal: String,
    /// Optional verifier command (e.g. `"cargo clippy -- -D warnings"`).
    pub verifier_cmd: Option<String>,
    /// Budget for this goal. `Budget::unlimited()` if not specified.
    pub budget: Budget,
    /// Optional no-progress threshold override.
    pub no_progress_threshold: Option<usize>,
}

/// Trait for declarative goal controllers (CRD analogy).
///
/// Each controller knows how to materialize one type of goal. The registry
/// dispatches by name. The `spec` parameter is the user's free-form input
/// (e.g. `"src/auth/"` for a "fix-clippy-in-dir" controller).
pub trait Controller: Send + Sync {
    /// Controller name (e.g. `"fix-clippy"`, `"write-tests"`).
    fn name(&self) -> &str;

    /// Human-readable description of what this controller does.
    fn description(&self) -> &str;

    /// Materialize a goal recipe from a user spec.
    fn resolve(&self, spec: &str) -> GoalRecipe;
}

/// Registry of named controllers.
///
/// Lookups are O(1) by name. The registry is `Send + Sync` and can be shared
/// across tasks.
pub struct ControllerRegistry {
    controllers: std::collections::HashMap<String, Arc<dyn Controller>>,
}

impl ControllerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { controllers: std::collections::HashMap::new() }
    }

    /// Register a controller. Replaces any existing controller with the same name.
    pub fn register(mut self, controller: impl Controller + 'static) -> Self {
        self.controllers.insert(controller.name().to_string(), Arc::new(controller));
        self
    }

    /// Look up a controller by name and materialize a recipe.
    pub fn resolve(&self, name: &str, spec: &str) -> Option<GoalRecipe> {
        self.controllers.get(name).map(|c| c.resolve(spec))
    }

    /// List all registered controller names with their descriptions.
    pub fn list(&self) -> Vec<(String, String)> {
        self.controllers
            .values()
            .map(|c| (c.name().to_string(), c.description().to_string()))
            .collect()
    }

    /// Number of registered controllers.
    pub fn len(&self) -> usize {
        self.controllers.len()
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.controllers.is_empty()
    }
}

impl Default for ControllerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in controllers
// ---------------------------------------------------------------------------

/// Controller: fix all clippy warnings.
///
/// Goal: "Fix all clippy warnings in the project."
/// Verifier: `cargo clippy --all-targets -- -D warnings`
pub struct FixClippyController;

impl Controller for FixClippyController {
    fn name(&self) -> &str {
        "fix-clippy"
    }

    fn description(&self) -> &str {
        "Fix all clippy warnings. Optional spec: package name (e.g. `fix-clippy lim-agent`)."
    }

    fn resolve(&self, spec: &str) -> GoalRecipe {
        let spec = spec.trim();
        let goal = if spec.is_empty() {
            "Fix all clippy warnings in the project. Run `cargo clippy --all-targets -- -D warnings` \
             to see current warnings, then fix each one. Do not change the public API or break tests."
                .to_string()
        } else {
            format!(
                "Fix all clippy warnings in the `{spec}` package. \
                 Run `cargo clippy -p {spec} -- -D warnings` to see current warnings, then fix each one. \
                 Do not change the public API or break tests."
            )
        };
        let verifier = if spec.is_empty() {
            "cargo clippy --all-targets -- -D warnings".to_string()
        } else {
            format!("cargo clippy -p {spec} -- -D warnings")
        };
        GoalRecipe {
            goal,
            verifier_cmd: Some(verifier),
            budget: Budget::with_max_sessions(5),
            no_progress_threshold: Some(3),
        }
    }
}

/// Controller: write tests until they pass.
///
/// Goal: "Write tests for the specified module/path."
/// Verifier: `cargo test` (configurable)
pub struct WriteTestsController {
    test_cmd: String,
}

impl WriteTestsController {
    /// Create with a custom test command (e.g. `"cargo test -p lim-agent"`).
    pub fn new(test_cmd: impl Into<String>) -> Self {
        Self { test_cmd: test_cmd.into() }
    }
}

impl Default for WriteTestsController {
    fn default() -> Self {
        Self::new("cargo test")
    }
}

impl Controller for WriteTestsController {
    fn name(&self) -> &str {
        "write-tests"
    }

    fn description(&self) -> &str {
        "Write tests for the specified path/spec until `cargo test` passes."
    }

    fn resolve(&self, spec: &str) -> GoalRecipe {
        let spec = spec.trim();
        let goal = if spec.is_empty() {
            "Write comprehensive tests for the project. Identify untested code paths, \
             write tests for them, and ensure `cargo test` passes with no failures."
                .to_string()
        } else {
            format!(
                "Write comprehensive tests for `{spec}`. Identify untested code paths, \
                 write tests for them, and ensure `cargo test` passes with no failures."
            )
        };
        GoalRecipe {
            goal,
            verifier_cmd: Some(self.test_cmd.clone()),
            budget: Budget::with_max_sessions(8),
            no_progress_threshold: Some(4),
        }
    }
}

/// Controller: run a custom goal with no built-in verifier.
///
/// Use this when the goal doesn't have a deterministic verifier. The loop
/// runs until budget is exhausted or the agent stops naturally.
pub struct CustomController {
    name: String,
    description: String,
}

impl CustomController {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

impl Controller for CustomController {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn resolve(&self, spec: &str) -> GoalRecipe {
        GoalRecipe {
            goal: spec.to_string(),
            verifier_cmd: None,
            budget: Budget::unlimited(),
            no_progress_threshold: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_by_name() {
        let registry = ControllerRegistry::new()
            .register(FixClippyController)
            .register(WriteTestsController::default());

        let recipe = registry.resolve("fix-clippy", "").unwrap();
        assert!(recipe.goal.contains("clippy warnings"));
        assert_eq!(
            recipe.verifier_cmd.as_deref(),
            Some("cargo clippy --all-targets -- -D warnings")
        );

        let recipe = registry.resolve("write-tests", "src/auth/").unwrap();
        assert!(recipe.goal.contains("src/auth/"));
        assert_eq!(recipe.verifier_cmd.as_deref(), Some("cargo test"));
    }

    #[test]
    fn registry_list_returns_all() {
        let registry = ControllerRegistry::new()
            .register(FixClippyController)
            .register(WriteTestsController::default());

        let list = registry.list();
        assert_eq!(list.len(), 2);
        let names: Vec<_> = list.into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"fix-clippy".to_string()));
        assert!(names.contains(&"write-tests".to_string()));
    }

    #[test]
    fn unknown_controller_returns_none() {
        let registry = ControllerRegistry::new().register(FixClippyController);
        assert!(registry.resolve("nonexistent", "").is_none());
    }

    #[test]
    fn fix_clippy_with_package_spec() {
        let recipe = FixClippyController.resolve("lim-agent");
        assert!(recipe.goal.contains("lim-agent"));
        assert!(recipe.verifier_cmd.as_deref().unwrap().contains("-p lim-agent"));
    }

    #[test]
    fn custom_controller_passes_spec_as_goal() {
        let c = CustomController::new("my-goal", "custom goal");
        let recipe = c.resolve("do something specific");
        assert_eq!(recipe.goal, "do something specific");
        assert!(recipe.verifier_cmd.is_none());
    }
}
