# 行为链路与可观测性

lim 的核心设计目标之一：**不成为黑盒**。本文档梳理 agent 运行时的所有行为链路、可观测信号、已知黑盒点，以及每次迭代必须自检的清单。

> **本文档是 code review 的关键参考**。任何 PR 若引入了未被本文档覆盖的隐式行为，或削弱了既有可观测性，必须在 review 时被指出并补全文档。

---

## 1. 设计原则

这些原则驱动 lim 的可观测性设计。任何新模块、新行为、新代码路径都必须服从这些原则；偏离时必须显式标注并补充理由。

### P1. 行为可追踪（Behavior Traceable）

> "我想要我们的 agent 有很清晰的行为链路。"

每一条从用户输入到 LLM 输出到工具执行到消息回写的路径，都必须能从一份 session JSONL 完整回放。**不允许存在"产生结果但不在 JSONL 留痕"的代码路径**。

具体含义：

- 每一次 LLM 调用、每一次工具执行、每一次上下文修改，都必须 emit `AgentEvent`
- `AgentEvent` 携带足够字段让读者无需读源码即可重建行为
- 反例（已修复）：`context_window.summarize()` 曾是隐藏 LLM 调用，不进 event channel。现已通过 `ContextCompacted` event 的 `summary_model` / `summary_prompt_tokens` / `summary_completion_tokens` 字段完全可观测（见 §8.1）

### P2. 隐式行为显式化（Implicit Made Explicit）

> "要么过于复杂，要么跟黑盒一样，不知道塞进去什么行为。"

agent 代码中存在大量"隐式行为"：默认值、自动派发、隐式排序、自动重试、自动 compaction。这些行为如果不显式标注，就是黑盒。要求：

- **隐式行为的决策点必须有 log 或 event**：排序、过滤、降级、自动重试、自动压缩，每发生一次都要留下可观测痕迹
- **决策理由必须可重建**：例如 `TurnStart` 携带 `model` / `tool_count` / `tool_names` / `system_prompt_len`，让读者能重建"这一轮 LLM 看到了什么"
- **反例（已修复）**：
  - `ToolRegistry::schemas()` 曾返回 HashMap 迭代顺序（非确定），可能微妙地影响 LLM 工具选择 → 现已按 `name` 排序
  - ApprovalHooks Reject 和 schema 校验失败曾是静默返回 → 现已 emit `ToolExecutionStart` + `ToolExecutionEnd`

### P3. 错误不静默（No Silent Failure）

> "SSE 解析失败原本静默 `return vec![]`"

错误处理三档：

- **致命错误**：`return Err(...)` 向上传播，停止当前 turn
- **可恢复错误**（如 retryable provider error）：emit `AgentEvent::Error` + `warn!` log，按策略重试
- **可降级错误**（如单个 SSE event JSON 解析失败）：emit `warn!` log，丢弃该 event，继续流

**严禁**：返回空 `vec`、`unwrap_or_default()`、`let _ = ...` 丢弃 Result。任何"看起来失败但不报错"的代码路径，必须配 `warn!` 解释为何静默是安全的。

### P4. 可观测性分层（Layered Observability）

可观测信号分两层，互不替代：

| 层 | 载体 | 用途 | 生命周期 |
|---|---|---|---|
| **Event** | `AgentEvent` → broadcast channel → `SessionWriter` → JSONL | 行为审计、session 回放、跨 session 分析 | 永久持久化 |
| **Log** | `tracing` 宏 → stderr / 文件 | 运行时调试、性能分析、临时排障 | 进程生命周期 |

**Event 是契约，Log 是辅助**。任何"应该持久化"的信号必须用 Event；任何"运行时关心但不需持久化"的信号用 Log。Log 不能替代 Event，因为 Log 不进 JSONL、不能跨 session 回放、不被 TUI 消费。

### P5. 闭合枚举（Closed Enum）

错误类型用闭合枚举，禁止 catch-all 变体：

- `LimError` 8 个具体变体，无 `Other(anyhow::Error)`
- `ProviderError` 9 个具体变体
- 新增错误源必须显式添加变体，不允许悄悄塞进 catch-all

理由：catch-all 变体会让错误分类失效，重试策略、用户提示、降级决策都依赖错误的具体类型。

### P6. 锁不跨 await（Lock-Free Across Await）

`ToolRegistry` 使用 `RwLock<HashMap<String, Arc<dyn Tool>>>`，但所有读操作只持锁足够长时间拿到 `Arc` clone，立即释放。`Arc::clone` 是单次原子操作，clone 出来的 `'static` 引用可以安全跨 `.await`。

**理由**：长-running 工具执行（如 `shell` 命令数秒）不能阻塞 MCP 服务器并发注册新工具。违反此原则会导致死锁或性能塌方。

---

## 2. 行为链路总览

```
┌─────────┐    ┌──────────────────────────────────────────┐
│  User   │───▶│              AgentLoop                   │
│ Message │    │  ┌──────────────────────────────────┐    │
└─────────┘    │  │  run() / run_continue()           │    │
               │  └────────────┬─────────────────────┘    │
               │               ▼                          │
               │  ┌──────────────────────────────────┐    │
               │  │ Context Projection (3 layers)    │    │
               │  │  1. Loop edit (hooks, transient) │    │
               │  │  2. User edits (persistent)      │    │
               │  │  3. Compaction (token budget)    │    │
               │  └────────────┬─────────────────────┘    │
               │               ▼                          │
               │  ┌──────────────────────────────────┐    │
               │  │ LlmRequest assembly              │    │
               │  │  + emit TurnStart (self-audit)   │    │
               │  └────────────┬─────────────────────┘    │
               │               ▼                          │
               │  ┌──────────────────────────────────┐    │
               │  │ Provider.stream() + retry        │    │
               │  │  + emit MessageStart/Update/End  │    │
               │  │  + emit Error on retryable fail  │    │
               │  └────────────┬─────────────────────┘    │
               │               ▼                          │
               │  ┌──────────────────────────────────┐    │
               │  │ if stop_reason == ToolCall:      │    │
               │  │   execute_tool_calls()           │    │
               │  │   (per call: validate → hook →   │    │
               │  │    execute → emit Start/End)    │    │
               │  └────────────┬─────────────────────┘    │
               │               ▼                          │
               │  ┌──────────────────────────────────┐    │
               │  │ TurnEnd (stop_reason + usage)   │    │
               │  │ loop until no tool calls         │    │
               │  └──────────────────────────────────┘    │
               └────────────────┬─────────────────────────┘
                                ▼
                        ┌───────────────┐
                        │  Event Bus    │──▶ SessionWriter ──▶ JSONL
                        │ (broadcast)   │──▶ TUI / CLI
                        └───────────────┘
```

每一层的行为、信号、决策点在后续章节展开。

