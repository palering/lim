# lim 执行链路与数据流转

本文档描述 lim 在运行时的完整执行链路和数据流转，从 CLI 入口到 LLM 调用再到工具执行与事件持久化。它是 [overview.md](overview.md)（静态架构）的运行时视角补充，与 [behavior-trace.md](behavior-trace.md)（可观测性）配套阅读。

## 全局视图

```
┌──────────────────────────────────────────────────────────────────┐
│  User                                                            │
│   │                                                              │
│   ▼                                                              │
│  CLI / TUI ──────────────► AgentLoop ──────────► Provider        │
│   │                         │                      │             │
│   │                         │                      ▼             │
│   │                         │                   LLM API          │
│   │                         │                      │             │
│   │                         │                      ▼             │
│   │                         │ ◄───────────────── StreamEvent     │
│   │                         │                      │             │
│   │                         ▼                      ▼             │
│   │                      ToolRegistry ◄─────── ToolCall          │
│   │                         │                      │             │
│   │                         ▼                      ▼             │
│   │                      Executor ◄─────────── CommandSpec       │
│   │                         │                      │             │
│   │                         ▼                      ▼             │
│   │                      ToolResult ◄──────── CommandOutput      │
│   │                         │                                    │
│   │                         ▼                                    │
│   │ ◄───── broadcast::Sender<EventEnvelope> ◄── emit()           │
│   │              │                                               │
│   │              ├──► SessionWriter (JSONL 持久化)                │
│   │              ├──► TUI 渲染                                    │
│   │              └──► LoopEvent 转发（LoopRunner 场景）            │
│   │                                                              │
│   ▼                                                              │
│  User 看到响应                                                    │
└──────────────────────────────────────────────────────────────────┘
```

---

## 一、启动链路

### 1.1 CLI 入口（`apps/cli/src/main.rs`）

```
main() → parse args (clap) → dispatch
  ├─ lim run        → run_session()
  ├─ lim tui        → run_tui() → lim_tui::run(TuiConfig)
  ├─ lim loop       → run_loop() → build_loop_runner() → LoopRunner::run_goal_loop()
  ├─ lim loop-daemon → run_daemon_*()
  ├─ lim providers  → list_providers()
  ├─ lim sessions   → list_sessions() / show_session_details()
  └─ lim edit-context → edit_context()
```

**关键启动步骤**（以 `lim run` 为例）：

1. **平台探测**：`PlatformInfo::detect()` → `OsKind` / `ArchKind` / `ShellKind`（启动期一次性决策，缓存到 `PlatformInfo`）
2. **配置加载**：`ConfigLoader::load()` → 三层合并（Global XDG → Project `.lim/` → CLI flags）
3. **Provider 构造**：`create_provider(&config, provider_name)` → `AnthropicProvider` / `OpenAiChatProvider` / `OpenAiResponsesProvider`
4. **Executor 构造**：`build_executor(trust, platform)` → `LocalExecutor::permissive()` 或 `LocalExecutor::workspace(roots)`
5. **ToolRegistry 构造**：`ToolRegistry::new()` + `register_builtins(&registry, executor)` → 注册 `read`/`write`/`edit`/`shell`
6. **ContextWindowManager**（可选）：`ContextWindowManager::new(ContextConfig)` → 注入到 `AgentLoop`
7. **PromptBuilder**（可选）：加载 AGENTS.md 等上下文文件 → `PromptBuilder::with_project_context()`
8. **AgentLoop 构造**：`AgentLoop::new(provider, registry, event_tx, cancel)` + `.with_config()` + `.with_hooks()` + `.with_context_window()` + `.with_prompt_builder()`
9. **SessionWriter 订阅**：spawn task 消费 `broadcast::Receiver<EventEnvelope>` 写 `sessions/<id>.jsonl`
10. **运行**：`agent_loop.run(initial_messages).await` → 输出 transcript

