# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Pending (requires user decision)

- Git history squash (pre-`v0.0.1-pre-squash` tag preservation + logical-phase squash)
- `v0.1.0` tag creation
- Cargo.lock tracking decision (currently gitignored; binary projects typically commit it)
- musl vs gnu static linking decision for Linux portable binary
- Code signing (macOS notarization + Windows authenticode) — optional for v0.1

## [v0.1.0] — 2026-07-09 (pending release)

### Added — Core Agent Runtime

- **AgentLoop**: turn-by-turn driver with streaming, tool dispatch, hooks, steering, and prompt caching
- **Provider abstraction**: 3 API formats — OpenAI Chat Completions, OpenAI Responses, Anthropic Messages — with unified `Provider` trait + `StreamEvent` streaming
- **Tool system**: `Tool` trait + `ToolRegistry` with `filtered_clone(&whitelist)` for role-based tool access; built-in tools (read/write/edit/shell) + loop control tools
- **Executor abstraction**: `Executor` trait + `LocalExecutor` with `DefaultPolicy` (denylist priority over allowlist)
- **Trust levels**: `Workspace` (default) / `Permissive` (explicit `--trust` opt-in); cross-workspace git operations blocked by default
- **Approval modes**: 5 tiers — Auto / AllowReadOnly / AllowNonNetwork / Always (Permissive cannot combine with Auto)
- **Context window management**: `ContextWindowManager` with 3-tier pipeline (trim / compact / overflow recovery) + `CompactionStrategy` injection
- **Compaction Phase 1**: `StructuredSummary` (9 LLM fields + 3 runtime fields) + file-level hints extraction + cache-safe `SUMMARY_INSTRUCTION` + 3-level JSON parse fallback
- **Session Tree Phase 1**: `AgentMessage.parent_id` field planted (backward compatible, linear Vec container unchanged)
- **Steering**: runtime message injection via `mpsc::UnboundedSender<SteeringMessage>`; status bar shows count
- **Prompt caching**: `PromptLayout` with 4 tiers (Static/Project/Session/Dynamic) + cache breakpoints for Anthropic prompt caching
- **AgentHooks**: unified policy extension point (`before_tool_decision`, `stop_decision`, `session_end_reason`)

### Added — Session Persistence

- **JSONL event log**: `SessionWriter` (atomic append, `truncate(true)` for path reuse safety) + `SessionReader` + `replay_transcript` / `replay_edits`
- **Session resume**: `--resume <path>` replays JSONL + restores context edits
- **Context editing**: `lim edit-context` — exclude/replace messages, `ContextEdited` events persisted for replay
- **Session discovery**: `lim sessions --list/--details/--limit`

### Added — Loop Engineering

- **LoopRunner**: cross-session goal loop driver — goal → session sequence → verify → next session
- **Verifiers**: `AlwaysPassVerifier`, `AllPassVerifier`, `CommandVerifier` (shell exit code), `LlmVerifier` (LLM-as-judge), `ReviewerVerifier` (independent reviewer agent with tool access)
- **Controllers**: `Controller` trait + `CustomController` / `FixClippyController` / `WriteTestsController` / `NoProgressDetector`
- **Budget**: `max_sessions` / `max_tokens` with `BudgetExhausted` result
- **Backoff**: `BackoffStrategy` trait + `FixedBackoff`
- **Ledger**: `LedgerBackend` trait + `StateLedger` (workspace-local `.lim/loop/`) + `DaemonLedger` (independent storage with `sync_to_workspace`)
- **GoalRunner**: daemon-friendly goal execution abstraction (`GoalRunner` trait + `LoggingGoalRunner` placeholder)
- **DaemonServer**: in-process goal management (Submit/List/Status/Pause/Resume/Sync commands)
- **Timer**: `Timer` trait + `IntervalTimer` + `TimerLoop` integration layer

### Added — Configuration

- **lim-config crate**: layered config (Global XDG → Project `.lim/` → CLI flags) with `ConfigLoader` merge
- **`[context]` section**: configurable context file loading — 4 search strategies (workspace_root / walk_up / subdirs / fixed), no hard-coded file names, `AGENTS.md` is just the default
- **`[serve]` section**: `ServeConfig` / `GoalDaemonConfig` / `SystemServiceConfig` — CLI flag > config > built-in default
- **`[providers.*]` section**: provider configurations with `type` / `api_key_env` / `base_url` / `full_url` / `default_model`
- **AgentRoleConfig**: `.lim/agents/<role>.toml` for sub-agent persona + capabilities

### Added — Transport Layer (lim-transport)

- **T0-T3 transport**: `LocalEndpoint` abstraction + `UnixStreamTransport` / `NamedPipeClientTransport` / `TcpStreamTransport` / `A2AHttpTransport`
- **LocalSocketTransport**: cross-platform local transport unifying Unix socket / Named Pipe
- **ProtocolAdapter**: `LimNativeAdapter` (serde_json) + `A2AAdapter` (A2A JSON-RPC), AgentMessage ↔ wire format interop
- **MCP dual-mode**: `McpServer<H>` (lim as server, `McpToolHandler` trait) + `McpClient` / `McpTool` / `McpToolProxy` (lim as client, consuming external MCP servers)
- **A2A protocol**: `AgentCard` / `A2ATask` / `TaskState` / `Artifact` / `A2AMessage` / `A2APart` + `A2ARequestHandler` trait + send/get/cancel/fetch_agent_card API
- **Service discovery**: `ServiceDiscovery` trait + `LocalRegistry` (T1) + `MdnsDiscovery` (T2 stub) + `RegistryDiscovery` (T3 stub)
- **Topology auto-selection**: `TopologySelector` infers T0-T3 from endpoint URL scheme
- **63 tests**: a2a 11 + mcp_server 8 + mcp_client 2 + mcp_protocol 7 + selector 11 + local_endpoint 8 + tcp 3 + unix_socket 4 + discovery 5 + protocol 3 + named_pipe 1

