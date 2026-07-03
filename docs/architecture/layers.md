# 分层职责与扩展点详解

本文档详细描述每层的职责、组件、扩展点，以及"谁定义 / 谁实现 / 谁调用"的关系。

## Primitives Layer（原语层）

### 组件

| crate | 职责 |
|---|---|
| `lim-core` | 跨 crate 共享的数据类型和中性 trait |
| `lim-llm` | LLM provider 抽象与实现 |

### lim-core 提供的类型

- **身份类型**：`SessionId`、`BranchId`、`TurnId`、`MessageId`、`ToolCallId`、`TraceId`
- **消息类型**：`AgentMessage`、`LlmMessage`、`Role`、`ContentBlock`
- **工具类型**：`ToolCall`、`ToolResult`、`ToolSchema`、`ToolCategory`
- **事件类型**：`AgentEvent`、`EventEnvelope`
- **安全类型**：`TrustLevel`、`ToolGuard` trait
- **错误类型**：`LimError`、`Result`
- **上下文编辑**：`ContextEdit`
- **停止原因**：`StopReason`
- **用量**：`Usage`

### lim-llm 提供的抽象

| Trait/Struct | 说明 |
|---|---|
| `Provider` trait | `stream(LlmRequest) -> BoxStream<StreamEvent>` + `model_metadata(model)` |
| `LlmRequest` | model + messages + tools + system + max_tokens |
| `StreamEvent` | TextDelta / ToolCallStart / ToolCallDelta / Usage / Done |
| `ProviderError` | Auth / RateLimited / Transport / Server / Other |

**Provider 实现者**：AnthropicProvider / OpenAiChatProvider / OpenAiResponsesProvider / MockProvider

### 设计约束

- lim-core **不依赖 tokio**，只有同步类型和 trait
- Tool trait 因需要 async（`execute` 返回 `ToolFuture`）放在 lim-agent，这是合理的妥协
- lim-core 不定义行为策略，只定义数据形状和中性抽象（如 `ToolGuard` 只暴露 `trust_level()`，不做权限决策）

---

## Core Layer（核心层）

### 组件

| 模块 | 文件 | 职责 |
|---|---|---|
| `AgentLoop` | agent_loop.rs | 纯循环执行器 |
| `AgentHooks` | hooks.rs | 统一策略扩展点 |
| `Tool` trait | tool.rs | 工具抽象 |
| `ToolRegistry` | registry.rs | 工具注册表 |
| `ContextWindowManager` | context_window.rs | context 压缩管理 |
| `CompactionStrategy` | compaction/strategy.rs | 压缩策略 trait |
| `StreamAccumulator` | stream_accumulator.rs | 流事件组装 |

### AgentLoop 的职责边界

**只做**：
- 消息流转：接收用户消息 → 调 LLM → 收到工具调用 → 执行工具 → 回传结果 → 下一轮
- LLM 调用：构造 LlmRequest，处理流式响应，retry 重试
- 工具调度：通过 ToolRegistry 查找工具，调用 hooks 拦截
- 事件 emit：通过 broadcast channel 发送 EventEnvelope
- 取消：响应 CancellationToken

**不做**（通过钩子注入）：
- context 投影策略（通过 `hooks.edit_context` + user_edits + ContextWindowManager）
- 权限决策（通过 `hooks.before_tool_call`）
- 停止条件（通过 `hooks.should_stop` + `max_steps`）
- session 持久化（通过 broadcast channel 解耦，SessionWriter 订阅）

### AgentHooks：统一策略扩展点

`AgentHooks` 是 lim 的**唯一策略入口**。所有策略决策都通过这个 trait 注入。

```rust
pub trait AgentHooks: Send + Sync {
    // === 工具调用拦截 ===
    fn before_tool_call(&self, call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>>;
    fn after_tool_call(&self, result: &ToolResult) -> HookFuture<Result<()>>;
    
    // === 上下文编辑 ===
    fn edit_context(&self, messages: &[AgentMessage]) -> HookFuture<Result<Option<ContextEdit>>>;
    
    // === Loop Engineering：停止控制 ===
    fn should_stop(&self, ctx: &StopContext) -> StopDecision;
    fn max_steps(&self) -> Option<usize>;
    
    // === 权限（吸收 PermissionPolicy + ToolGuard） ===
    fn check_path(&self, path: &Utf8Path) -> Result<()>;
    fn check_command(&self, cmd: &str, args: &[String]) -> Result<()>;
    fn trust_level(&self) -> TrustLevel;
    
    // === 压缩（吸收 CompactionStrategy） ===
    fn compact(&self, req: CompactRequest) -> Result<CompactionResult>;
}
```

> **注**：当前实现中，`PermissionPolicy`、`ToolGuard`、`CompactionStrategy` 仍是独立 trait。合并进 `AgentHooks` 是目标状态，重构按演进路线分步进行。当前阶段的 P0 工作是**新增** `should_stop` 和 `max_steps`，不破坏现有 trait。

