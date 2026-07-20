# lim 模块清单与迭代路线

本文档详细列举每个 crate 的子模块、功能职责，以及后续迭代与优化方向。它是 [overview.md](overview.md)（分层架构）的展开，与 [execution-flow.md](execution-flow.md)（运行时链路）互补。

> 维护规则：新增子模块或调整职责时同步更新本文档；已完成的迭代项标记 ✅ 并保留一段时间，未完成的保留为方向性描述。

## 目录结构

```
lim/
  Cargo.toml          # workspace root
  crates/             # 核心 library，按四层分层
    lim-core/         # Primitives — 数据形状 + 中性 trait
    lim-llm/          # Primitives — LLM provider 抽象
    lim-agent/        # Core + Harness — agent loop + loop_runner
    lim-session/      # Core — JSONL 持久化与回放
    lim-executor/     # 横切 — 工具执行能力 + 权限策略
    lim-config/       # Product — 分层配置 + XDG
    lim-skills/       # Harness — skill 加载（loader-only）
    lim-tools/        # Product — 内置工具实现
    lim-workflow/     # Harness — DAG 工作流编排
  apps/
    cli/              # lim 主 binary
    tui/              # 终端 UI
  packs/
    novel/            # 长篇小说创作领域包
```

---

## 1. lim-core（Primitives 层）

**定位**：跨 crate 共享的数据类型和中性 trait。零运行时依赖（不依赖 tokio），只有数据形状。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | `SCHEMA_VERSION`、`LIM_VERSION`、`platform_string()` | 入口 + re-exports + build.rs 注入的版本常量 |
| `id.rs` | `SessionId`、`BranchId`、`TurnId`、`MessageId`、`ToolCallId`、`TraceId`、`GoalId`、`ArtifactId` | ULID-based ID newtype，`ToolCallId` 为 String newtype（兼容 Anthropic `toolu_` 格式） |
| `message.rs` | `AgentMessage`、`LlmMessage`、`Role`、`TurnResult`、`convert_to_llm` | 消息类型 + LLM 投影转换。`AgentMessage` 含 `parent_id`（Session Tree Phase 1 留种） |
| `content.rs` | `ContentBlock`（Text/Thinking/ToolCall/Image/ArtifactRef） | 消息内容块 |
| `event.rs` | `AgentEvent`、`EventEnvelope` | 事件类型 + 信封（seq/session_id/branch_id/trace_id + flattened event） |
| `tool.rs` | `ToolCall`、`ToolResult`、`ToolSchema` | 工具调用数据形状 |
| `usage.rs` | `Usage` | Token 用量 |
| `stop_reason.rs` | `StopReason` | 停止原因（EndTurn/ToolUse/MaxTurns/Cancel/Error 等） |
| `loop_end.rs` | `SessionEndReason` | Session 结束原因（GoalCompleted/NeedsHuman/MaxSteps/Error 等） |
| `prompt.rs` | `PromptLayout`、`PromptLayoutBuilder`、`PromptSegment`、`CacheTier` | 缓存感知的 prompt 布局（Static/Project/Session/Dynamic 四层） |
| `context_edit.rs` | `ContextEdit`、`apply_edit`、`compose_edits` | 上下文编辑描述符（exclude/replace/insert），replay_edits 用于 session resume |
| `guard.rs` | `ToolGuard` trait、`TrustLevel`、`PermissiveGuard`、`WorkspaceGuard` | 工具守卫抽象 + 信任级别 |
| `env.rs` | `PlatformInfo`、`OsKind`、`ArchKind`、`ShellKind` | 平台/Shell 探测（启动期一次性决策） |
| `inject.rs` | `InjectInfo` | 系统提示注入信息 |
| `status.rs` | `ComponentStatus`、`Condition`、`ConditionStatus`、`StatusProvider` | k8s 风格状态系统（控制 vs 观测双层模型） |
| `steering.rs` | `SteeringMessage` | 运行中注入的方向调整消息 |
| `error.rs` | `LimError`、`Result` | 错误类型 |
| `build.rs` | — | `git describe --tags --always --dirty=-dirty` → `LIM_BUILD_VERSION` env |

### 迭代与优化方向