### 1.2 TUI 启动（`apps/tui`）

TUI 启动三个并发 tokio task：

```
lim_tui::run(TuiConfig)
  │
  ├── Task 1: 输入 poller ────► mpsc::Sender<AgentCommand>
  │                              │
  ├── Task 2: agent 后台 task ◄──┘ (消费 AgentCommand)
  │   │                          │
  │   │   AgentLoop ◄── broadcast::Sender<EventEnvelope>
  │   │                          │
  │   └──► mpsc::Sender<TuiEvent> │
  │                              │
  └── Task 3: render loop ◄──────┘ (60fps)
```

**AgentCommand** 是 TUI → agent 的控制通道：`Message(String)` / `SetModel(String)` / `SetProvider(...)` / `ApplyEdit(ContextEdit)` / `ClearEdits` / `Compact` / `ResumeSession(...)`。

---

## 二、单 Turn 执行链路（AgentLoop::run_inner）

这是 lim 的核心循环。一次 `run_inner` 调用 = 一个完整 turn（User/Assistant 消息回合）。

### 2.1 总体流程

```
run_inner(messages)
  │
  ├── 1. Cancel 检查 (CancellationToken)
  │
  ├── 2. Steering 消息注入 (turn 边界)
  │      └─ drain steering_rx → 注入为 User 消息 → emit SteeringMessageApplied
  │
  ├── 3. Layer 1: System reminder 注入 (动态信息：cwd/timestamp)
  │
  ├── 4. Layer 2: User edits (持久化上下文编辑)
  │      └─ compose_edits(&user_edits) → apply_edit(messages, composed)
  │
  ├── 5. Layer 3: Context compaction (token 预算)
  │      └─ cwm.compact(working_messages, provider, model)
  │           ├─ 未超预算 → 直接 convert_to_llm
  │           └─ 超预算 → SummarizeStrategy.compact()
  │                ├─ find_recent_start (保留最近 N 对)
  │                ├─ extract_file_hints (本地提取 read/write/edit 路径)
  │                ├─ summarize() — 隐藏 LLM 调用 (cache-safe forking)
  │                ├─ StructuredSummary::parse_from_llm_output (三级 fallback)
  │                ├─ 填充 read_files/modified_files
  │                └─ render_for_projection → 插入为 User 消息
  │           → emit ContextCompacted (如发生)
  │
  ├── 6. LlmRequest 组装
  │      ├─ LlmRequest::new(model, llm_messages)
  │      ├─ .with_tools(tool_schemas)
  │      └─ .with_prompt_layout(layout) 或 .with_system(system_prompt)
  │
  ├── 7. emit TurnStart (turn_id, model, message_count, tool_count, tool_names, system_prompt_len)
  │
  ├── 8. Provider 流式调用
  │      └─ provider.stream(request) → BoxStream<StreamEvent>
  │           ├─ TextDelta { text } ─────────► emit MessageUpdate
  │           ├─ ToolCallStart { id, name } ──► emit MessageUpdate
  │           ├─ ToolCallDelta { id, args } ─► emit MessageUpdate
  │           ├─ Usage(u) ───────────────────► 暂存（TurnEnd 用）
  │           ├─ Error(e) ────────────────────► 重试判定
  │           └─ Done ────────────────────────► 结束流
  │
  ├── 9. StreamAccumulator 累积为 AgentMessage (assistant)
  │      └─ emit MessageStart → MessageUpdate* → MessageEnd
  │
  ├── 10. Tool 调用循环 (如果 stop_reason == ToolUse)
  │       └─ for each ToolCall in assistant_message:
  │            ├─ hooks.before_tool_call() → Proceed/Reject/ModifyArgs
  │            ├─ emit ToolExecutionStart { tool_call_id, tool_name, started_at }
  │            ├─ registry.execute(tool_call) → ToolResult
  │            │    └─ Tool::execute() → Executor::execute_command/read_file/write_file/edit_file
  │            ├─ emit ToolExecutionEnd { tool_call_id, result, ended_at }
  │            └─ 追加 Tool 消息到 messages
  │
  ├── 11. emit TurnEnd { turn_id, stop_reason, usage }
  │
  ├── 12. hooks.should_stop() → StopSession / Continue
  │
  └── 13. 如 stop_reason == ToolUse 且未 StopSession → 回到 step 1 (下一个 turn)
         否则 → 返回 TurnResult
```

