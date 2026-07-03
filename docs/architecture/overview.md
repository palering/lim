# lim 架构总览

本文档定义 lim 的分层架构、设计原则与扩展点组织方式。它是所有后续架构决策的参照系。

## 一、四层架构

lim 采用严格的四层分层，外加一个横切关注点（lim-executor）。每层只能依赖下层，禁止反向依赖。

```
┌─────────────────────────────────────────────────────────┐
│  Product Layer                                           │
│  apps/cli + apps/tui + lim-tools + 审批策略 + loop 触发   │
│  职责：UI 渲染、平台交互、组装、策略实现                  │
├─────────────────────────────────────────────────────────┤
│  Harness Layer                                           │
│  lim-session + compaction + context_window + loop_runner │
│  职责：运行时基础设施                                     │
├─────────────────────────────────────────────────────────┤
│  Core Layer                                              │
│  AgentLoop + AgentHooks + Tool trait                     │
│  职责：纯循环（消息流转 → LLM 调用 → 工具调度 → 事件 emit）│
├─────────────────────────────────────────────────────────┤
│  Primitives Layer                                        │
│  lim-core（类型）+ lim-llm（provider）                    │
│  职责：数据形状 + 中性 trait + LLM 抽象                   │
└─────────────────────────────────────────────────────────┘
         ↑ lim-executor 横切关注点（执行能力，独立于层）
```

### 各层职责

**Primitives Layer（原语层）**
- 定义跨 crate 共享的数据类型：`AgentMessage`、`ContentBlock`、`ToolCall`、`EventEnvelope`、`SessionId` 等
- 定义中性 trait：`Provider`（LLM 抽象）
- 零运行时依赖：lim-core 不依赖 tokio，只有数据形状
- Tool trait 因需要 async 放在 lim-agent（Core 层），这是合理的妥协

**Core Layer（核心层）**
- `AgentLoop`：纯循环执行器，只做消息流转 → LLM 调用 → 工具调度 → 事件 emit
- `AgentHooks`：统一策略扩展点（机制在此定义，策略由上层实现）
- `Tool` trait + `ToolRegistry`：工具注册与调度
- `ContextWindowManager`：context 压缩管理（机制在此，策略通过 `CompactionStrategy` 注入）
- **核心层不做策略决策**：不内联 context 投影逻辑、不实现权限规则、不决定何时停止

**Harness Layer（基础设施层）**
- `lim-session`：session JSONL 持久化（SessionWriter/SessionReader/SessionManager）
- `compaction`：context 压缩策略实现（SummarizeStrategy）
- `loop_runner`：Loop Engineering 组件（LoopRunner + StateLedger + Verifier）
- **Harness 层是"如何使用核心循环"的基础设施**，不直接面向用户

**Product Layer（产品层）**
- `apps/cli`：CLI 入口，参数解析，命令分发
- `apps/tui`：终端 UI，用户交互，审批弹窗渲染
- `lim-tools`：内置工具集（read/write/edit/shell）
- 审批策略实现（ApprovalHooks）
- loop 触发器（CLI `lim loop` 命令）
- **产品层不定义策略 trait，只实现策略**；它不持有业务规则，业务规则通过 Harness/Core 层的纯函数提供

### lim-executor 的特殊定位

`lim-executor` 不是某一层的组件，而是**横切关注点**——Core/Harness/Product 都可能需要执行能力（读文件、跑命令）。executor 本身不做权限决策，权限是产品层策略，通过 `PermissionPolicy` trait 注入。

## 二、十条设计原则

### 1. 核心循环只做循环

`AgentLoop` 只负责：消息流转、LLM 调用、工具执行调度、事件 emit、取消。

所有策略（context 投影、compaction 触发、权限、retry、stop 判断）通过钩子注入。循环本身是无策略的框架。

### 2. 策略在产品层，机制在核心层

核心层提供 hook/通道/trait（机制），产品层决定"做什么"（策略）。

- 权限确认 → 产品层实现 `AgentHooks::before_tool_call`
- compaction 策略 → Harness 层实现 `CompactionStrategy`
- 停止条件 → 产品层实现 `AgentHooks::should_stop`