---

## 3. 启动链路

### 3.1 进程启动（`apps/cli`）

1. 解析 CLI 参数（`--provider`, `--model`, `--resume`, `--trust` 等）
2. 加载 `.lim/config.toml`（按 provider section 解析）
3. **检测 `PlatformInfo`**（`PlatformInfo::detect()`，见 `crates/lim-core/src/env.rs`）
   - 检测 OS / arch / shell / workspace_roots
   - **Shell 选择**：Unix 用 `PosixSh`；Windows 按 Git Bash > WSL > PowerShell 优先级检测
   - **Workspace roots**：`git rev-parse --show-toplevel` 优先，fallback cwd
   - **打印到 stderr**：`Platform: linux / x86_64 | Shell: sh (POSIX sh (Unix))`
4. 构造 `Provider` 实例（`OpenAiChatProvider` / `AnthropicProvider` / `OpenAiResponsesProvider`）
5. 构造 `LocalExecutor`（用 `platform.workspace_roots` 做 workspace 沙箱）
6. 构造 `ToolRegistry`，注册内置工具：
   - `read`, `write`, `edit`（共享 `Arc<dyn Executor>`）
   - `shell`（持有 `Arc<dyn ShellBackend>`，由 `detect_default_backend()` 检测一次）
7. 构造 `InjectInfo::new(platform)`，调用 `.system_prompt_fragment()` 生成 system prompt
8. 构造 `AgentLoop`，注入 provider / registry / hooks / config（含 system_prompt）
9. 如有 `--resume <session.jsonl>`：
   - `replay_transcript(events)` 重建消息历史
   - `replay_edits(events)` 重建 user edits
   - 调用 `run_continue(prior, new_msg)` 而非 `run()`
10. 启动 broadcast channel + `SessionWriter`
11. 进入 `AgentLoop::run()`

> **TUI 入口**（`apps/tui`）流程类似，但 TUI 内部自己调用 `PlatformInfo::detect()` + `InjectInfo::new()` 构造 system prompt，保持自包含。apps/cli 只向 TUI 传递 provider / registry / executor。

### 3.2 启动期可观测信号

| 信号 | 层 | 内容 |
|---|---|---|
| `AgentStart` event | Event | session_id（隐含在 envelope） |
| `info!("agent started")` | Log | session_id |
| `eprintln!("Platform: ...")` | stderr | OS / arch / shell / workspace |

### 3.3 PlatformInfo 检测的可观测性

`PlatformInfo::detect()` 是纯检测层，无副作用。检测结果：

- **被 `LocalExecutor` 使用**：作为 workspace_roots 用于路径校验
- **被 `ShellTool` 使用**：通过 `detect_default_backend()` 决定调用哪个 shell backend
- **被 `InjectInfo` 使用**：渲染为 system prompt fragment 让 LLM 自适应语法

shell 选择是**启动期一次性决策**，session 中途不切换。这避免每次 tool call 都重新检测的开销，也让 LLM 在整个 session 中使用一致的语法假设。

---

## 4. Agent Loop 执行链路

### 4.1 单轮执行（一个 turn）

`run_inner()` 对每个 turn 执行以下步骤：

#### 4.1.1 Context Projection（3 层投影）

```
messages (持久化历史)
    │
    ├─ Layer 1: Loop edit (hooks.edit_context, transient)
    ▼
working_messages
    │
    ├─ Layer 2: User edits (persistent, replayed on resume)
    ▼
working_messages (with user edits applied)
    │
    ├─ Layer 3: Compaction (token budget, may invoke hidden LLM call)
    ▼
llm_messages (linearized, ready for provider)
```

- **Layer 1**：`hooks.edit_context()` 返回 `Option<ContextEdit>`。修改只影响当前 turn 的 LLM 请求，不持久化
- **Layer 2**：`user_edits` 是 `Vec<ContextEdit>`，通过 `compose_edits()` 合并后 `apply_edit()`。这些 edit 持久化在 JSONL 中（通过 `ContextEdited` event），resume 时通过 `replay_edits()` 重建
- **Layer 3**：若 `tokens > context_window * 0.8`，调用 `summarize()` 生成摘要替换旧消息。**隐藏 LLM 调用，元数据已通过 `ContextCompacted` event 完全可观测（见 §8.1）**

每层完成后无显式 event，但 compaction 会 emit `ContextCompacted`（如发生）。

#### 4.1.2 LlmRequest 组装 + TurnStart 自审计

```rust
let mut request = LlmRequest::new(&self.config.model, llm_messages);
request = request.with_tools(tool_schemas.clone());
if let Some(ref system) = self.config.system_prompt {
    request = request.with_system(system.clone());
}

self.emit(AgentEvent::TurnStart {
    turn_id,
    model: request.model.clone(),
    message_count: request.messages.len(),
    tool_count: tool_schemas.len(),
    tool_names: tool_schemas.iter().map(|s| s.name.clone()).collect(),
    system_prompt_len: request.system.as_ref().map(|s| s.len()),
})?;
```

**自审计含义**：JSONL 读者无需读源码即可重建"这一轮 LLM 看到了什么模型、多少消息、哪些工具、多长的 system prompt"。`system_prompt_len` 只持久化长度不持久化内容，避免敏感 prompt 泄漏到 JSONL。

#### 4.1.3 Provider 流式 + 重试

```rust
let mut attempt: u32 = 0;
let mut content_emitted = false;

loop {
    if self.cancel.is_cancelled() { break; }
    let stream = provider.stream(request.clone());

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                if let Some(delta) = accumulator.ingest(event) {
                    content_emitted = true;
                    self.emit(AgentEvent::MessageUpdate { ... })?;
                }
            }
            Err(e) => {
                attempt_error = Some(e);
                break;
            }
        }
    }

    if attempt_error.is_none() { break; }
    let can_retry = err.is_retryable() && !content_emitted && attempt < MAX_RETRIES;

    self.emit(AgentEvent::Error {
        message: err.to_string(),
        retryable: can_retry,
        retry_count: attempt + 1,
        max_retries: MAX_RETRIES,
    })?;

    if !can_retry { return Err(err.into()); }

    attempt += 1;
    accumulator.reset();  // 保留 msg_id 让 TUI 稳定
    tokio::select! {
        _ = sleep(backoff) => {},
        _ = cancel.cancelled() => break,
    }
}
```

**重试策略**：

- 只在**未输出任何 content** 时重试（避免重复内容污染 TUI）
- 只对 retryable 错误重试（`RateLimited` / `ServerOverloaded` / `Transport`）
- 指数退避：1s → 2s → 4s，最多 `MAX_RETRIES = 3` 次
- 每次重试前 emit `AgentEvent::Error` 让 TUI 显示发生了什么
- 重试耗尽时 emit `Error { retryable: false }` 然后向上传播