- **Session Tree Phase 2**：激活 `parent_id` 的 fork/rewind 语义，新增 `BranchCreated`/`BranchSwitched` 事件，可能需要 bump `SCHEMA_VERSION`
- **ID 序列化优化**：`ToolCallId` 当前是 String newtype，未来评估是否统一为 ULID + 兼容层
- **PromptLayout 缓存命中率**：当前 cache breakpoint 已就位，但缺实测数据；后续接 provider usage 的 `cache_read_input_tokens` 反馈到 `PromptBuilder` 做自适应
- **Status 系统prod 级落地**：当前 `StatusProvider` trait 已定义但生产代码接入有限，后续需要把 LoopRunner/Daemon/Verifier 的状态查询统一走 Status

---

## 2. lim-llm（Primitives 层）

**定位**：LLM provider 抽象与实现。支持三种 API 格式：OpenAI Chat Completions、OpenAI Responses、Anthropic Messages。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `provider.rs` | `Provider` trait、`LlmRequest` | 核心抽象：`stream(LlmRequest) -> BoxStream<StreamEvent>` + `model_metadata(model)` |
| `stream.rs` | `StreamEvent` | 流式事件（TextDelta/ToolCallDelta/Usage/Error/Done） |
| `model.rs` | `ModelId`、`ModelMetadata` | 模型标识 + 元数据（context_window/max_output） |
| `error.rs` | `ProviderError` | provider 错误分类（RateLimit/Transport/Server/InvalidRequest/Auth/ContextLengthExceeded） |
| `mock.rs` | `MockProvider`、`MockResponse`、`MockToolCall` | 测试用 mock provider，支持 fixture JSON |
| `http.rs` | — | HTTP 工具函数 |
| `providers/mod.rs` | `LimConfig`、`ProviderInstanceConfig`、`ProviderType`、`create_provider` | provider 配置 + 工厂 |
| `providers/config.rs` | — | provider 配置解析 |
| `providers/anthropic.rs` | `AnthropicProvider` | Anthropic Messages API 实现（cache_control 支持） |
| `providers/openai_chat.rs` | `OpenAiChatProvider` | OpenAI Chat Completions 实现 |
| `providers/openai_responses.rs` | `OpenAiResponsesProvider` | OpenAI Responses API 实现 |

### 关键设计

- **Provider endpoint 解析优先级**：`full_url`（最高，绕过所有拼接）> `base_url + protocol suffix` > `type default base_url + suffix`
- **base_url 纯主机**：所有 base_urls 必须是纯 host（无 `/v1`），`/v1` 在 protocol suffix 中
- **SSE 错误传播**：SSE error events 必须通过 `tx.send(Err(...))` 传播，不静默丢弃
- **ToolCallId 一致性**：OpenAI provider 的 tool call streaming 必须为 start/delta 事件生成一致的 ToolCallId

### 迭代与优化方向

- **缓存命中数据回收**：从 Anthropic 响应提取 `cache_read_input_tokens`，填充到 `TurnEnd` 事件的 `Usage.cached_tokens`（当前未实现）
- **OpenAI prompt caching**：OpenAI 依赖隐式前缀稳定性，需要在 `LlmRequest` 构造时保证 system/tools 前缀字节一致
- **更多 provider**：Gemini、Bedrock、Azure OpenAI、本地模型（ollama/llama.cpp）
- **流式 tool call 累积鲁棒性**：当前 `StreamAccumulator` 处理 OpenAI 的分片 JSON 已稳定，但极端边界（网络中断 + 部分 tool call）需要更多测试
- **重试策略**：当前 `agent_loop.rs` 有 MAX_RETRIES=3 + 指数退避，可考虑提取为可配置的 `RetryStrategy`

---

## 3. lim-agent（Core + Harness 层，体量最大）

**定位**：agent loop、hooks、orchestration primitives。包含核心 turn-by-turn 驱动 `AgentLoop`、`AgentHooks`、`ToolRegistry`、`ContextWindowManager`、`StreamAccumulator`，以及三个子模块：`compaction/`、`loop_runner/`、`multiagent/`。

### 3.1 顶层模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `agent_loop.rs` | `AgentConfig`、`AgentLoop`、`AgentRoleSpec` | 核心 turn 循环：消息流转 → LLM 调用 → 工具调度 → 事件 emit。支持 steering 注入、user edits、prompt builder、context window |
| `hooks.rs` | `AgentHooks`、`BeforeToolDecision`、`StopDecision`、`StopContext`、`SessionEndReason` | 统一策略扩展点（机制在此定义，策略由上层实现） |
| `tool.rs` | `Tool` trait | 工具 trait（async，放在 lim-agent 而非 lim-core 是合理妥协） |
| `registry.rs` | `ToolRegistry`、`validate_args` | 工具注册 + 调度 + `filtered_clone(&whitelist)` 支持角色工具过滤 |
| `stream_accumulator.rs` | `StreamAccumulator` | 流式响应累积器，处理分片 tool call |
| `context_window.rs` | `ContextConfig`、`ContextWindowManager`、`CompactionResult`、`CompactionTriggerReason` | context 压缩管理（机制在此，策略通过 `CompactionStrategy` 注入） |
| `prompt_builder.rs` | `PromptBuilder`、`build_system_reminder` | cache-aware prompt 组装（Static/Project/Session/Dynamic 四层 + cache breakpoint） |