### Added — Serve Unified Entry (lim-serve)

- **ServeRunner**: foreground / daemon / system three run modes + protocol + capability orthogonal composition
- **Handler injection**: `ToolRegistryMcpHandler` (wraps `ToolRegistry`) + `AgentFactoryA2AHandler` (wraps `AgentFactory`) + `LoopRunnerGoalRunner` (adapts `LoopRunner` to `GoalRunner` trait)
- **Goal daemon bridge**: `spawn_daemon_server` starts `DaemonServer` in foreground mode with optional `GoalRunner`
- **MCP server integration**: `with_mcp_handler` injects `Arc<dyn McpToolHandler>`, foreground spawns MCP server task
- **A2A server**: minimal HTTP/1.1 server (GET agent-card + POST JSON-RPC + Bearer auth)
- **Daemon mode**: `--daemon` spawns detached child process (not fork+setsid, avoids tokio fork safety issue) + PID file management
- **System service**: `ServiceManager` trait — Linux systemd user unit / macOS launchd LaunchAgent / Windows `sc.exe` (all three platforms implemented)
- **Shutdown**: three-way `tokio::select!` — Ctrl+C + SIGTERM + external `CancellationToken`
- **CLI**: `lim serve` with `--protocol` / `--daemon` / `--system` / `--workspace` / `--config` / `--endpoint` / `--auth-token`

### Added — CLI

- `lim init` — workspace initialization (`--provider` pre-fills config template, `--force` overwrites)
- `lim run` — non-interactive single-message agent session
- `lim tui` — interactive TUI (ratatui + crossterm) with slash commands, approval popups, steering, provider/model switching
- `lim providers` — list configured providers
- `lim sessions` — list/view saved sessions
- `lim edit-context` — append `ContextEdited` events to saved sessions
- `lim loop` — Loop Engineering: `--goal` / `--resume` / `--list` / `--status` / `--verify` / `--max-sessions` / `--max-tokens`
- `lim serve` — unified server entry point
- `lim goal` — goal management (submit/list/status/pause/resume/sync) as preferred alias
- `lim loop-daemon` — **deprecated**, forwards to `lim serve` / `lim goal`

### Added — Domain Pack

- **novel-pack**: novel writing domain extension on `lim-workflow` — `StoryBible` / `SceneOutline` / `ChapterOutline` data model + chapter generation DAG (writer → editor → reviewer nodes) + `novel-cli` binary

### Added — Infrastructure

- **Version embedding**: `build.rs` runs `git describe --tags --always --dirty=-dirty` → `LIM_VERSION` env, `lim --version` and `SessionHeader.lim_version` both use it
- **CI**: `ci.yml` (fmt + clippy `-D warnings` + build + test) + `release.yml` (6-platform matrix: Linux x86_64/aarch64 gnu, macOS x86_64/aarch64, Windows x86_64/aarch64 msvc)
- **Dependabot**: github-actions + cargo dual-ecosystem weekly updates
- **Release profile**: `lto = "fat"` + `codegen-units = 1` + `strip = "symbols"` + `panic = "abort"`

### Added — Documentation

- `docs/architecture/` — overview, modules, execution-flow, behavior-trace, compaction, loop-engineering, evolution, layers, transport-layer, serve-mode
- `docs/integration-guide.md` — L0-L4 integration complexity progression with novel-writing case study
- `docs/functional-testing.md` — functional test guide covering full workflow
- `docs/conventions/` — basics.md (13 rules) + full-guide.md (14 sections) + README.md index
- `docs/research/` — agent-sandbox-landscape, system-prompt-persistence, mxc-feasibility-study, zerostack surveys

### Known Limitations (v0.1)

- **daemon IPC**: in-process mpsc only, no Unix socket / named pipe transport for `DaemonServer` cross-process communication
- **A2A task storage**: `AgentFactoryA2AHandler` uses in-memory `HashMap`, tasks lost on restart
- **mDNS / registry discovery**: in-memory stubs, no real network discovery
- **Windows Named Pipe transport**: skeleton only (TCP loopback fallback works)
- **Compaction Phase 2**: no `OverflowRecovery` trigger (context_length_exceeded recovery)
- **Session Tree Phase 2**: `parent_id` planted but fork/rewind not activated
- **Sandbox backend**: `LocalExecutor` is in-process only (Landlock/bwrap/Seatbelt planned for v0.2+)
- **CLI module split**: `apps/cli/src/main.rs` is ~2200 lines single file

### Test Coverage

- **624 unit/integration tests** passing across workspace
- `cargo fmt --check` clean
- `cargo clippy --all-targets -- -D warnings` clean