#### 4.1.4 工具调用分支

- 若 `stop_reason == ToolCall && assistant_msg.has_tool_calls()`：执行工具，emit 每个 `ToolResult` 作为 `MessageEnd`，emit `TurnEnd(ToolCall)`，**继续外层循环**让模型处理工具结果
- 否则：emit `TurnEnd(stop_reason)`，**break 退出**

### 4.2 Agent Loop 可观测信号

| 事件 | 层 | 携带 |
|---|---|---|
| `TurnStart` | Event | turn_id, model, message_count, tool_count, tool_names, system_prompt_len |
| `MessageStart` | Event | message_id, role |
| `MessageUpdate` | Event | message_id, content_index, delta |
| `MessageEnd` | Event | message_id, 完整 message |
| `TurnEnd` | Event | turn_id, stop_reason, usage |
| `Error` | Event | message, retryable, retry_count, max_retries |
| `ContextCompacted` | Event | messages_before/after, tokens_before/after |
| `ContextEdited` | Event | ContextEdit |
| `AgentStart` / `AgentEnd` | Event | (无字段) |
| `info!("agent started/ended")` | Log | session_id |
| `debug!("turn started")` | Log | turn_num |
| `warn!("agent cancelled")` | Log | turn_num |
| `warn!("retrying provider stream")` | Log | attempt, backoff_ms, error |

---

## 5. Tool 执行链路

### 5.1 单个工具调用流程

`execute_single_tool_call()` 对每个工具调用执行：

```
1. registry.get(name) ──▶ Option<Arc<dyn Tool>>
   │  (UnknownTool 时返回 Err，由上层 emit 错误 result)
   ▼
2. validate_args(schema, arguments)
   │  失败 ──▶ emit ToolExecutionStart + ToolExecutionEnd(error)
   │           返回 Ok(ToolResult::error)（不向上抛，让 LLM 看到 schema 错误）
   ▼
3. hooks.before_tool_call(call)
   │  Proceed ─────▶ 使用原 args
   │  ModifyArgs ▶ 使用新 args
   │  Reject ────▶ emit ToolExecutionStart + ToolExecutionEnd(error)
   │                返回 Ok(ToolResult::error("Rejected: ..."))
   ▼
4. emit ToolExecutionStart { tool_call_id, tool_name, started_at: now_ms() }
   ▼
5. tool.execute(tc_id, args, cancel).await
   │  (Cancel 检查在每个工具内部自己实现)
   ▼
6. emit ToolExecutionEnd { tool_call_id, result, ended_at: now_ms() }
   ▼
7. hooks.after_tool_call(&result)
```

**关键设计**：

- **schema 失败和 hooks Reject 不向上抛错**：返回 `Ok(ToolResult::error(...))`，让 LLM 看到错误信息并自我修正。但仍然 emit `ToolExecutionStart` + `ToolExecutionEnd` 让 JSONL 完整记录"为什么这次调用失败了"
- **`started_at` / `ended_at` 用 Unix ms**：让 JSONL 读者可以计算工具执行耗时，不依赖 tracing 时间戳
- **registry.get() 返回 `Arc` clone**：锁只持足够长时间拿 Arc，不跨 `.await`，详见第 1 节 P6

### 5.2 Tool 执行可观测信号

| 事件 | 层 | 携带 |
|---|---|---|
| `ToolExecutionStart` | Event | tool_call_id, tool_name, started_at |
| `ToolExecutionUpdate` | Event | tool_call_id, output（流式） |
| `ToolExecutionEnd` | Event | tool_call_id, result, ended_at |
| `debug!("executing tool")` | Log | tool name |
| `warn!("schema validation failed")` | Log | tool, error |
| `error!("tool execution error")` | Log | tool, error（仅当 execute 返回 Err） |

---

## 6. Provider 链路

### 6.1 三种 provider 实现共享的流式结构

```
Provider::stream(request)
    │
    ├─ build_request_body(request) ──▶ serde_json::Value
    ▼
tokio::spawn(async move {
    debug!("sending request", url, model, message_count)
    trace!(body = ?body)
    ▼
    reqwest POST + headers
    ▼
    if !status.is_success():
        debug!(status, body, "HTTP error response")
        tx.send(Err(classify_http_error(status, text)))
        return
    ▼
    debug!(status, "stream started")
    sse_event_stream(response)
    │
    └─ while let Some(result) = stream.next().await:
        Ok(event) ──▶ trace!(event_type, data_len, "SSE event")
        │             │
        │             ├─ try_extract_error(&event) ──▶ tx.send(Err(err))
        │             │   (检测 HTTP 200 + error SSE event 的畸形流)
        │             ▼
        │             state.parse_event(&event) ──▶ Vec<StreamEvent>
        │             │
        │             └─ for ev in events: tx.send(Ok(ev))
        │
        Err(e) ──▶ tx.send(Err(e)); return
})
│
└─ ReceiverStream::new(rx).boxed()
```

### 6.2 三种 provider 的格式差异

| Provider | endpoint | system prompt | tool call arguments |
|---|---|---|---|
| `OpenAiChatProvider` | `/v1/chat/completions` | system role message | 索引累加（`index → ToolCallId` 映射） |
| `OpenAiResponsesProvider` | `/v1/responses` | `instructions` field | `output_item.done` 一次性完整 |
| `AnthropicProvider` | `/v1/messages` | top-level `system` field | `input_json_delta` 累加 |

### 6.3 SSE 解析的静默降级（已修复）

每个 provider 的 `parse_event()` 在 JSON 解析失败时：

```rust
let payload: serde_json::Value = match serde_json::from_str(data) {
    Ok(v) => v,
    Err(e) => {
        tracing::warn!(
            data_len = data.len(),
            error = %e,
            "{provider} SSE: failed to parse JSON payload, dropping event"
        );
        return vec![];
    }
};
```

**为何选择 `warn!` 而非 `Err`**：单个 SSE event 解析失败不代表整个流坏了。丢弃一个 event 后续 events 仍可能正常。`warn!` 让运行时知道发生了降级，但不过度打断。

### 6.4 StreamAccumulator 的 JSON 降级（已修复）

LLM 返回的 tool call arguments 可能是畸形 JSON。`StreamAccumulator::into_message()` 处理：

```rust
let arguments = match serde_json::from_str(&args_str) {
    Ok(v) => v,
    Err(e) => {
        tracing::warn!(
            tool_call_id = %id,
            tool_name = %name,
            args_len = args_str.len(),
            error = %e,
            "tool call arguments JSON parse failed, falling back to raw string"
        );
        serde_json::Value::String(args_str)
    }
};
```

**为何降级而非报错**：工具执行可能能从原始字符串恢复部分信息（如 regex 提取）。降级 + warn 让工具自己决定如何处理。