### 3.2 compaction/ 子模块（上下文压缩）

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `mod.rs` | re-exports | 模块入口 |
| `strategy.rs` | `CompactRequest`、`CompactionStrategy` trait | 压缩策略抽象 |
| `summarize.rs` | `SummarizeStrategy` | 默认策略：LLM 摘要旧消息 + 保留最近 N 对。支持 `with_provider`/`with_model` 用便宜模型 |
| `summary_schema.rs` | `StructuredSummary` | Phase 1 结构化摘要（9 LLM 字段 + 3 运行时字段），三级 fallback 解析 |
| `file_hints.rs` | `FileHints`、`extract_file_hints` | 本地确定性提取 read/write/edit 文件路径 |

**Phase 1 已完成**（2026-07-04）：结构化摘要 + 文件级线索 + cache-safe `SUMMARY_INSTRUCTION` const + 三级 JSON 解析 fallback。

### 3.3 loop_runner/ 子模块（Loop Engineering 核心，14 个文件）

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `mod.rs` | re-exports | 模块入口 |
| `runner.rs` | `LoopRunner`、`AgentFactory`、`LoopState` | 多 session 协调器：goal → session 序列 + verify → next session |
| `events.rs` | `LoopEvent` | Goal Loop 级事件（GoalStarted/SessionStarted/VerificationDone/GoalFinished 等） |
| `controller.rs` | `Controller` trait、`ControllerRegistry`、`CustomController`、`FixClippyController`、`WriteTestsController`、`NoProgressDetector` | 控制器：决定 session 失败后下一 session 怎么走 |
| `verifier.rs` | `Verifier` trait、`VerifyResult`、`VerifyOutcome`、`VerifyMetadata`、`AlwaysPassVerifier`、`AllPassVerifier`、`CommandVerifier`、`LlmVerifier`、`ReviewerVerifier` | 验证器：决定 goal 是否达成 |
| `ledger.rs` | `LedgerBackend` trait、`StateLedger`、`DaemonLedger`、`WorkspaceBinding` | 状态账本：goal state 持久化 |
| `state.rs` | `GoalStatus`、`GoalRecipe`、`Budget`、`BudgetRemaining` | goal 状态 + 预算 |
| `daemon.rs` | `DaemonServer`、`DaemonCommand`、`DaemonResponse`、`DaemonHandle` | daemon 服务器（跨 workspace goal 管理） |
| `goal_runner.rs` | `GoalRunner` trait、`LoggingGoalRunner`、`GoalResult` | goal 执行抽象（daemon spawn 后台 task 用） |
| `budget.rs` | — | 预算计算（max_sessions/max_tokens） |
| `backoff.rs` | `BackoffStrategy` trait、`FixedBackoff` | 退避策略（verify 失败后 sleep） |
| `timer.rs` | `Timer` trait、`IntervalTimer`、`TimerError` | 定时器抽象（Time-based loop） |
| `timer_loop.rs` | `TimerLoop` | 定时循环集成层 |
| `progress.rs` | `DEFAULT_NO_PROGRESS_THRESHOLD` | 无进展检测阈值 |
| `ipc.rs` | `IpcTransport` trait、`NullTransport` | IPC 传输抽象（当前 NullTransport，未来 Unix socket） |

### 3.4 multiagent/ 子模块（多 agent 协作）

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `mod.rs` | re-exports | 模块入口 |
| `scheduler.rs` | `Scheduler`、`SpawnOptions`、`AgentHandle`、`AgentStatus` | 多 agent 调度器 |
| `topology.rs` | `LocalTopology`、`CentralizedTopology`、`NetworkTopology`、`into_topology_arc` | 拓扑结构（本地/中心化/网络） |
| `transport.rs` | `AgentTransport`、`LocalChannelTransport`、`RoutingHub` | agent 间通信 |
| `workspace.rs` | `Workspace`、`WorkspaceId`、`WorkspaceRegistry` | workspace 管理 |
| `worktree.rs` | `WorktreeHandle`、`WorktreeSpec`、`create_worktree` | git worktree 管理（RAII auto-remove） |