### 3. 广播通道是边界

`AgentLoop` 通过 `tokio::broadcast` channel 发事件，不持有 SessionWriter。session 持久化是 Harness 层订阅者，不是核心的依赖。这保证了核心循环与持久化解耦。

### 4. crate 边界对齐职责边界

每个 crate 有且只有一个职责：
- `lim-core`：类型
- `lim-llm`：provider
- `lim-agent`：循环
- `lim-session`：持久化
- `lim-executor`：执行能力
- `lim-tools`：内置工具

不交叉。Crate 边界就是职责边界。

### 5. 扩展优于内置

新功能优先考虑通过 hooks/扩展点实现，不往核心塞。对应 Pi 的 "No MCP/No sub-agents/No plan mode" 哲学：核心不做的，留给扩展。

### 6. 原语层零运行时

`lim-core` 不依赖 tokio，只有数据形状和中性 trait。Tool trait 因为需要 async 放在 lim-agent，这是合理的妥协。

### 7. 执行与管理分离

`AgentLoop` 只负责"跑一轮"（Turn Loop），循环生命周期管理（继续/停止/恢复/队列）是上层（LoopRunner / future Scheduler）的职责。

对应 Pi 的 `agentLoop()` 函数 vs `Agent` 类的分离。

### 8. 编排是可选的，不是核心的

workflow（静态或动态）是 Harness 层的可选组件。核心不需要知道"多节点编排"的存在。一个 agent 单独跑就够用时，不应该被强加编排层。

### 9. AI-native 自省

lim 的核心功能（session 管理、loop 控制、任务调度）应该作为工具暴露给 AI，让 AI 能在对话中动态创建和管理长程任务。聊天本身也是一种 loop，AI 可以随时从"对话模式"切换到"任务模式"。

### 10. 纵深演进，每阶段可独立交付

类比 k8s 纵深（docker → compose → k8s → k3s），每个阶段都有完整产品形态，不强制升级。Loop Engineering 跨越多个阶段，每个阶段有不同成熟度形态。详见 [evolution.md](evolution.md)。

## 三、扩展点组织

lim 的扩展点分两类：

### 能力扩展（Capability）— 定义"能做什么"

独立的 trait，各自描述一种能力维度，保持正交：

| Trait | 定义层 | 本质 |
|---|---|---|
| `Provider` | lim-llm | "能调哪个 LLM" |
| `Executor` | lim-executor | "能执行什么 IO" |
| `Tool` | lim-agent | "能用什么工具" |

### 策略扩展（Policy）— 定义"怎么做"

统一收敛到 `AgentHooks` trait，所有方法都有默认实现，使用者按需 override：

| 方法 | 默认行为 | 谁实现 |
|---|---|---|
| `before_tool_call` | Proceed | Product 层（审批） |
| `after_tool_call` | 无操作 | 任意 |
| `edit_context` | None | 任意 |
| `should_stop` | Continue | Product 层 / LoopRunner |
| `max_steps` | None | Product 层 / LoopRunner |
| `check_path` | Ok(()) | Harness 层（PermissionPolicy） |
| `check_command` | Ok(()) | Harness 层（PermissionPolicy） |
| `trust_level` | Workspace | Harness 层 |
| `compact` | 默认策略 | Harness 层（CompactionStrategy） |

详细的扩展点清单见 [layers.md](layers.md)。

## 四、参考实现

lim 的架构参考了 Pi（earendil-works/pi）的分层理念，但根据 Rust 语言特性和 lim 的约束做了调整：

- Pi 的 `ExtensionAPI` 统一入口 → lim 的 `AgentHooks` 统一策略入口
- Pi 的 `AgentLoopConfig` 钩子 → lim 的 `AgentHooks` 方法
- Pi 的 harness 子目录 → lim 的独立 crate（lim-session）
- Pi 的 coding-agent 产品层 → lim 的 apps/cli + apps/tui

关键差异：lim 用 Rust 的类型系统保证事件流和消息流的正确性，用 broadcast channel 实现核心与持久化的彻底解耦。