### 6.5 Provider 可观测信号

| 事件 | 层 | 携带 |
|---|---|---|
| `debug!("sending request")` | Log | url, model, message_count |
| `trace!(body = ?body)` | Log | 完整请求体（含 prompt 内容，仅 trace 级别） |
| `debug!("HTTP error response")` | Log | status, body |
| `debug!("stream started")` | Log | status |
| `trace!("SSE event")` | Log | event_type, data_len |
| `warn!("failed to parse JSON payload, dropping event")` | Log | data_len, error |
| `warn!("tool call arguments JSON parse failed")` | Log | tool_call_id, tool_name, args_len, error |

---

## 7. Session 持久化

### 7.1 SessionWriter 行为

- **`SessionWriter::create(path)`** 用 `truncate(true)` 而非 `append(true)`。理由：每次 session run 写自包含 JSONL，append 模式会让旧 run 的 events 污染新 run
- **每行 flush**：`write_event()` 写一行 + flush，确保崩溃后已写的内容不丢失
- **路径**：`sessions/<session_id>.jsonl`（项目内，遵守 Containment Principle）

### 7.2 Session Resume 行为

`run_continue(prior_messages, new_user_message)`：

1. **re-emit prior messages as `MessageEnd`**：让新 session JSONL 自包含完整对话历史
2. **re-emit persisted `ContextEdited` events**：让链式 resume（A → B → C）不丢失 A 的 edits
3. **emit new user message as `MessageEnd`**
4. **进入 `run_inner()`** 继续执行

### 7.3 Session 可观测信号

- **每行 JSONL 包含完整 `EventEnvelope`**：`seq`, `session_id`, `branch_id`, `trace_id`, `event`
- **`seq` 单调递增**：可用于检测丢失（虽然 broadcast channel 不会丢失）
- **JSONL 可被 `replay_transcript()` 和 `replay_edits()` 重建**：是 resume 的基础

### 7.4 Session schema versioning

**目的**：让未来的 reader 能检测不兼容的 session 文件，并对破坏性 schema 变更应用迁移。

#### 设计

每个新 session 的第一行（seq=1）是 `SessionHeader` 事件，由 `AgentLoop::run_inner()` 在 `AgentStart` 之前 emit：

```rust
AgentEvent::SessionHeader {
    lim_version: String,     // e.g. "0.1.0"，仅 debug/审计
    schema_version: u32,     // 单调递增整数，兼容性判断的主信号
    platform: String,        // e.g. "macos/aarch64"，跨平台诊断
}
```

- `lim_core::SCHEMA_VERSION`：当前 schema 版本常量（目前 = 1）
- `lim_core::LIM_VERSION`：编译期 `env!("CARGO_PKG_VERSION")`
- `lim_core::platform_string()`：`"{std::env::consts::OS}/{std::env::consts::ARCH}"`

**为什么不只用 `lim_version`**：判断兼容性需要解析 semver 并维护"哪个 lim 版本引入了哪个 schema 破坏"的映射表，脆弱且易错。`schema_version` 是单调整数，**只在真正发生破坏性 schema 变更时 bump**，语义清晰。

#### Reader 检测策略（`SessionReader::read_all`）

| 文件的 `schema_version` | 行为 |
|---|---|
| 无 `SessionHeader`（旧文件） | 当作 `schema_version = 0`（legacy），`debug!` log，正常读取。`#[serde(default)]` 兜底字段缺失 |
| `== SCHEMA_VERSION` | 正常读取，`debug!` log |
| `< SCHEMA_VERSION` | 走兼容路径（目前 no-op），`debug!` log。**未来迁移钩子挂在这里** |
| `> SCHEMA_VERSION` | **拒绝读取**，返回错误："please upgrade lim to read this file" |

**关键 YAGNI 决策**：当前不实现 `migrate()` 函数。`#[serde(default)]` 已经处理了所有已知的字段缺失情况（`TurnStart` 的 model/tool_count、`ToolExecutionStart` 的 started_at 等）。只有遇到 serde 兜底不了的破坏性变更（字段重命名、语义单位变更、envelope 结构重组）时，才在 `check_schema_version` 的 `< current` 分支里加迁移逻辑。

#### 什么变更需要 bump `SCHEMA_VERSION`

- ✅ **不需要 bump**：新增 event 字段（用 `#[serde(default)]` 兜底）
- ✅ **不需要 bump**：新增 event 变体（旧 reader 遇到未知变体会报错，但旧文件不会包含新变体）
- ❌ **需要 bump**：字段重命名（如 `tool_name` → `name`）
- ❌ **需要 bump**：语义单位变更（如 `started_at` 从秒改成毫秒）
- ❌ **需要 bump**：envelope 结构重组（如 `EventEnvelope` 字段移位）
- ❌ **需要 bump**：tag scheme 变更（如 `#[serde(tag = "event")]` 改成别的）

#### 未来迁移规划（尚未实现）

当出现需要 bump 的破坏性变更时：

1. 在 `lim-session` 新增 `migrate` 模块，实现 `fn migrate(events: Vec<EventEnvelope>, from_schema: u32) -> Result<Vec<EventEnvelope>>`
2. 在 `SessionReader::check_schema_version` 的 `< current` 分支调用 `migrate(events, v)`
3. 迁移函数内部按 `from_schema` 分发：`if from_schema < 2 { events = migrate_v1_to_v2(events)?; }` 等
4. bump `SCHEMA_VERSION` 常量
5. 在本节文档记录迁移规则

**迁移函数的设计原则**：
- 幂等：对已经是目标版本的 events 调用应该是 no-op
- 可组合：`v1 → v3` = `v1 → v2 → v3`，每步只处理一个版本跳跃
- 可观测：每个迁移步骤 emit `debug!` log，记录转换了多少事件

#### 可观测信号

- `debug!` log：reader 检测到 schema_version 时记录（无论匹配与否）
- `SessionHeader` 事件本身：持久化在 JSONL 第一行，可供人工审计
- 错误：未来版本拒绝读取时，错误消息包含文件的 schema_version 和当前支持的版本

---

## 8. 已知黑盒点 / 部分黑盒

这些是当前架构中**部分不可观测**的代码路径。新增类似路径时必须在本文档登记，并尽量补全可观测性。

### 8.1 ContextWindow.summarize() 的隐藏 LLM 调用（已可观测）

**位置**：`crates/lim-agent/src/context_window.rs::summarize()`

**状态**：✅ **完全可观测**（自 2026-06-30 起，v1.2）

compaction 触发的 LLM 调用不进 event channel（不 emit `TurnStart` / `MessageStart` / `MessageEnd`），避免与主 turn 的事件流混淆。但其元数据已通过 `ContextCompacted` event 持久化到 JSONL，让读者无需读源码即可审计：