### 迭代与优化方向

- **Compaction Phase 2**：`OverflowRecovery` trigger（provider 返回 context_length_exceeded 后恢复压缩）+ `turn_prefix_summary` 处理 split turn + `trigger_reason` 提升到 `ContextCompacted` event
- **Session Tree Phase 2**：激活 `parent_id`，新增 fork/rewind 语义、BranchCreated/Switched 事件、非线性容器（可能从 Vec 改为树结构）
- **LoopRunner daemon 实化**：当前 `IpcTransport` 是 `NullTransport`，需要实现 Unix socket transport；`DaemonAction::Shutdown` 未实现
- **CentralizedTopology 实化**：当前是 pass-through stub，需要实现 coordinator 策略（过滤、限流、优先级、持久化日志）
- **WorkspaceRegistry 接入 daemon**：当前 daemon 不能跨 goal 聚合查看"某 workspace 下的所有 goal"
- **Backoff 策略扩展**：`ExponentialBackoff` + jitter，与 Budget `min_interval` 字段整合
- **Timer cron 库选择**：当前 `IntervalTimer` 是固定间隔，需要评估 `cron` crate vs 自解析
- **CLI 重构**：`apps/cli/src/main.rs` 已 2000 行单文件，需要拆分子命令模块
- **Verifier trait 扩展**：当前 `VerifyOutcome` 已有 metadata，未来可能增加 `artifacts`（验证产物）

---

## 4. lim-session（Core 层）

**定位**：JSONL 事件日志的持久化、回放、发现。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `writer.rs` | `SessionWriter` | 原子追加事件到 JSONL（`truncate(true)` 而非 `append(true)` 防止路径复用污染） |
| `reader.rs` | `SessionReader` | 读回 JSONL 事件 + `recompute_ids()` |
| `replay.rs` | `replay_transcript`、`replay_edits` | 提取消息流 / 重放 ContextEdited |
| `manager.rs` | `SessionManager`、`SessionMeta`、`SessionDetails` | 枚举 `sessions/` 目录 |

### 迭代与优化方向

- **Session 压缩归档**：长期运行的项目 sessions/ 会膨胀，需要 archive/compact 机制
- **并发写安全**：当前 `SessionWriter` 单写者，未来多 agent 并发写同一 session 需要锁
- **Schema migration 框架**：当前靠 `#[serde(default)]` 兜底，未来需要显式 migration 函数

---

## 5. lim-executor（横切关注点）

**定位**：tool 执行与权限策略。提供 `Executor` trait、默认 in-process 实现 `LocalExecutor`、`DefaultPolicy` 用于命令 denylist + 路径校验。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `executor.rs` | `Executor` trait、`CommandSpec`、`CommandOutput`、`FileEdit`、`EditReport`、`ExecutorCapabilities` | 执行抽象 |
| `local.rs` | `LocalExecutor` | in-process 实现：`permissive()` / `workspace(roots)` 构造器 |
| `policy.rs` | `DefaultPolicy`、`PermissionPolicy` trait | 权限策略（denylist 优先于 allowlist） |

### 迭代与优化方向

- **Sandbox 后端**：当前 `LocalExecutor` 是 in-process，未来接入 sandbox（Landlock/bwrap/Seatbelt/microVM/Wasm）。详见 [research/agent-sandbox-landscape.md](../research/agent-sandbox-landscape.md) 和 [research/mxc-feasibility-study.md](../research/mxc-feasibility-study.md)
- **命令 denylist 配置化**：当前 `DefaultPolicy` 硬编码，未来通过 `[sandbox]` 配置段
- **跨 workspace git 操作**：当前 `git -C <external path>` 默认 blocked，需要 `--trust permissive` 才允许

---

## 6. lim-config（Product 层，P4 引入）

**定位**：分层配置 + XDG Base Directory 支持。三层合并：Global (`~/.config/lim/config.toml`) → Project (`<workspace>/.lim/config.toml`) → CLI flags。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `loader.rs` | `ConfigLoader` | 三层合并加载 |
| `paths.rs` | `ProjectPaths`、`XdgPaths` | 路径解析（项目本地 + XDG） |
| `schema.rs` | `WorkspaceConfig`、`ContextFileConfig`、`ContextFileEntry`、`ContextSearch`、`ServeConfig`、`GoalDaemonConfig`、`SystemServiceConfig`、`AgentRoleConfig` | 配置 schema |
| `context_loader.rs` | `LoadedContext`、`load_context_files` | 上下文文件加载（AGENTS.md 等，支持 4 种查找策略） |
| `error.rs` | `ConfigError`、`ConfigResult` | 错误类型 |

