# Library Polish — Ideas for External API Hardening

> Status: ideas, not confirmed for implementation.

These are areas that could improve lim's ergonomics for external consumers (sub-agents, multi-agent orchestration, third-party integrations). None are committed to any phase yet.

## 1. ProviderFactory

Currently each provider is created via `OpenAiProvider::new()` directly — no unified entry point. Multiple `AgentLoop` instances sharing a provider work (via `Arc<dyn Provider>`), but there's no config-driven factory or connection pooling strategy.

Possible direction: `ProviderFactory` or builder that constructs providers from config, manages shared state (HTTP client, connection pool), and produces `Arc<dyn Provider>` for multi-agent scenarios.

## 2. Session Isolation

`SessionWriter` takes a filesystem path — parallel sub-agents need distinct session directories. Current `sessions/` directory + `SessionId` already isolates, but there's no explicit session manager that allocates and tracks session lifecycle.

Possible direction: `SessionManager` that creates isolated session directories, provides `SessionWriter` handles, and supports concurrent sub-agent sessions without path conflicts.

## 3. Cross-Agent State in WorkflowContext

`WorkflowContext` uses `Arc<RwLock<HashMap<String, Value>>>` for KV storage. This works for simple data passing but doesn't provide typed channels for structured communication between sub-agents.

Possible direction: enhanced context with typed, channel-based message passing for inter-agent communication, or a dedicated `AgentBus` / message broker pattern for multi-agent coordination.