```rust
AgentEvent::ContextCompacted {
    messages_before, messages_after,
    tokens_before, tokens_after,
    // 隐藏 LLM 调用的元数据（v1.2 新增）：
    summary_model: String,           // 用于摘要的模型
    summary_prompt_tokens: u64,      // 摘要调用的输入 token
    summary_completion_tokens: u64,  // 摘要调用的输出 token
}
```

**运行时可观测信号**：

- `debug!` log（启动时）：`context compaction: starting hidden LLM summary call` + model + conversation_chars
- `warn!` log（stream 错误时）：`context compaction: stream error during summary` + error
- `warn!` log（空响应降级时）：`summary provider returned empty response; falling back to verbatim old messages` + prompt_tokens + completion_tokens
- `warn!` log（Phase 1：LLM 输出不可解析为 StructuredSummary 时）：`summary LLM output not parseable as StructuredSummary; using free-text fallback` + prompt_tokens + completion_tokens
- `ContextCompacted` event（完成时）：携带 `summary_model` + token 用量
- `CompactionResult.summary: Option<StructuredSummary>`（Phase 1，内部字段，不进 event）：`Some` 表示 LLM 输出成功解析为结构化摘要；`None` 表示 fallback 到自由文本。包含 9 个 LLM 填充字段（goal/constraints/done/in_progress/blocked/key_decisions/next_steps/critical_context）+ 2 个运行时填充字段（read_files/modified_files，由 `extract_file_hints()` 从 `read`/`write`/`edit` 工具调用参数本地确定性提取）
- TUI 状态栏：`Context compacted: 12 → 5 messages (summary cost: 342 tokens via gpt-4o)`

**设计权衡**：为什么不直接给隐藏调用也 emit `TurnStart` / `MessageEnd`？

- 隐藏调用是 agent 内部决策（compaction），不是用户发起的 turn。若 emit `TurnStart`，TUI 会显示一个用户从未触发的 turn，造成混乱
- 隐藏调用的"结果"不是一条 message，而是替换底层 context projection 的摘要文本。emit `MessageEnd` 会污染 message 历史
- 元数据嵌入 `ContextCompacted` 是更合适的归属：JSONL 读者能从"compaction 发生"事件中看到完整成本，不需要在事件流中插一个"伪 turn"

**审查规则**：任何引入新隐藏 LLM 调用的 PR 必须在本文档登记，且必须配 `debug!` log + 在对应的"完成"event 中嵌入元数据（model + token 用量）。

### 8.2 Hooks 内部行为

**位置**：`AgentHooks::before_tool_call` / `after_tool_call` / `edit_context` 的具体实现

**问题**：hooks 是用户提供的 trait 实现，其内部行为不可被 lim 强制观测。`before_tool_call` 的 `Reject` / `ModifyArgs` 决策只通过 emit 的 `ToolExecutionEnd` 间接体现，没有专门的"hooks 决策"event。

**当前缓解**：TUI / CLI 的 `ApprovalHooks` 实现内部有自己的 TUI 交互（弹窗、键盘流），这部分是 UI 层的可观测性，不属于 agent runtime。

**审查规则**：第三方 hooks 实现应在自己的 `before_tool_call` / `edit_context` 内部加 `debug!` log。lim 不强制，但在文档示例中推荐。

### 8.3 Provider 内部的 reqwest 连接池

**位置**：`reqwest::Client` 内部

**问题**：连接复用、keep-alive、DNS 缓存等行为由 reqwest 管理，lim 不观测。这些是网络层的细节，通常不影响 agent 行为正确性。

**审查规则**：不观测。如出现连接层问题（如 DNS 故障），通过 `ProviderError::Transport` 的 source chain 追溯到 `reqwest::Error`。

### 8.4 ReviewerVerifier 的 reviewer agent 内部行为（已闭合）

**位置**：`crates/lim-agent/src/loop_runner/verifier.rs::ReviewerVerifier::verify()`

**状态**：✅ **已闭合**（P4.4 修复，2026-07-02）

`ReviewerVerifier` 每次 `verify()` 调用 `self.factory.create_session()` spawn 一个独立的 reviewer agent，然后调用 `reviewer.run(prompt)`。reviewer agent 内部走标准 `AgentLoop` 流程，会 emit `TurnStart` / `MessageEnd` / `ToolExecutionStart` / `ToolExecutionEnd` 等 `AgentEvent`。

**P4.4 修复后**：reviewer 的 `AgentEvent` 通过 `ReviewPersister` 持久化到 `.lim/loop/<goal_id>/reviews/review-<N>.jsonl`。`Verifier::verify()` 签名扩展为接收 `VerifyContext { goal_id, loop_dir }`，`ReviewerVerifier` 用 `loop_dir` 构造 review 文件路径。`LoopEvent::VerificationDone` 也扩展了 `verifier_kind` / `verifier_session_id` / `verifier_tokens_used` 字段。

**B-1/B-2 修复后**（2026-07-03）：
- **B-1**：主 agent 的 `AgentEvent`（TurnStart / MessageStart / ToolExecutionStart / ToolExecutionEnd / MessageEnd）现在通过 `SessionEventPersister` 持久化到 `.lim/loop/<goal_id>/session-<N>.jsonl`。此前 `LoopRunner::run_goal_loop` 中 `event_rx` 被丢弃（`_event_rx`），主 agent 的行为链路完全不落盘。
- **B-2**：`Verifier` trait 返回类型从 `Result<VerifyResult>` 扩展为 `Result<VerifyOutcome>`，`VerifyOutcome { result, metadata: VerifyMetadata { session_id, tokens_used } }`。`LoopEvent::VerificationDone` 的 `verifier_session_id` / `verifier_tokens_used` 现在从 `VerifyMetadata` 填充（`ReviewerVerifier` 报告 `session_id`，`LlmVerifier` 报告 `tokens_used`，`AllPassVerifier` 聚合子 verifier 的 metadata）。

**当前可观测信号**（Log 层 + Event 层）：

- `debug!("spawning reviewer agent for verification")` — 启动时
- `debug!("reviewer agent responded", first_line, response_len)` — 完成时
- `info!("reviewer verifier: PASS")` / `warn!("reviewer verifier: FAIL")` — 结果
- `LoopEvent::VerificationDone { result, verifier_kind, verifier_session_id, verifier_tokens_used }` — loop event channel，持久化到 `.lim/loop/<goal_id>/events.jsonl`，所有字段均已填充
- reviewer `AgentEvent` → `.lim/loop/<goal_id>/reviews/review-<N>.jsonl`（可回放 reviewer 决策过程）
- 主 agent `AgentEvent` → `.lim/loop/<goal_id>/session-<N>.jsonl`（可回放主 agent 每个 session 的完整行为链路）

**遗留**：

