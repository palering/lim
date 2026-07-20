# zerostack 项目调研

> 调研日期：2026-07-04
> 调研目的：对比开源 agent 项目设计，识别可吸收的设计点
> 仓库：https://github.com/gi-dellav/zerostack
> 版本：v1.6.0（截至调研日）

## 1. 基本信息

| 维度 | zerostack | lim（当前） |
|---|---|---|
| **定位** | 极简编码 agent（~12k LoC） | 实验性 agent 框架（~50k+ LoC） |
| **架构** | 单 crate，所有代码在 `src/` | 9 crate workspace + 4 层分层 |
| **底层依赖** | 基于 rig 0.39（agent 框架） | 完全自研（lim-llm 自研 provider） |
| **Binary size** | 12.9MB | 未优化（未测） |
| **RAM** | ~16MB（峰值 24MB） | 未测 |
| **Allocator** | mimalloc | 系统 |
| **TUI** | 自研（crossterm 直连，无 ratatui） | ratatui + crossterm |
| **License** | GPL-3.0 | 未定 |
| **版本** | v1.6.0（15 个 release） | v0.1（未发布） |

## 2. 架构对比

### zerostack 的选择：单 crate + rig 框架 + enum dispatch

```rust
// zerostack: enum dispatch 替代 trait object
enum AnyAgent { OpenAi(...), Anthropic(...), Gemini(...), Ollama(...) }
// 匹配比 vtable 快，但加 provider 要改 enum
```

### lim 的选择：多 crate 分层 + trait object

```rust
// lim: trait object + Arc
trait Provider { async fn stream(...) -> BoxStream<StreamEvent> }
// 加 provider 不改核心类型，但有一次 vtable 开销
```

### 取舍

- zerostack：单 crate 编译快、binary 小，但扩展受 rig 约束
- lim：分层清晰、可嵌入性强，但编译慢、binary 大

## 3. 值得吸收的 7 个设计点

### 3.1 Prompts 系统（高价值）

zerostack 内置 10 个 prompt 模式，运行时 `/prompt <name>` 切换：

| Prompt | 用途 |
|---|---|
| `code` (默认) | 编码模式，全工具访问 |
| `plan` | 只规划不写码 |
| `review` | 代码审查 |
| `debug` | 根因分析 |
| `ask` | 只读模式（read/grep/glob） |
| `brainstorm` | 设计探索 |
| `frontend-design` | 前端设计 |
| `review-security` | 安全审查 |
| `simplify` | 代码简化 |
| `write-prompt` | 写 prompt |

**对 lim 的启示**：lim 目前只有单一 `system_prompt`，可以引入"prompt 模式"概念——本质是预置的 `AgentRoleSpec`，让用户 `/mode review` 一键切换。这与 lim 已有的 `AgentRoleSpec` + `.lim/agents/<role>.toml` 机制天然契合，只是缺预置模板和 TUI 命令。

### 3.2 Doom-loop 检测（中价值）

zerostack 在 permission checker 里追踪相同 tool call 重复 3+ 次，触发警告/拒绝。

**对 lim 的启示**：lim 的 `NoProgressDetector` 是 goal 级别（连续 N 次验证失败），但**没有 tool call 级别的循环检测**。agent 卡在"读文件→编辑→读文件→编辑"循环里时，lim 不会主动干预。可以加一个 `ToolCallLoopDetector` hook。

### 3.3 Headless Loop 模式（中价值）

```bash
zerostack --loop --loop-prompt "Refactor the API" --loop-max 10 --loop-run "cargo test"
```

每次迭代包含：原始任务 + 演进的 `LOOP_PLAN.md` + 上次迭代摘要 + 验证输出。

**对 lim 的启示**：lim 的 `LoopRunner` 更完整（Verifier/Controller/Budget），但**启动门槛高**——需要写 goal recipe、选 verifier。zerostack 的 `--loop-prompt + --loop-run` 是极简版，适合"快速跑个迭代"。lim 可以加一个 `lim loop --quick` 模式，内部用 `AlwaysPassVerifier` + `CommandVerifier(--loop-run)` 包装。

### 3.4 Feature Gates（中价值）

zerostack 的 `loop`/`mcp`/`acp`/`memory`/`subagents`/`git-worktree` 都是编译期 feature：

```toml
[features]
default = ['loop', 'git-worktree', 'mcp', 'subagents', 'archmd', 'status-signals', 'multithread']
```