### 关键设计

- **P4 放宽 Containment Principle**：全局配置和跨项目数据迁移到 XDG 路径，项目本地状态（`.lim/loop/`、`.lim/worktrees/`、`sessions/`）仍在项目内
- **Context 文件加载可配置**：无硬编码文件名，`[context]` 段配置查找策略（workspace_root/walk_up/subdirs/fixed），missing file 非致命

### 迭代与优化方向

- **配置 schema 文档化**：当前 schema 散落在代码中，需要生成 JSON Schema 供用户参考
- **热加载**：当前配置在启动期一次性加载，未来支持运行时 reload
- **多项目 workspace 聚合**：XDG 路径已就位，但跨项目查询工具未实现

---

## 7. lim-skills（Harness 层）

**定位**：human-editable 验证技能加载器。skill 是带 YAML frontmatter 的 markdown 文件，编码"如何验证目标已达成"。**loading-only**：只发现/解析/索引，不实现 `Verifier` trait（`SkillVerifier` 在 lim-agent 中实现）。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `loader.rs` | `SkillLoader`、`SkillRegistry`、`parse_skill_file` | 加载器：解析 YAML frontmatter + markdown body，project 覆盖 global |
| `schema.rs` | `Skill`、`SkillFrontmatter` | skill 数据模型 |
| `error.rs` | `SkillError`、`SkillResult` | 错误类型 |

### 迭代与优化方向

- **Skills 索引延迟加载**：当前 startup 时全量加载，未来参考 Pi 的 progressive disclosure——启动时只放索引进 system prompt，命中任务时用 `read` 工具加载完整内容
- **Skill 市场**：跨项目共享 skill 库（XDG 路径已支持）
- **Skill 版本化**：frontmatter 加 version 字段，支持 skill 升级

---

## 8. lim-tools（Product 层）

**定位**：内置核心工具（read / write / edit / shell）。每个工具实现 `lim-agent::Tool` trait，通过 `register_builtins` 注册到 `ToolRegistry`。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | `register_builtins`、`register_loop_control_tools` | 入口 + 注册函数 |
| `read.rs` | `ReadFileTool` | 读文件（支持行号、offset/limit） |
| `write.rs` | `WriteFileTool` | 写文件（覆盖） |
| `edit.rs` | `EditFileTool` | 编辑文件（精确字符串替换，`replace_all` 支持批量） |
| `shell.rs` | `ShellTool`、`ShellBackend` | shell 命令执行（平台中性：Unix POSIX sh / Windows Git Bash/WSL/PowerShell） |
| `loop_control.rs` | — | loop 控制工具（`loop_list_goals`/`loop_goal_status`/`loop_pause_goal`/`loop_resume_goal`，AI-native 自省） |

### 迭代与优化方向

- **MCP 工具支持**：当前只有内置工具，未来通过 MCP 协议接入外部工具
- **Tool 并行执行**：当前工具串行执行，未来参考 Pi 的并行执行 + 按源序写回结果
- **Shell 输出截断策略**：当前有硬上限，未来可配置
- **Edit 工具冲突检测**：当前 `old_string` 必须唯一，未来支持 fuzzy match + 冲突提示

---

## 9. lim-workflow（Harness 层）

**定位**：基于 DAG 的工作流编排。构建 `Node` 的有向无环图，用 Kahn 算法做分层拓扑排序，`WorkflowExecutor` 并发执行每一层。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `graph.rs` | `WorkflowGraph` | DAG 图结构 |
| `node.rs` | `Node`、`NodeKind`、`NodeOutput` | 节点（LLM/Tool/SubWorkflow） |
| `executor.rs` | `WorkflowExecutor` | 分层并发执行 |
| `context.rs` | `WorkflowContext` | 节点间数据传递 |
| `artifact.rs` | `Artifact`、`ArtifactStore` | 工作流产物持久化 |
| `events.rs` | `WorkflowEvent` | 工作流事件 |
| `types.rs` | `NodeId` | 节点 ID |

### 迭代与优化方向

- **动态工作流**：当前是静态 DAG，未来支持运行时动态生成节点（LLM 决定下一步）
- **条件分支**：当前节点都是 must-run，未来支持 if/else 分支
- **循环结构**：当前 DAG 不支持循环，未来支持 while/for 循环（带终止条件）
- **工作流持久化与恢复**：当前 in-memory，未来支持 checkpoint + resume