### Tool trait

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    fn category(&self) -> ToolCategory { ToolCategory::ReadOnly }
    fn execute(&self, tc_id: ToolCallId, args: Value, cancel: CancellationToken) -> ToolFuture<Result<ToolResult>>;
}
```

**实现者**（在 lim-tools crate）：
- `ReadFileTool`（ReadOnly）
- `WriteFileTool`（Write）
- `EditFileTool`（Write）
- `ShellTool`（Script）

### ToolRegistry

工具的运行时注册表，被 `AgentLoop` 和 `ApprovalHooks` 共享。`schemas()` 按 name 排序保证 LLM 工具列表稳定。

---

## Harness Layer（基础设施层）

### 组件

| crate/模块 | 职责 |
|---|---|
| `lim-session` | session JSONL 持久化 |
| `lim-agent/compaction` | 压缩策略实现 |
| `lim-agent/loop_runner`（新增） | Loop Engineering 组件 |

### lim-session

| 组件 | 说明 |
|---|---|
| `SessionWriter` | 写事件到 JSONL（truncate(true)，避免复用路径时数据污染） |
| `SessionReader` | 读 JSONL，处理向后兼容（Value 预解析去重 + `#[serde(default)]`） |
| `SessionManager` | session 列表/详情/删除，元数据从 JSONL 派生 |

### loop_runner（新增，Loop Engineering）

| 组件 | 说明 |
|---|---|
| `LoopRunner` | Goal Loop 执行器，调用 AgentLoop 跑单个 session |
| `StateLedger` | 跨 session 状态持久化（.lim/loop/） |
| `Verifier` trait | goal 验证（CommandVerifier / LlmVerifier） |
| `Budget` | 预算控制（max_sessions / max_tokens / max_cost） |

详见 [loop-engineering.md](loop-engineering.md)。

---

## Product Layer（产品层）

### 组件

| crate | 职责 |
|---|---|
| `apps/cli` | CLI 入口，命令分发 |
| `apps/tui` | 终端 UI，交互渲染 |
| `lim-tools` | 内置工具集 |

### Product 层的约束

1. **不定义策略 trait**：策略 trait 在 Core/Harness 层定义
2. **只实现策略**：如 `ApprovalHooks` 实现 `AgentHooks`
3. **不持有业务规则**：审批的 Trust×ApprovalMode 联动规则是纯函数，可在 Harness 层
4. **做 UI 和组装**：把 Provider + Executor + Tool + Hooks 拼起来

### 审批策略（ApprovalHooks）

`ApprovalHooks` 是 `AgentHooks` 的产品级实现，位于 `apps/tui`。它：
- 实现 `before_tool_call`：根据 ApprovalMode + ToolCategory 决定是否弹窗
- 通过 mpsc channel 与 TUI 主线程通信（发送 ApprovalRequest，await oneshot 回复）
- 查询 `ToolGuard::trust_level()` 执行 Trust×ApprovalMode 联动

**已知问题**：审批逻辑困在 TUI，CLI 无法复用。演进路线中会将其下沉到 Harness 层或独立 crate。

### Loop 触发器（新增）

CLI 的 `lim loop` 命令是 LoopRunner 的产品层入口：

```
lim loop --goal "..." --verifier "cargo test"
lim loop --resume [goal_id]
lim loop --status [goal_id]
```

---

## 扩展点总览表

### 能力扩展（正交 trait）

| Trait | 定义层 | 定义文件 | 方法数 | 实现者 | 调用者 |
|---|---|---|---|---|---|
| `Provider` | lim-llm | provider.rs | 2 | Anthropic/OpenAI×2/Mock | AgentLoop, SummarizeStrategy |
| `Executor` | lim-executor | executor.rs | 5 | LocalExecutor, Stub | 4 个 Tool |
| `Tool` | lim-agent | tool.rs | 4 | read/write/edit/shell | ToolRegistry, AgentLoop |

### 策略扩展（AgentHooks 的方法）

| 方法 | 当前状态 | 默认行为 | 实现者 |
|---|---|---|---|
| `before_tool_call` | ✅ 已有 | Proceed | ApprovalHooks (TUI) |
| `after_tool_call` | ✅ 已有 | Ok(()) | ApprovalHooks (TUI) |
| `edit_context` | ✅ 已有 | None | （未来扩展） |
| `should_stop` | 🆕 P0 新增 | Continue | LoopRunner / Product 层 |
| `max_steps` | 🆕 P0 新增 | None | LoopRunner / Product 层 |
| `check_path` | 🔜 未来合并 | Ok(()) | DefaultPolicy (当前独立 trait) |
| `check_command` | 🔜 未来合并 | Ok(()) | DefaultPolicy (当前独立 trait) |
| `trust_level` | 🔜 未来合并 | Workspace | DefaultPolicy (当前独立 trait) |
| `compact` | 🔜 未来合并 | 默认策略 | SummarizeStrategy (当前独立 trait) |

### 其他扩展点

| 组件 | 类型 | 说明 |
|---|---|---|
| `ToolRegistry` | 注册表 | Tool 的运行时注册，支持 register/unregister |
| `ContextWindowManager` | 管理器 | 持有 `Box<dyn CompactionStrategy>`，被 AgentLoop 持有 |
| `StateLedger` 🆕 | 状态持久化 | Loop Engineering 的跨 session 状态 |
| `Verifier` 🆕 | 验证 trait | Loop Engineering 的 goal 验证 |
| `Budget` 🆕 | 预算控制 | Loop Engineering 的资源限制 |

---

## 依赖方向图

```
apps/cli ──┬──> lim-agent ──> lim-core
           ├──> lim-session ──> lim-core
           ├──> lim-tools ──> lim-executor ──> lim-core
           └──> lim-llm ──> lim-core

apps/tui ──┬──> lim-agent
           ├──> lim-session
           ├──> lim-tools
           └──> lim-llm

lim-agent ──┬──> lim-core
            └──> lim-llm

lim-session ──> lim-core
lim-executor ──> lim-core
lim-tools ──> lim-executor + lim-core
```

**禁止反向依赖**：lim-core 不依赖任何 lim-* crate。lim-agent 不依赖 lim-session。apps/* 不被任何 crate 依赖。
