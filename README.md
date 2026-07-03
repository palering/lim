# lim

> 一个实验性的 AI agent 框架，聚焦 Loop Engineering 与行为可观测性。

**当前状态**：早期开发中，架构快速演进，API 不稳定。

## 核心特性

- **Loop Engineering** — 跨 session 的 Goal Loop：持久化状态、验证目标完成、预算控制、无进展检测、退避策略
- **行为可观测** — 所有 agent 行为都有对应的可观测信号（Event 或 Log），session JSONL 可完整回放一次 run
- **分层架构** — 参考 k8s 的扩展模型，Product / Harness / Core / Primitives 四层清晰分离
- **多 Provider 支持** — Anthropic、OpenAI Chat、OpenAI Responses，统一 streaming 接口
- **可扩展 Verifier** — Command / LLM / Reviewer / Skill 四种验证器，支持组合
- **Time-based Loop** — Timer 抽象层支持定时触发的 goal loop（interval / cron 可扩展）
- **Multi-Agent** — Worktree 隔离 + Workspace 注册，支持多 agent 并行

## 快速开始

### 安装

```bash
git clone https://github.com/palering/lim.git
cd lim
cargo build --release
# 二进制位于 target/release/lim
```

### 配置

在项目根目录创建 `.lim/config.toml`：

```toml
[[providers]]
name = "anthropic"
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[[providers]]
name = "openai"
type = "openai-chat"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
```

设置环境变量：

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# 或
export OPENAI_API_KEY="sk-..."
```

### 运行

```bash
# 单次对话
lim run --message "帮我写一个 hello world"

# Goal Loop 模式（跨 session 自动迭代直到目标完成）
lim loop --goal "fix all clippy warnings" --verify "cargo clippy"

# 查看 goal 状态
lim loop --status <goal_id>

# 列出所有 goal
lim loop --list

# 启动 TUI 交互界面
lim tui
```

## 架构概览

```text
┌─────────────────────────────────────────────┐
│  Product Layer  (apps/cli, apps/tui)        │  用户接口、CLI 参数解析、配置加载
├─────────────────────────────────────────────┤
│  Harness Layer  (lim-agent)                 │  AgentLoop、LoopRunner、Hooks、Verifier
├─────────────────────────────────────────────┤
│  Core Layer     (lim-core, lim-llm)         │  数据结构、Provider trait、Session
├─────────────────────────────────────────────┤
│  Primitives     (lim-core)                  │  ID、Error、Event 等零依赖原语
└─────────────────────────────────────────────┘
```

设计原则：
- **Policy in product, mechanism in core** — 核心层提供 hooks/channels/traits，产品层决定策略
- **Extension over built-in** — 新功能优先通过 hooks/extension points 实现
- **AI-native introspection** — 核心功能（session 管理、loop 控制）应可被 AI 调用

详见 [docs/architecture/](docs/architecture/)。

## Crate 结构

| Crate | 职责 |
|-------|------|
| `lim-core` | 原语层：ID、Error、Event、Message、Tool trait |
| `lim-config` | 配置加载：TOML schema、路径解析、provider 配置 |
| `lim-llm` | LLM Provider：Anthropic、OpenAI Chat/Responses，统一 streaming |
| `lim-session` | Session 持久化：JSONL writer/reader、replay、manager |
| `lim-agent` | Harness 层：AgentLoop、LoopRunner、Hooks、Verifier、Timer、Backoff |
| `lim-skills` | Skill 加载：YAML frontmatter + markdown body，project 覆盖 global |
| `lim-tools` | 内置工具：read、write、edit、shell、loop_control |
| `lim-executor` | 执行器：本地命令执行、策略控制 |
| `lim-workflow` | 工作流：DAG 图、节点执行、artifact 管理 |
| `apps/cli` | CLI 前端：子命令分发、参数解析 |
| `apps/tui` | TUI 前端：ratatui + crossterm |
| `packs/novel` | 领域扩展：小说创作工作流 |

## 文档

- [PROGRESS.md](PROGRESS.md) — 当前进度
- [docs/INDEX.md](docs/INDEX.md) — 文档索引
- [docs/architecture/overview.md](docs/architecture/overview.md) — 架构总览
- [docs/architecture/loop-engineering.md](docs/architecture/loop-engineering.md) — Loop Engineering 设计
- [docs/architecture/behavior-trace.md](docs/architecture/behavior-trace.md) — 行为链路与可观测性
- [docs/architecture/evolution.md](docs/architecture/evolution.md) — 演进路线
- [docs/conventions/](docs/conventions/) — 代码规范

## 开发

```bash
# 运行所有测试
cargo test --workspace

# Clippy 检查（0 warning）
cargo clippy --workspace --all-targets -- -D warnings

# 格式化检查
cargo fmt --all -- --check
```

## License

MIT