**对 lim 的启示**：lim 目前所有功能都是默认编译的。如果用户只想要"单次 LLM 调用"（L1 集成），不需要 `lim-workflow`/`multiagent`/`loop_runner`。加 feature gate 可以减小 binary size 和编译时间。但 lim 是 workspace，feature gate 要在 app 层做。

### 3.5 ACP 支持（低价值，未来考虑）

ACP（Agent Communication Protocol）让编辑器（如 Zed）把 zerostack 当 agent 后端。`--acp` 启动 JSON-RPC server。

**对 lim 的启示**：lim 目前没有 IDE 集成入口。如果未来要支持 Zed/VSCode 把 lim 当后端，ACP 是一个标准化路径。与 A2A 协议处于相似层级，可一并规划。v0.1 不需要。

### 3.6 内存与体积优化（低价值，发布前考虑）

- `mimalloc` 全局 allocator
- `compact_str` 替代 String（小字符串内联）
- `smallvec` 替代 Vec
- `opt-level = "z"` + `lto = "thin"` + `codegen-units = 1` + `strip = true`

**对 lim 的启示**：lim 的 release profile 已经有 LTO/strip 配置，但没用 mimalloc/compact_str。v0.1 发布前可以考虑加 mimalloc（一行代码的事），binary size 能再小一点。

### 3.7 `--quick-model` 预设（低价值）

zerostack 配置文件里定义"快速模型"组合，`--quick-model work` 一键切换到便宜模型做日常活，`--quick-model heavy` 切到强模型做难活。

**对 lim 的启示**：lim 目前 `--provider` + `--model` 是分开指定的。可以加一个 `[quick_models]` 配置段，`--quick-model <name>` 一键切换 provider+model+context_window 组合。

## 4. lim 比 zerostack 强的地方

| 维度 | lim | zerostack |
|---|---|---|
| **分层架构** | 4 层 + 9 crate，边界清晰 | 单 crate，模块耦合 |
| **Loop Engineering** | Verifier/Controller/Ledger/Budget/Backoff/Timer 完整体系 | 简单迭代 + LOOP_PLAN.md |
| **行为可观测性** | JSONL 事件流 + ContextCompacted + Steering 事件 + 黑盒点审计 | 基本日志 |
| **Session Tree** | parent_id 留种 | 无 |
| **Compaction** | StructuredSummary + file_hints + cache-safe forking | 简单摘要 |
| **多 agent** | multiagent 子模块（拓扑/transport/worktree） | subagents feature（简单并行） |
| **工作流编排** | lim-workflow DAG | 无 |
| **领域扩展** | packs/novel | 无 |
| **Context 文件加载** | 可配置查找策略（4 种） | 硬编码 AGENTS.md/CLAUDE.md |
| **Provider 抽象** | 自研 trait，完全控制 | 依赖 rig，受框架约束 |
| **Cache-aware prompt** | PromptLayout 4 层 + cache breakpoint | rig 内部处理，不可控 |

## 5. 关键洞察

zerostack 验证了一个事实：**很多用户只想要一个能用的编码 agent，不需要完整框架**。它的 12k LoC 覆盖了 90% 的日常需求。

lim 的价值不在于"更小的 agent"，而在于**可嵌入、可扩展、可观测的 agent 框架**——让外部程序（如小说编写工具）能按需引入能力。这是 zerostack 没有的维度。

所以 lim 不应该追求"比 zerostack 更小"，而应该追求"比 zerostack 更可嵌入"。

## 6. 可吸收项的纳入规划

将以上设计点纳入 lim 的规划，按优先级与阶段排布。详见 [docs/TODO.md](../TODO.md) "待讨论项"段与 [docs/architecture/evolution.md](../architecture/evolution.md)。

| 设计点 | 优先级 | 建议阶段 | 状态 |
|---|---|---|---|
| Prompts 模式系统 | 高 | v0.2 | 待讨论 |
| Tool call 循环检测 | 高 | v0.2 | 待讨论 |
| mimalloc + profile 优化 | 中 | v0.1 发布前 | 待评估 |
| Feature gates | 中 | v0.2 | 待讨论 |
| `lim loop --quick` 极简模式 | 中 | v0.2 | 待讨论 |
| `--quick-model` 预设 | 低 | v0.2 | 待讨论 |
| ACP 支持 | 低 | v0.3+ | 待讨论（与 A2A 同期） |