### 2.2 关键数据转换

**AgentMessage → LlmMessage 投影**：

```
AgentMessage { id, role, content: Vec<ContentBlock>, parent_id }
                         │
                         ▼ convert_to_llm()
              LlmMessage { role, content: Vec<ContentBlock>, tool_call_ids }
                         │
                         ▼ provider-specific serialization
              JSON payload (Anthropic/OpenAI format)
```

**Context 投影三层管线**（在 step 3-5）：

```
原始 messages (AgentMessage Vec, 完整历史，永不修改)
        │
        ▼ Layer 1: System reminder (动态：<system-reminder> 块)
working_messages
        │
        ▼ Layer 2: User edits (apply_edit + compose_edits)
working_messages (编辑后)
        │
        ▼ Layer 3: ContextWindowManager (compaction，只影响 LlmMessage 投影)
llm_messages (Vec<LlmMessage>，可能含 summary + recent)
```

**注意**：三层管线只影响 LLM 看到的内容，原始 `AgentMessage` 历史永不修改。这是 "History is Sacred" 原则。

### 2.3 事件发射时机

每个事件都在固定时机 emit，保证 JSONL 能完整回放：

| 事件 | 时机 | 携带数据 |
|---|---|---|
| `SessionHeader` | session 开始（seq=1） | lim_version, schema_version, platform |
| `AgentStart` | run() 入口 | — |
| `SteeringMessageApplied` | turn 边界（drain steering 后） | count, contents, enqueued_at |
| `TurnStart` | LlmRequest 组装后、stream 调用前 | turn_id, model, message_count, tool_count, tool_names |
| `MessageStart` | 第一个 chunk 到达 | message_id, role |
| `MessageUpdate` | 每个 TextDelta/ToolCallDelta | message_id, content_index, delta |
| `MessageEnd` | 流结束、消息组装完成 | message_id, message |
| `ToolExecutionStart` | tool 调用前 | tool_call_id, tool_name, started_at (ms) |
| `ToolExecutionUpdate` | tool 流式输出（当前未用） | tool_call_id, output |
| `ToolExecutionEnd` | tool 完成 | tool_call_id, result, ended_at (ms) |
| `TurnEnd` | turn 结束 | turn_id, stop_reason, usage |
| `ContextCompacted` | compaction 发生时 | messages_before/after, tokens_before/after, summary_model/tokens |
| `ContextEdited` | 用户交互编辑时 | edit |
| `Error` | 可恢复错误 | message, retryable, retry_count, max_retries |
| `SessionEnd` | 提前结束 | reason |
| `AgentEnd` | run() 正常结束 | — |

---

## 三、Loop Engineering 链路（LoopRunner）

`lim loop` 命令触发的多 session 协调流程。

### 3.1 Goal Loop 总体流程