---

## 10. apps/cli（Product 层）

**定位**：lim 主 binary，clap 子命令分发。单文件 `main.rs`（约 2000 行）。

### 子命令

| 命令 | 说明 |
|---|---|
| `lim init` | 初始化 workspace：创建 `.lim/` + 生成 `config.toml` 模板（`--provider` 预填）+ `AGENTS.md` 模板 |
| `lim run` | 非交互单消息 agent session（`--provider`/`--model`/`--fake`/`--message`/`--output`/`--resume`/`--trust`） |
| `lim tui` | 启动交互式 TUI（`--provider`/`--model`/`--trust`） |
| `lim providers` | 列出 `.lim/config.toml` 中配置的 providers |
| `lim sessions` | 列出/查看 saved sessions（`--details`/`--limit`） |
| `lim edit-context` | 给 saved session 追加 `ContextEdited` 事件（`--exclude`/`--replace`/`--list`） |
| `lim loop` | Loop Engineering：跨多 session 朝目标推进（`--goal`/`--resume`/`--list`/`--status`/`--verify`/`--max-sessions`/`--max-tokens`） |
| `lim serve` | 统一服务入口：foreground/daemon/system 三模式 + MCP/A2A/goal daemon 协议组合（`--protocol`/`--daemon`/`--system`/`--workspace`/`--config`/`--endpoint`/`--auth-token`） |
| `lim goal` | goal 管理首选别名：`submit`/`list`/`status`/`pause`/`resume`/`sync`（`lim loop-daemon <action>` 保留为向后兼容） |
| `lim loop-daemon` | **已废弃**：内部转发到 `lim goal` / `lim serve`。保留为向后兼容别名 |

### 关键私有函数

- `build_executor(trust, platform)` — 根据 trust 构造 `LocalExecutor`
- `run_session(...)` — `lim run` 主流程
- `build_loop_runner(...)` — 构造 `LoopRunner`（provider/executor/registry/verifier/budget/loop_dir）
- `load_reviewer_role_config()` — 从 `.lim/agents/reviewer.toml` 加载 reviewer 角色配置
- `load_project_context()` — 加载 AGENTS.md 等项目上下文文件
- `role_config_to_spec(cfg)` — config 层 → harness 层 spec 转换

### 迭代与优化方向

- **模块拆分**：2000 行单文件需要拆分为 `cmd/run.rs`、`cmd/loop.rs`、`cmd/daemon.rs` 等
- **`lim run` 输出格式**：当前固定 transcript 格式，未来支持 JSON/streaming 输出供脚本消费
- **`lim loop` 状态查询**：当前 `--list`/`--status` 输出简陋，未来支持表格/JSON 格式

---

## 11. apps/tui（Product 层）

**定位**：终端 UI，基于 ratatui + crossterm。三并发任务：输入 poller / agent 事件 bridge / 60fps render loop。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | `TuiConfig`、`run()`、re-exports | 入口 |
| `app.rs` | `App` | 状态机 |
| `approval.rs` | `ApprovalHooks`、`ApprovalMode`、`ApprovalRequest`、`ApprovalResponse`、`ToolLists` | 审批策略实现 |
| `event.rs` | `TuiEvent` | TUI 事件 |
| `terminal.rs` | — | 终端管理 |
| `app/commands.rs` | slash 命令处理、`AgentCommand` | `/help`、`/model`、`/provider`、`/edit`、`/allow`、`/deny`、`/approve`、`/compact`、`/sessions`、`/resume`、`/clear`、`/tokens`、`/exit` |
| `app/agent_event.rs` | — | agent 事件桥接 |
| `app/input.rs` | — | 输入处理 |
| `ui/mod.rs`、`ui/header.rs`、`ui/input.rs`、`ui/messages.rs`、`ui/popup.rs`、`ui/status.rs` | — | UI 组件 |

### Slash 命令

`/help`、`/model <name>`、`/provider <name>`、`/edit <...>`、`/allow <tool>`、`/deny <tool>`、`/approve <mode>`、`/compact`、`/sessions`、`/resume <id>`、`/clear`、`/tokens`、`/exit`

### 迭代与优化方向

- **流式渲染优化**：当前 60fps render，大消息列表可能卡顿，未来用虚拟滚动
- **多 session 切换**：当前单 session，未来支持 tab 式多 session
- **Steering 消息 UI**：当前状态栏显示数量，未来支持 inline 展示
- **图片支持**：当前 `ContentBlock::Image` 已定义但 TUI 不渲染