- reviewer session 仍写入主 `sessions/` 目录（语义混乱，P4.5 角色配置可解决）

**审查规则**：任何引入"spawn 隐藏 agent"的 PR 必须在本文档登记，且必须配 `debug!` log + 在对应的"完成"event 中嵌入元数据 + 持久化 agent event 到 JSONL。

> **P4.4 已闭合**（2026-07-02）：spawn `ReviewPersister` task 消费 `event_rx` 写 `.lim/loop/<goal_id>/reviews/review-<N>.jsonl`；`LoopEvent::VerificationDone` 携带 `verifier_kind` 元数据；`LoopRunner.with_loop_dir()` 启用 LoopEvent 落盘到 `.lim/loop/<goal_id>/events.jsonl`。

---

## 9. Status 系统设计（P4.2）

> 新增于 P4.2。借鉴 k8s `Status.conditions` 设计，与 Event 系统形成"控制 vs 观测"的双层模型。

### 9.1 Event vs Status 职责边界

lim 的可观测性有两套信号，**职责严格分离**：

| 维度 | Event（不可变, 观测） | Status（可变, 决策） |
|---|---|---|
| 谁产生 | 组件行为发生时 emit | 组件内部状态变化时更新 |
| 谁消费 | TUI / 日志 / 审计 / 回放 | 调度器 / 协调者 / 外部决策 |
| 语义 | "发生了什么"（历史记录） | "现在是什么"（当前快照） |
| 持久化 | JSONL（追加, 不可变） | state.json（覆盖, 可变） |
| 丢失影响 | 丢了不影响控制流 | 丢了影响决策 ← 必须可靠 |
| 跨进程 | broadcast（进程内） | StateLedger（文件持久化） |
| k8s 对应 | `core/v1.Event` | `Status.conditions` |
| lim 对应 | `AgentEvent` / `LoopEvent` | `LoopState` / `ComponentStatus` |

**铁律**：调度决策读 Status，不读 Event。Event 是旁路记录，丢了不影响正确性；Status 是决策来源，必须可靠持久化。

### 9.2 StatusProvider trait

```rust
// crates/lim-core/src/status.rs
pub trait StatusProvider: Send + Sync {
    fn status(&self) -> ComponentStatus;
}

pub struct ComponentStatus {
    pub kind: String,           // "loop_runner" / "scheduler" / ...
    pub id: String,             // 实例标识
    pub phase: String,          // 聚合状态
    pub conditions: Vec<Condition>,
    pub extra: serde_json::Value,
}

pub struct Condition {
    pub r#type: String,         // "BudgetExceeded" / "NoProgress" / ...
    pub status: ConditionStatus, // True / False / Unknown
    pub last_transition_time: u64,
    pub reason: String,
    pub message: String,
}
```

### 9.3 各组件实现

- **LoopRunner**: 暴露当前 goal 的 `LoopState` + budget remaining + verifier kind + no-progress count
- **Scheduler**: 暴露所有 agent 的 `AgentStatus` 列表
- **WorkspaceRegistry**: 暴露已注册 workspace 列表
- **DaemonServer**: 聚合以上所有组件的 status

### 9.4 LoopState 扩展

`LoopState` 新增 `conditions: Vec<Condition>` 字段（`#[serde(default)]` 兜底，不 bump `SCHEMA_VERSION`）：

```rust
pub struct LoopState {
    // ... 既有字段 ...
    pub status: GoalStatus,           // 保留，作为聚合状态
    #[serde(default)]
    pub conditions: Vec<Condition>,   // 新增：细粒度状态
}
```

这样能表达复合状态，例如 "Goal 正在运行但 BudgetExceeded=True 且 NoProgress=True"。

### 9.5 审查规则

- 任何调度决策代码必须读 Status（`StateLedger` / `ComponentStatus`），不得读 Event 历史
- 新增组件若参与调度，必须实现 `StatusProvider`
- `Condition.type` 用形容词/过去动词（`BudgetExceeded` / `VerificationFailed`），不用进行时（`Deploying`）— 学 k8s 约定

---

## 10. 可观测性清单（PR review 用）

任何引入新行为或修改既有行为的 PR，必须对照以下清单自检。**未通过的 PR 不能合并**。

### 10.1 必须项

- [ ] 新增 LLM 调用点？emit `TurnStart` / `MessageEnd`，或显式标注为"隐藏调用"并在本文档第 8 节登记
- [ ] 新增工具？emit `ToolExecutionStart` / `ToolExecutionEnd`（由 `execute_single_tool_call` 自动处理，无需工具实现者关心）
- [ ] 新增错误路径？分类为致命 / 可恢复 / 可降级，分别按 P3 处理
- [ ] 新增隐式行为（默认值、自动派发、排序、过滤）？配 `debug!` log 说明决策
- [ ] 新增持久化数据？通过 `AgentEvent` 持久化，不通过 log
- [ ] 新增 `Arc<dyn Trait>` 共享状态？检查锁是否跨 `.await`（P6）

### 10.2 推荐项

- [ ] 新增 provider 实现？遵循第 6 节的 `debug!` / `trace!` 模式（request / response / SSE event 三层 log）
- [ ] 新增工具？在 `execute()` 内部加 `debug!(tool = name, args = ...)` 入口 log（args 截断 100 字符）
- [ ] 修改 agent_loop 流程？更新第 4 节流程图和信号表

### 10.3 反模式（禁止）