```
lim loop --goal "..." --verify "cargo test"
  │
  ├── build_loop_runner() → LoopRunner
  │
  └── LoopRunner::run_goal_loop(goal)
        │
        ├── 1. 初始化 StateLedger (goal state 持久化到 .lim/loop/<goal_id>/)
        ├── 2. emit LoopEvent::GoalStarted
        │
        ├── 3. Session Loop (直到 goal 完成 或 budget 耗尽)
        │     │
        │     ├─ a. Budget 检查 (max_sessions / max_tokens)
        │     │     └─ 超出 → emit GoalFinished(BudgetExhausted) → 退出
        │     │
        │     ├─ b. 构造 AgentFactory + spawn SessionRunner
        │     │     └─ AgentFactory::create_agent() → AgentLoop
        │     │
        │     ├─ c. SessionEventPersister spawn 后台 task
        │     │     └─ 消费 broadcast::Receiver<EventEnvelope> → 写 .lim/loop/<goal_id>/session-<N>.jsonl
        │     │
        │     ├─ d. AgentLoop::run(messages).await → TurnResult
        │     │
        │     ├─ e. emit LoopEvent::SessionEnded
        │     │
        │     ├─ f. Verifier::verify(VerifyContext)
        │     │     ├─ AlwaysPassVerifier → 直接 Pass
        │     │     ├─ CommandVerifier → Executor::execute_command("cargo test")
        │     │     ├─ LlmVerifier → 独立 LLM 调用评估（无工具）
        │     │     ├─ ReviewerVerifier → spawn 独立 reviewer agent（有工具）
        │     │     │     └─ ReviewPersister 写 .lim/loop/<goal_id>/reviews/review-<N>.jsonl
        │     │     └─ SkillVerifier → 委托 Command/LLM verifier
        │     │     → 返回 VerifyOutcome { result: Pass/Fail, metadata: { session_id, tokens_used } }
        │     │
        │     ├─ g. emit LoopEvent::VerificationDone
        │     │
        │     ├─ h. 判定
        │     │     ├─ Pass → emit LoopEvent::GoalFinished(GoalCompleted) → 退出
        │     │     ├─ Fail + 未达 no-progress threshold → Controller 决定下一步
        │     │     │     ├─ CustomController → 用户自定义逻辑
        │     │     │     ├─ FixClippyController → 注入 "fix clippy" 指令
        │     │     │     └─ WriteTestsController → 注入 "write tests" 指令
        │     │     ├─ Fail + 达到 no-progress threshold → BackoffStrategy
        │     │     │     ├─ Some(delay) → sleep → 下一 session
        │     │     │     └─ None → 升级 NeedsHuman → emit GoalFinished → 退出
        │     │     └─ Error → emit GoalFinished(GoalError) → 退出
        │     │
        │     └─ i. 回到 a (下一 session)
        │
        └── 4. 清理（保留 .lim/loop/<goal_id>/ 持久化状态）
```

### 3.2 Goal 状态流转

```
            ┌──────────┐
            │ Pending  │ (submit 后)
            └────┬─────┘
                 ▼
            ┌──────────┐
            │ Running  │ ◄──────────┐
            └────┬─────┘            │
                 │                  │
        ┌────────┼────────┐         │
        ▼        ▼        ▼         │
  ┌──────┐ ┌──────┐ ┌──────────┐    │
  │Paused│ │ Error│ │Completed │    │
  └──┬───┘ └──────┘ └──────────┘    │
     │ resume                        │
     └───────────────────────────────┘
                 │
                 ▼ verify Fail + 无进展
            ┌──────────┐
            │NeedsHuman│
            └──────────┘
```

### 3.3 Verifier 数据流

```
session N 完成
     │
     ▼
VerifyContext { goal_id, loop_dir, messages? }
     │
     ▼
Verifier::verify()
     │
     ├─ AlwaysPassVerifier → VerifyOutcome { result: Pass, metadata: None }
     │
     ├─ CommandVerifier
     │    └─ Executor::execute_command("cargo test")
     │         → exit_code == 0 ? Pass : Fail
     │
     ├─ LlmVerifier
     │    └─ 独立 LLM 调用（system prompt = "Evaluate if goal is achieved"）
     │         → 解析 LLM 输出 → Pass/Fail
     │         → metadata.tokens_used = StreamEvent::Usage 累积
     │
     └─ ReviewerVerifier
          └─ spawn 独立 reviewer agent
               ├─ fresh context（不继承主 agent 历史）
               ├─ 工具访问（read/shell，无 write/edit）
               ├─ ReviewPersister 消费 event_rx → 写 review-<N>.jsonl
               └─ agent 最终消息 → 解析 → Pass/Fail
                    → metadata.session_id = reviewer session_id
                    → metadata.tokens_used = 累积 usage
```