---

## 12. packs/novel（领域扩展）

**定位**：在 `lim-workflow` 之上构建多节点工作流，把 story bible 分解成 scenes，每个 scene 由 LLM 起草、review、组装成最终 chapter。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `lib.rs` | re-exports | 入口 |
| `types.rs` | `StoryBible`、`SceneOutline`、`ChapterOutline`、`Character`、`ReviewReport` | 数据模型 |
| `workflow.rs` | `build_chapter_workflow` | 构建 chapter 生成工作流 DAG |
| `orchestrator.rs` | — | 编排器 |
| `writer.rs` | — | writer 节点 |
| `editor.rs` | — | editor 节点 |
| `reviewer.rs` | — | reviewer 节点 |
| `bin/novel-cli.rs` | — | novel-cli binary（`chapter` 子命令） |

### 迭代与优化方向

- **novel-cli 打包策略**：当前作为独立 bin，未来决定是随主 release 分发还是作为扩展
- **更多领域包**：novel 是第一个 pack，未来可能有 `packs/research/`、`packs/coding/` 等
- **工作流模板复用**：跨 pack 共享工作流模板

---

## 13. lim-transport（T1+ 传输层 + 协议适配）

**定位**：T1+ 跨进程/跨节点/跨网络传输 + 协议适配层。T0（in-process）留在 lim-agent，本 crate 处理 T0 以上的一切。设计详件见 [transport-layer.md](transport-layer.md)。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `local_endpoint.rs` | `LocalEndpoint`、`EndpointParseError` | 平台感知 IPC 地址（Unix socket / Named Pipe / TCP），endpoint URL 解析 |
| `local_socket.rs` | `LocalSocketTransport` | T0/T1 跨平台本地传输抽象（Unix socket / Named Pipe 统一封装） |
| `unix_socket.rs` *(cfg(unix))* | `UnixStreamTransport` | T1 Unix socket 传输 |
| `named_pipe.rs` *(cfg(windows))* | `NamedPipeClientTransport` | T1 Windows Named Pipe 传输（骨架） |
| `tcp.rs` | `TcpStreamTransport` | T1/T2 TCP 传输 |
| `protocol.rs` | `ProtocolAdapter` trait、`LimNativeAdapter` | 协议适配层：AgentMessage ↔ wire format 互转 |
| `mcp_protocol.rs` | `JsonRpcRequest/Response/Error`、`McpToolDef`、`McpCallToolResult` | JSON-RPC 2.0 + MCP 消息类型 |
| `mcp_server.rs` | `McpServer<H>`、`McpToolHandler` trait、`StaticToolHandler` | MCP Server 模式（lim 作为 server 暴露工具） |
| `mcp_client.rs` | `McpClient`、`McpTool`、`McpToolProxy` | MCP Client 模式（lim 调用外部 MCP server 的工具） |
| `a2a.rs` | `AgentCard`、`A2ATask`、`A2AAdapter`、`A2AHttpTransport`、`A2ARequestHandler` | A2A 协议（HTTP-based agent-to-agent 通信） |
| `discovery.rs` | `ServiceDiscovery` trait、`AgentAddr`、`LocalRegistry`、`TransportTier` | 服务发现（注册/查找 agent 地址） |
| `mdns.rs` | `MdnsDiscovery`、`MdnsServiceRecord` | T2 mDNS 服务发现（in-memory stub，待接 `mdns-sd` crate） |
| `registry.rs` | `RegistryDiscovery`、`RegistryEntry` | T3 A2A registry 服务发现（in-memory stub，待接真实 HTTP registry） |
| `selector.rs` | `TopologySelector`、`TopologyConfig`、`ResolvedTopology` | 拓扑自动选择（从 endpoint 推断 T0-T3 层级） |

### 关键设计

- **不依赖 lim-agent**：lim-agent 的 `AgentTransport` trait 由 app 层注入 T1+ 实现，L1/L2 集成用户可免网络依赖
- **Arc blanket impl**：`impl<T: McpToolHandler + ?Sized> McpToolHandler for Arc<T>`，使 `Arc<dyn McpToolHandler>` 可注入 `McpServer<H>`
- **63 个测试**：a2a 11 + mcp_server 8 + mcp_client 2 + mcp_protocol 7 + selector 11 + local_endpoint 8 + tcp 3 + unix_socket 4 + discovery 5 + protocol 3 + named_pipe 1

### 迭代与优化方向

