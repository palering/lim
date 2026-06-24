# Architecture

Architectural decisions for lim. These are confirmed design choices that guide implementation.

## Frontend / Library Boundary

### TUI as Frontend

TUI is a "frontend" — a presentation layer over the agent loop, parallel to CLI. The architecture separates frontends (binary crates in `apps/`) from core libraries (lib crates in `crates/`).

**Decision: separate `tui` crate in `apps/` directory, with feature-gated dependency from main binary.**

- ratatui / crossterm are terminal-specific deps, unrelated to CLI's plain-text output
- Multiple frontends (CLI, TUI, future desktop/web) each as independent crate, sharing core libraries
- Single binary distribution with subcommands (`lim run`, `lim tui`), TUI compiled in via feature gate
- TUI implementation isolated in its own crate — easy to swap backend (ratatui → alternative) without touching the main binary

### Directory Structure

Three top-level directories, each with a clear semantic:

```
lim/
  crates/       # 核心 library — 可导出的 crate
    lim-core/
    lim-llm/
    lim-agent/
    lim-executor/
    lim-tools/
    lim-session/
    lim-workflow/

  apps/         # 应用层 — 可运行的前端
    cli/        # lim 主 binary（子命令分发）
    tui/        # TUI 前端（ratatui，可替换）
    desktop/    # 桌面前端（未来，Tauri）
    web/        # Web 前端（未来）

  packs/        # 领域扩展 — 特定领域的 lib + bin
    novel/      # novel-pack（小说创作）
```

- `crates/` = 纯 library，被 `apps/` 和 `packs/` 消费
- `apps/` = 面向终端用户的应用，一个 `cargo install lim` 分发所有子命令
- `packs/` = 领域特定的扩展包（lib + bin），独立于主应用

主 binary（`apps/cli/`）通过 feature gate 可选依赖各前端 crate，实现单 binary 多模式分发。

### Library Exportability

All crates in `crates/` are library-only (`[lib]`) with complete `pub use` re-exports. Application crates live in `apps/`. No `publish = false`, no `private = true`.

Dependency graph is unidirectional:

```
lim-core (leaf)
  ├── lim-llm
  ├── lim-executor
  ├── lim-session
  ├── lim-agent (→ lim-core, lim-llm)
  ├── lim-tools (→ lim-core, lim-executor, lim-agent)
  └── lim-workflow (→ lim-core, lim-llm, lim-agent, lim-session)
```

External applications import only the layers they need. `novel-pack` proves the model — it depends on `lim-core`, `lim-llm`, `lim-agent`, `lim-workflow`, `lim-tools` and builds its own multi-agent orchestration on top.

Multi-agent / sub-agent usage pattern:

```toml
[dependencies]
lim-agent = "..."   # AgentLoop, AgentHooks, ToolRegistry
lim-llm = "..."     # Provider trait + implementations
lim-workflow = "..." # DAG orchestration (optional)
```

Create sub-agents via `AgentLoop::new()`, share providers via `Arc<dyn Provider>`, coordinate via custom `AgentHooks` implementations, register shared tools in `ToolRegistry`.