---

## 四、Compaction 数据流（详细）

```
working_messages (AgentMessage Vec)
        │
        ▼ ContextWindowManager::compact()
        │
        ├─ tokens_before = last_known_input_tokens OR estimate_tokens(messages)
        │
        ├─ budget = context_window - max_output_tokens - compact_buffer_tokens
        │
        ├─ if tokens_before <= budget → 直接 convert_to_llm → 返回（无 compaction）
        │
        └─ else → 委托给 CompactionStrategy
                │
                ▼ SummarizeStrategy::compact()
                │
                ├─ recent_start = find_recent_start(messages, keep_recent_pairs)
                │    └─ 从后向前找第 N 个 User 消息，调整边界保持 tool_call→result 完整
                │
                ├─ (system_msgs, old_text) = prepare_compaction(&messages[..recent_start])
                │
                ├─ file_hints = extract_file_hints(&messages[..recent_start])
                │    └─ 扫描 ContentBlock::ToolCall
                │         ├─ "read" → read_files (deduped, first-seen order)
                │         ├─ "write"/"edit" → modified_files
                │         └─ "shell" → 忽略（路径提取不可靠）
                │
                ├─ provider/model 选择
                │    ├─ override_provider (with_provider) → 用便宜模型
                │    └─ main_provider (默认) → 同主 agent
                │
                ├─ system_prompt_text (cache-safe forking)
                │    └─ 从 system_msgs 提取 Text 块 → 共享 prompt cache 前缀
                │
                ├─ summarize(old_text, provider, model, system_prompt, tools)
                │    ├─ summary_prompt = SUMMARY_INSTRUCTION + "<conversation>" + old_text + "</conversation>"
                │    ├─ LlmRequest::new(model, [User(summary_prompt)])
                │    │    .with_system(system_prompt)  // cache 命中
                │    │    .with_tools(tools)           // cache 命中
                │    ├─ provider.stream(request)
                │    └─ 累积 TextDelta → summary_text (raw LLM output)
                │
                ├─ StructuredSummary::parse_from_llm_output(summary_text)
                │    ├─ Level 1: 直接 JSON parse
                │    ├─ Level 2: 提取 ```json``` 代码块
                │    └─ Level 3: 大括号匹配提取首个 {...}
                │
                ├─ if parse 成功 && !is_llm_output_empty:
                │    ├─ summary.read_files = file_hints.read_files
                │    ├─ summary.modified_files = file_hints.modified_files
                │    ├─ summary_text = summary.render_for_projection()
                │    │    └─ 固定 section 顺序：Goal/Constraints/Done/.../Files Read/Files Modified
                │    └─ CompactionResult.summary = Some(summary)
                │
                └─ else (parse 失败 或 LLM 输出空):
                     ├─ summary_text 保持原样 (free-text fallback)
                     └─ CompactionResult.summary = None
                │
                ▼
        CompactionResult {
            messages: [system_msgs..., summary_msg, recent_msgs...],
            was_compacted: true,
            summary: Option<StructuredSummary>,
            trigger_reason: AutoThreshold,
            ...
        }
                │
                ▼ emit ContextCompacted { messages_before/after, tokens_before/after, summary_model/tokens }
```

---

## 五、Session 持久化与回放

### 5.1 写入链路