- [ ] `return vec![]` / `unwrap_or_default()` 丢弃错误而不 log → 违反 P3
- [ ] `let _ = result` 丢弃 Result → 违反 P3
- [ ] 新增 `LimError::Other(...)` catch-all → 违反 P5
- [ ] 在 `Mutex` / `RwLock` guard 持有时 `.await` → 违反 P6
- [ ] 通过 `println!` 替代 `tracing` → 违反 [basics.md 第 13 条](../conventions/basics.md#13-日志用-tracing禁止-println)
- [ ] 引入隐式行为但不加 log → 违反 P2

---

## 11. 迭代检查清单（阶段性 review）

每次完成一个 phase 或重大重构后，按此清单审计当前实现是否符合本文档。

### 11.1 Event 覆盖审计

- [ ] 所有 LLM 调用是否都有对应的 `TurnStart` / `MessageStart` / `MessageEnd`？（除第 8 节登记的隐藏调用）
- [ ] 所有工具执行是否都有 `ToolExecutionStart` / `ToolExecutionEnd`？
- [ ] 所有错误是否都 emit 了 `Error` event（致命错误除外，因为它们向上传播）？
- [ ] 所有上下文修改是否都 emit 了 `ContextEdited`（user edits）或 `ContextCompacted`（自动 compaction）？

### 11.2 Log 覆盖审计

- [ ] 所有 provider 是否都有 `debug!("sending request")` + `trace!(body)` + `debug!("stream started")` + `trace!("SSE event")`？
- [ ] 所有降级路径是否都有 `warn!`？
- [ ] 所有重试是否都有 `warn!("retrying ...")` 含 attempt / backoff / error？

### 11.3 黑盒点审计

- [ ] 第 8 节列表是否仍然准确？
- [ ] 是否引入了新的黑盒点未登记？
- [ ] 已登记的黑盒点是否有改进计划？（即使未实施，应记录"何时改进"）

### 11.4 行为链路完整性

- [ ] 一份 session JSONL 是否能完整回放一次 run？测试方法：`replay_transcript(events)` 应该重建出与原始 run 相同的消息序列
- [ ] `replay_edits(events)` 是否能重建出与原始 run 相同的 user edits？
- [ ] `run_continue(prior, new_msg)` 是否产生与原始 run（从同一 prior 出发）相同的后续行为？（最多差异在 LLM 的随机性上）

---

## 12. 相关文档

- [../architecture.md](../architecture.md) — 架构分层、目录结构、依赖图
- [../conventions/basics.md](../conventions/basics.md) — 13 条日常代码准则
- [../conventions/full-guide.md](../conventions/full-guide.md) — 14 节完整规范
- [../review/](../review/) — 历次实现质量 review 记录
- [../archive/completed-phases.md](../archive/completed-phases.md) — 已完成 phase 的技术决策

## Changelog

| 日期 | 变更 | 备注 |
|---|---|---|
| 2026-06-30 | 初始版本 v1.0 | 建立 6 条原则、行为链路总览、11 节结构。作为 review 关键参考 |
| 2026-06-30 | v1.1 — 平台抽象与 shell 选择 | §3.1 启动链路补充 PlatformInfo 检测步骤；新增 §3.3 "PlatformInfo 检测的可观测性"；shell 选择改为启动期一次性决策（PosixSh / GitBash / Wsl / PowerShell）。详见 `crates/lim-core/src/{env,inject}.rs` + `crates/lim-tools/src/shell.rs` |
| 2026-06-30 | v1.2 — 闭合 §8.1 黑盒点 | `ContextCompacted` 新增 `summary_model` / `summary_prompt_tokens` / `summary_completion_tokens` 字段，让 compaction 的隐藏 LLM 调用完全可观测。§8.1 从"部分黑盒"升级为"✅ 完全可观测"。TUI 状态栏显示 `summary cost: N tokens via MODEL` |
| 2026-07-01 | v1.3 — Session schema versioning | 新增 `SessionHeader` event（seq=1，emit 在 AgentStart 之前）+ `lim_core::SCHEMA_VERSION` 常量（当前 = 1）。`SessionReader::read_all` 检测 schema_version：未来版本拒绝读取，旧版本走兼容路径。新增 §7.4 记录设计、检测策略、bump 规则和未来迁移规划。旧文件（无 header）当作 schema_version=0 正常读取，由 `#[serde(default)]` 兜底 |
| 2026-07-02 | v1.4 — Loop Engineering P1–P3 落地 | `LoopRunner::with_event_sender` 注入 `broadcast::Sender<LoopEvent>`，在 goal start/resume、session start/end、verification done、budget checked、no-progress escalation、goal finished、error 处 emit `LoopEvent`。这是 Goal Loop 级别（跨 session）的可观测信号，与 `AgentEvent`（Turn Loop 级别，单 session 内）分离。新增 1 个测试 `event_sender_emits_lifecycle_events` 验证 6 个关键事件都 emit。详见 `loop-engineering.md` §十二 已知问题 #7：目前 LoopEvent 只发 broadcast 不落盘，未来需 spawn 订阅 task 写 `.lim/loop/<goal_id>/events.jsonl`。另外：`register_loop_control_tools` 在 CLI `build_loop_runner` 注册 4 个 AI-native 元工具（`loop_list_goals`/`loop_goal_status`/`loop_pause_goal`/`loop_resume_goal`），让 agent 能在 session 内自省 Goal Loop 状态 |
| 2026-07-02 | v1.5 — P3.5 + P2.5 落地与黑盒登记 | P3.5: CLI 暴露 LlmVerifier 入口（`--verify llm` + `--verifier-provider/model/instructions`），引入 `VerifierCliConfig` 打包参数。P2.5: 新增 `ReviewerVerifier`（`--verify reviewer`），spawn 独立 reviewer agent + fresh context + 工具访问。本次 review 发现 reviewer agent 的 `AgentEvent` 被 `_event_rx` 丢弃（违反 P1），登记为 §8.4 已知黑盒点。改进计划：P4 spawn task 写 `.lim/loop/<goal_id>/review-<N>.jsonl` + 扩展 `LoopEvent::VerificationDone` 携带 `verifier_kind` / `verifier_session_id` / `verifier_tokens_used` 元数据 |
| 2026-07-02 | v1.6 — P4 规划与 Status 系统设计 | 新增 §9 Status 系统设计：借鉴 k8s `Status.conditions`，与 Event 系统形成"控制 vs 观测"双层模型。`StatusProvider` trait + `ComponentStatus` + `Condition` 让组件统一暴露状态。铁律：调度决策读 Status 不读 Event。原 §9/§10/§11 顺延为 §10/§11/§12。§8.4 加 P4.4 闭合计划标注。evolution.md P4 拆解为 11 个子任务（P4.1-P4.11），涵盖 XDG 配置、StatusProvider、LoopEvent 落盘、角色配置、Worktree、Skill、Time-based loop、daemon IPC |
| 2026-07-02 | v1.7 — P4.4 闭合 reviewer 黑盒 | §8.4 从"⚠️ 部分黑盒"升级为"✅ 已闭合"。核心变更：(1) `Verifier::verify()` 签名扩展接收 `VerifyContext { goal_id, loop_dir }`；(2) `ReviewerVerifier` 新增 `ReviewPersister` — spawn task 消费 `event_rx` 写 `.lim/loop/<goal_id>/reviews/review-<N>.jsonl`，reviewer 的 LLM 调用和工具执行完全可回放；(3) `LoopEvent::VerificationDone` 扩展 `verifier_kind`/`verifier_session_id`/`verifier_tokens_used` 字段；(4) `LoopRunner.with_loop_dir()` 启用 LoopEvent 落盘到 `.lim/loop/<goal_id>/events.jsonl`（`LoopEventPersister`）。新增测试 `reviewer_verifier_persists_events_to_loop_dir`。377 测试全通过 |
| 2026-07-03 | v1.8 — P4.5 角色配置 + 分层边界固化 | P4.5 落地 `.lim/agents/<role>.toml` 角色配置系统。核心变更：(1) lim-agent 定义中性 `AgentRoleSpec { system_prompt, model, tools_whitelist, max_turns }`（harness 层，不依赖 lim-config）— 保持 layering 干净；(2) CLI 层实现 `AgentRoleConfig` → `AgentRoleSpec` 转换函数 `role_config_to_spec`（product→harness 边界）；(3) `AgentFactory::for_role(spec, provider, registry, ctx, sessions_dir)` 构造方法；(4) `ReviewerVerifier::from_role_spec(spec, ...)` 构造方法（向后兼容：保留 `new()`）；(5) `ToolRegistry::filtered_clone(&whitelist)` 实现 tools_whitelist 过滤（共享 `Arc<dyn Tool>` 实例，不 clone 工具本身）；(6) CLI `build_loop_runner` reviewer 分支接入：`load_reviewer_role_config()` → 找到则用 role config，找不到则 `default_reviewer_spec()` 回退硬编码默认（pre-P4.5 行为）。Model 优先级：CLI flag > role config > provider default。新增 5 个测试（3 个 `filtered_clone` + 2 个 `from_role_spec`）。382 测试全通过，clippy + fmt 干净 |
| 2026-07-03 | v1.9 — P4.6/P4.7 Worktree + Skill 系统 | P4.6 Worktree 管理：新增 `crates/lim-agent/src/multiagent/worktree.rs`，`WorktreeHandle` RAII 模式（Drop auto-remove），`create_worktree()` async 函数封装 `git worktree add`，`WorktreeSpec { agent_role, branch, base_branch, auto_cleanup }`，9 个测试（6 单元 + 3 集成）。P4.7 Skill 系统：新 crate `crates/lim-skills/`（loader-only，不依赖 lim-agent），`SkillLoader` 解析 YAML frontmatter + markdown body，project 覆盖 global；`SkillVerifier` 在 lim-agent 中 impl `Verifier` trait，programmatic skill 委托 CommandVerifier，non-programmatic 委托 LlmVerifier（body 注入为 extra_instructions），`kind()` 返回 `"skill:command"`/`"skill:llm"` 区分 dispatch。15 个测试（11 loader + 4 SkillVerifier）。 |
| 2026-07-03 | v2.0 — P4.8/P4.9/P4.10 抽象层落地 | 按"先抽象隔离、再简单实现、文件层面分离"原则完成三个抽象层。**P4.8 Time-based loop**：`Timer` trait + `IntervalTimer`（`timer.rs`）+ `TimerLoop` 集成层（`timer_loop.rs`）+ `LoopEvent::TimeTriggered` 变体（`events.rs`）。`TimerLoop` 通过闭包接入 goal runner，不直接依赖 `LoopRunner`；terminal result 停止循环，transient result 等下次触发，goal_runner 错误不杀死 timer loop。11 个测试。**P4.9 daemon IPC**：`IpcTransport` trait + `NullTransport`（`ipc.rs`）+ `DaemonHandle` impl trait（现有 mpsc 提取）+ `GoalRunner` trait + `LoggingGoalRunner`（`goal_runner.rs`）。`DaemonServer::with_goal_runner()` 在 Submit 时 spawn 后台 task 执行 goal，根据 `GoalResult` 更新 ledger state。19 个测试（含 `submit_with_goal_runner_updates_status` 集成测试）。**P4.10 Backoff**：`BackoffStrategy` trait + `FixedBackoff`（`backoff.rs`）+ `LoopRunner::with_backoff()` 集成。`apply_backoff()` 在 `VerifyResult::Fail`（below no-progress threshold）后调用，`Some(delay)` sleep，`None` 升级 NeedsHuman；Pass/resume 时 `reset_backoff()`。Mutex 包装（`&self` 兼容），lock 不跨 await。8 个测试（7 单元 + 1 集成 `backoff_exhaustion_escalates_to_needs_human`）。三个抽象都遵循"trait 在独立文件、简单 impl 同文件或邻近、接入层在调用方"的文件分离原则。待定：cron 库、Unix socket transport、ExponentialBackoff+jitter。 |
| 2026-07-03 | v2.1 — 闭合 B-1/B-2 行为可观测性遗留 | §8.4 黑盒点完全闭合。**B-1**（主 agent 事件持久化）：`LoopRunner::run_goal_loop` 新增 `SessionEventPersister`，每个 session spawn 后台 task 消费 `broadcast::Receiver<EventEnvelope>` 写到 `.lim/loop/<goal_id>/session-<N>.jsonl`。此前 `event_rx` 被丢弃（`_event_rx`），TurnStart/MessageStart/ToolExecutionStart/ToolExecutionEnd/MessageEnd 不落盘。修复 `OpenOptions` 缺少 `.write(true)` 导致文件创建失败的 bug。**B-2**（Verifier trait 元数据）：`Verifier::verify()` 返回类型从 `Result<VerifyResult>` 扩展为 `Result<VerifyOutcome>`，新增 `VerifyMetadata { session_id, tokens_used }`。`ReviewerVerifier` 报告 `session_id`，`LlmVerifier` 追踪 `StreamEvent::Usage` 报告 `tokens_used`，`AllPassVerifier` 聚合子 verifier metadata，`SkillVerifier` 透传被委托 verifier 的 outcome。`LoopEvent::VerificationDone` 的 `verifier_session_id`/`verifier_tokens_used` 从 metadata 填充。新增测试 `loop_dir_persists_session_and_loop_events`（B-1 回归）+ reviewer metadata 断言。170 测试全通过，clippy + fmt 干净。 |
| 2026-07-04 | v2.2 — Session Tree Phase 1 + Compaction Phase 1 | **Session Tree Phase 1（纯留种）**：`AgentMessage` 新增 `parent_id: Option<MessageId>` 字段（`#[serde(default, skip_serializing_if = "Option::is_none")]`），4 个构造器默认 None，新增 `with_parent()` builder。线性 Vec 容器不变，LLM 投影不变，不新增 AgentEvent，不 bump SCHEMA_VERSION。旧 JSONL 字节级兼容。Phase 2 将激活 fork/rewind 语义。**Compaction Phase 1（结构化摘要 + 文件线索）**：`SummarizeStrategy` 现在产出 `StructuredSummary`（9 个 LLM 填充字段 + 3 个运行时填充字段）。LLM prompt 改为 cache-safe `const SUMMARY_INSTRUCTION`（JSON schema 指令），输出经三级 fallback 解析（直接 parse → ```json``` 代码块 → 大括号匹配）。文件级线索由 `extract_file_hints()` 从 `read`/`write`/`edit` 工具调用参数本地确定性提取（不依赖 LLM、不解析 shell）。`CompactionResult` 新增 `summary: Option<StructuredSummary>` + `trigger_reason: CompactionTriggerReason` 字段。解析失败 fallback 到自由文本摘要（保留旧行为）。§8.1 新增 Phase 1 可观测信号登记。506 测试全通过，clippy + fmt 干净。 |