- **E2 T2 心跳/重连**：基础 TCP 已实现，心跳 + 断线重连待加
- **E10 跨平台 CI**：三平台传输层测试矩阵
- **mDNS（T2）+ A2A registry（T3）**：ServiceDiscovery 的跨节点/跨网络实现

---

## 14. lim-serve（serve 统一入口 + 跨平台运行模式）

**定位**：`lim serve` 命令的运行时。组装 lim-transport 的协议 server + lim-agent 的 handler，提供 foreground / daemon / system 三种运行模式。设计详件见 [serve-mode.md](serve-mode.md)。

### 子模块

| 文件 | 核心类型 | 职责 |
|---|---|---|
| `options.rs` | `ServeOptions`、`RunMode`、`ProtocolKind`、`ProtocolSet`、`ServiceConfig`、`SystemAction`、`ServiceStatus` | 配置选项 + CLI args 序列化（`to_serve_args`） |
| `runner.rs` | `ServeRunner` | 核心 orchestrator：协议 server 编排 + 运行模式分发 + 信号处理 + goal daemon spawn |
| `handlers.rs` | `ToolRegistryMcpHandler`、`AgentFactoryA2AHandler` | 协议 handler 适配器：ToolRegistry → McpToolHandler、AgentFactory → A2ARequestHandler |
| `goal_bridge.rs` | `LoopRunnerGoalRunner`、`spawn_daemon_server` | F7 goal daemon 桥接：LoopRunner 适配 GoalRunner trait + daemon server spawn |
| `a2a_server.rs` | `A2AServer` | 最小 HTTP/1.1 server（GET agent-card + POST JSON-RPC + Bearer auth） |
| `daemon.rs` | `daemonize`、`stop_daemon`、`daemon_pid`、`default_log_dir` | --daemon 模式：spawn detached 子进程 + PID 管理 |
| `pid_file.rs` | `PidFile`、`is_process_alive`、`terminate_process` | PID 文件管理（`~/.lim/lim.pid`） |
| `service_manager.rs` | `ServiceManager` trait、`service_manager()` factory | --system 模式：systemd unit / launchd plist 生成 + 三平台 factory |

### 关键设计

- **Builder 模式注入 handler**：`ServeRunner::new(options).with_mcp_handler(h).with_a2a_handler(h).run()`
- **三路 shutdown**：Ctrl+C + SIGTERM + 外部 CancellationToken，`tokio::select!`（SIGTERM 提取为自由函数避免 `#[cfg]` 在 select 分支内）
- **daemon respawn**：不用 fork+setsid（tokio runtime fork 安全问题），改用 spawn detached 子进程
- **ServiceManager 平台实现**：Linux systemd user unit / macOS launchd LaunchAgent / Windows stub

### 迭代与优化方向

- **Handler 注入**：将 AgentKernel 的 ToolRegistry/AgentLoop 包装为 McpToolHandler/A2ARequestHandler
- **F7 goal-daemon 桥接**：当前为 warn stub，实际 LoopRunner 集成待 handler 注入时一并实现
- **F9 配置段**：`[serve]` / `[serve.mcp]` / `[serve.a2a]` 等配置段
- **Windows Service**：SCM 注册 + ServiceMain（当前为 stub）

---

## 跨模块迭代优先级

### v0.1 Release 前必做

1. **历史压缩**：打 `v0.0.1-pre-squash` tag → 按逻辑阶段 squash 成 5-6 个 commit → 同步文档 hash 引用
2. **musl vs gnu 决策**：是否需要静态链接 Linux 构建（v0.1 决定保持 gnu）
3. **代码签名**：macOS notarization + Windows authenticode（Phase B，v0.1 可选）
4. ~~**novel-cli 打包策略**~~ ✅ 已确认：随主 release 分发（release.yml 已包含）

### v0.2+ 候选

- Compaction Phase 2（Overflow Recovery）
- Session Tree Phase 2（fork/rewind 激活）
- Sandbox 后端落地（Landlock/bwrap/Seatbelt）
- ~~MCP 工具支持~~ ✅ 已完成（lim-transport E5/E6）
- daemon IPC 实化（Unix socket）
- CLI 模块拆分
- ~~Handler 注入（AgentKernel → McpToolHandler/A2ARequestHandler）~~ ✅ 已完成（2026-07-09）
- ~~F9 serve 配置段~~ ✅ 已完成（2026-07-09）

### 长期方向

- 多 agent 并行执行 + 按源序写回
- 动态工作流（LLM 决定节点）
- 跨项目 workspace 聚合查询
- 热加载配置
- Skill 市场