```
AgentLoop.emit(event)
  │
  ▼ self.event_tx.send(EventEnvelope)  (broadcast)
  │
  ├──► SessionWriter 订阅者（spawn task）
  │     └─ 消费 broadcast::Receiver<EventEnvelope>
  │         └─ SessionWriter::write_event(&envelope)
  │              └─ serde_json::to_string(&envelope) + "\n"
  │                   → atomic write to sessions/<session_id>.jsonl
  │
  ├──► TUI 订阅者
  │     └─ App::handle_event(TuiEvent) → 更新 UI 状态
  │
  └──► LoopEventPersister（LoopRunner 场景）
        └─ 转发为 LoopEvent → 写 .lim/loop/<goal_id>/events.jsonl
```

### 5.2 回放链路（`--resume`）

```
lim run --resume <session_id_or_path>
  │
  ▼ resolve_resume_path() → sessions/<id>.jsonl
  │
  ▼ SessionReader::read_all(path)
  │     └─ 逐行 deserialize EventEnvelope
  │         └─ envelope.recompute_ids()
  │
  ▼ replay_transcript(&envelopes) → Vec<AgentMessage>
  │     └─ 从 MessageEnd 事件提取 AgentMessage
  │
  ▼ replay_edits(&envelopes) → Vec<ContextEdit>
  │     └─ 从 ContextEdited 事件提取 edit
  │
  ▼ AgentLoop::new(...)
  │     .with_user_edits(edits)  // 不 emit ContextEdited（已在历史 JSONL 中）
  │
  ▼ agent_loop.run(replayed_messages).await
       └─ 用户输入新消息 → 继续 turn 循环
```

---

## 六、配置数据流

```
~/.config/lim/config.toml   ──┐
                                ├─► ConfigLoader::load()
<workspace>/.lim/config.toml ──┤     │
                                │     ▼
CLI flags (--provider/--model)─┘  WorkspaceConfig
                                       │
                                       ├─ providers: HashMap<String, ProviderInstanceConfig>
                                       │     └─ create_provider(&config, name) → Arc<dyn Provider>
                                       │
                                       ├─ default_provider / default_model
                                       │
                                       ├─ [context] files (AGENTS.md 等)
                                       │     └─ load_context_files() → LoadedContext
                                       │          └─ PromptBuilder::with_project_context()
                                       │
                                       ├─ [agents.<role>] (如 reviewer)
                                       │     └─ AgentRoleConfig → role_config_to_spec() → AgentRoleSpec
                                       │
                                       └─ [skills] paths
                                            └─ SkillLoader::load() → SkillRegistry
```

---

## 七、关键设计决策

### 7.1 broadcast channel 作为边界

`AgentLoop` 不持有 `SessionWriter`，只通过 `broadcast::Sender<EventEnvelope>` 发事件。这保证了：

- 核心循环与持久化解耦（SessionWriter 是可选订阅者）
- 多订阅者支持（TUI + SessionWriter + LoopEventPersister 同时订阅）
- 无订阅者时不报错（`let _ = tx.send(envelope)`）

### 7.2 AgentMessage 永不修改

所有 context 操作（user edits、compaction、steering 注入）只在 **LlmMessage 投影层** 进行。原始 `AgentMessage` Vec 是不可变历史，保证：

- Session 回放能还原真实对话
- User edits 可随时清除（回到原始历史）
- Compaction 失败可回退

### 7.3 隐藏 LLM 调用的可观测性

Compaction 触发的 LLM 调用不 emit `TurnStart`/`MessageEnd`（避免污染主 turn 事件流），但其元数据通过 `ContextCompacted` event 完全持久化：

- `summary_model`：用哪个模型做摘要
- `summary_prompt_tokens` / `summary_completion_tokens`：token 成本
- `debug!` log：启动时记录 model + conversation_chars
- `warn!` log：失败/降级时记录原因 + token 用量

详见 [behavior-trace.md §8.1](behavior-trace.md#81-contextwindowsummarize-的隐藏-llm-调用已可观测)。

### 7.4 Cache-safe forking

Compaction 的 LLM 调用复用主对话的 system prompt 和 tools，从 provider 视角看就像"上一轮 + 一个 compaction 消息"，最大化 prompt cache 命中：

- Anthropic：`cache_control` breakpoint 在 system prompt 末尾
- OpenAI：依赖隐式前缀稳定性（system + tools 字节一致）
- `SUMMARY_INSTRUCTION` 是 `const &str`，跨压缩调用字节一致

### 7.5 Trust Level 分级

```
Workspace (默认)
  ├─ 工具执行限制在 workspace roots 内
  ├─ 跨 workspace git 操作 blocked
  └─ 命令 denylist 生效

Permissive (--trust permissive)
  ├─ 无路径限制
  ├─ 跨 workspace git 允许
  ├─ 命令 denylist 仍生效
  └─ 强制 ApprovalMode::Always（不能用 Auto）
```

---

## 八、调试与排查

### 8.1 查看事件流

```bash
# 实时查看 session JSONL
tail -f sessions/<session_id>.jsonl | jq .

# 提取所有 turn 的 stop_reason 和 token usage
cat sessions/<id>.jsonl | jq 'select(.event == "turn_end") | {turn_id, stop_reason, usage}'

# 查看隐藏的 compaction LLM 调用成本
cat sessions/<id>.jsonl | jq 'select(.event == "context_compacted") | {summary_model, summary_prompt_tokens, summary_completion_tokens}'

# 查看 tool 执行耗时
cat sessions/<id>.jsonl | jq 'select(.event == "tool_execution_end") | {tool_call_id, ended_at, started_at: .started_at}'
```

### 8.2 查看日志

```bash
# 启动时带 RUST_LOG 看详细链路
RUST_LOG=lim_agent=debug,lim_llm=debug lim run --provider anthropic --message "Hello"

# 只看 compaction 相关日志
RUST_LOG=lim_agent::compaction=debug lim run ...
```

### 8.3 回放 session

```bash
# 列出 sessions
lim sessions --limit 10

# 查看某 session 详情
lim sessions --details sessions/<id>.jsonl

# resume（继续对话）
lim run --resume <session_id> --provider anthropic

# 编辑历史（排除某些消息）
lim edit-context --session sessions/<id>.jsonl --exclude "1,3,5" --list
```

---

## 九、性能特征

### 9.1 关键路径延迟

| 操作 | 典型延迟 | 备注 |
|---|---|---|
| 单 turn（无 tool） | 1-5s | 取决于 LLM 响应时间 |
| 单 turn（含 1 tool） | 2-10s | + tool 执行时间 |
| Compaction | 3-15s | 隐藏 LLM 调用，cache 命中可减半 |
| Session resume | <100ms | 读 JSONL + replay |
| Provider 切换 | <10ms | 仅替换 Arc<dyn Provider> |

### 9.2 内存占用

- `AgentMessage` 历史：随对话增长，未压缩
- `LlmMessage` 投影：每次 turn 重新生成，turn 结束后释放
- `EventEnvelope` 订阅者 buffer：默认 256 容量，溢出时丢最旧

### 9.3 Token 成本

- 主 turn：用户可见，`TurnEnd.usage` 记录
- Compaction 隐藏调用：`ContextCompacted.summary_*_tokens` 记录
- Verifier LLM 调用：`LoopEvent::VerificationDone.verifier_tokens_used` 记录
- Prompt cache 命中：当前未回收 `cache_read_input_tokens`（计划中）

---

## 十、相关文档

- [overview.md](overview.md) — 静态分层架构与设计原则
- [modules.md](modules.md) — 每个 crate 的子模块清单与迭代路线
- [behavior-trace.md](behavior-trace.md) — 行为链路与可观测性（黑盒点、审查规则）
- [compaction.md](compaction.md) — Compaction 设计细节
- [loop-engineering.md](loop-engineering.md) — Loop Engineering 完整设计
- [../functional-testing.md](../functional-testing.md) — 功能测试用例与试用指南
