# lim v2：Rust-native Agent Runtime 构思草案

## 0. 相对 first-draft 的关键修正

这一版不推翻第一版，而是收窄范围、修正安全表述，并把最初目标中的“小说/剧本多 agent 编排”提升为明确的一层。

主要变化：

1. `L0` 不再称为沙箱，而是 **权限策略层**。路径白名单、命令审批、网络开关只能降低误操作风险，不能提供强隔离。
2. “事务性取消”改为 **合作式取消 + 原子文件提交 + 进程组清理 + 事件记录**。文件工具可以尽量事务化，任意 shell 副作用不能承诺无脏状态。
3. 第一阶段 MVP 明显缩小：先跑通 `LLM -> tool_call -> tool_result -> next turn -> event log`，暂不做 TUI、Wasm、远端沙箱和插件市场。
4. 增加 `lim-workflow` / domain pack 层。Writer、Reviewer、Orchestrator 不进入 agent core，而是作为 workflow 节点和领域包能力。
5. 类型安全改为“Rust 类型生成 JSON Schema + 运行时校验 + 最晚反序列化”，避免把 LLM 输出误认为编译期可信。
6. 事件流、content block、usage/cost、replay 成为核心契约，而不是 UI 附属功能。

## 1. 项目定位

`lim` 是一个受 Pi 启发的 Rust-native agent runtime。它不追求一开始成为完整多 agent 平台，而是提供一组稳定、少量、可组合的 primitives，让具体应用在其上构建。

核心哲学：

1. **极薄 agent core**：只处理消息、模型流、工具调用、循环、事件、取消。
2. **控制流归 Rust**：编排、重试、fan-out/fan-in、review loop 由确定性 workflow 控制，不藏在 prompt 里。
3. **安全边界分层**：权限策略、执行器、强隔离分别建模，避免把提示确认当成沙箱。
4. **可重放优先**：所有 agent 运行都能通过 JSONL event log 追踪、压缩、分支和 replay。
5. **领域能力外置**：小说、剧本、代码、研究等垂直场景通过 domain pack 注入 artifact schema、prompt、reviewer 和 workflow。

## 2. Workspace 分层

第一版 crate 数量偏多。v2 将核心路径收敛为可以实际先完成的一组。

| 阶段 | Crate | 职责 | MVP |
| :--- | :--- | :--- | :--- |
| 基础 | `lim-core` | ID、错误、消息、content block、事件、usage、tool schema 类型 | 是 |
| 模型 | `lim-llm` | Provider trait、流式事件、模型元数据、Mock/Fake provider | 是 |
| Agent | `lim-agent` | Agent loop、tool registry、steering/follow-up queue、hooks、取消 | 是 |
| 工具 | `lim-tools` | 内置 read/write/edit/bash 工具，依赖 executor | 是 |
| 执行 | `lim-executor` | 文件/命令执行抽象，Local executor，权限策略入口 | 是 |
| 会话 | `lim-session` | Append-only JSONL event log、branch、summary、replay | 是 |
| CLI | `lim-cli` | 最小命令行入口，先用 stdout 验证闭环 | 是 |
| 编排 | `lim-workflow` | DAG/状态机、多 agent 编排、review loop、fan-out/fan-in | 第二阶段 |
| 运行时 | `lim-runtime` | 配置加载、上下文压缩、skill/domain pack 加载 | 第二阶段 |
| TUI | `lim-tui` | ratatui UI、事件消费、交互确认 | 第三阶段 |
| 扩展 | `lim-extension` | JSON-RPC/MCP、Wasm 插件运行时 | 后置 |

`lim-runtime` 不应成为早期大杂烩。它只负责把 core、agent、session、workflow、config 组装起来，不能反向污染下层 crate。

## 3. 核心数据模型

### 3.1 AgentMessage 与最晚转换

系统内部统一使用 `AgentMessage`。它可以包含 LLM 可见消息，也可以包含 UI、审计、artifact、summary、notification 等非 LLM 消息。

LLM 调用前的路径固定为：

```text
Vec<AgentMessage>
  -> transform_context()
  -> Vec<AgentMessage>
  -> convert_to_llm()
  -> Vec<LlmMessage>
  -> provider.stream()
```

这样可以同时满足三件事：

1. UI 和 session 保留完整历史。
2. compaction 可以注入 summary，而不破坏原始事件。
3. LLM 只看到它该看到的线性上下文。

### 3.2 ContentBlock

消息内容不要只建模成字符串。建议核心支持：

```rust
enum ContentBlock {
    Text { text: String },
    Thinking { text: String },
    ToolCall { id: ToolCallId, name: String, arguments: serde_json::Value },
    Image { data: Vec<u8>, mime_type: String },
    ArtifactRef { artifact_id: ArtifactId, label: Option<String> },
}
```

`ToolCall.arguments` 在 provider stream 阶段可以是 partial JSON；只有进入工具执行前才做 schema validation 和 typed deserialize。

### 3.3 AgentEvent

事件流是 core contract，不只是 TUI contract。

最小事件集合：

```text
agent_start
turn_start
message_start
message_update
message_end
tool_execution_start
tool_execution_update
tool_execution_end
turn_end
agent_end
```

每个事件应携带稳定 ID：

```text
session_id
branch_id
turn_id
message_id
tool_call_id
trace_id
```

这会让 replay、debug、成本统计、TUI 渲染、多 agent fan-in 都简单很多。

## 4. Agent Loop

Agent core 只提供单 agent loop，不硬编码 Plan Mode、Sub-Agent、Writer、Reviewer。

核心循环：

```text
outer loop: follow-up queue
  inner loop: steering queue + assistant turn + tool calls
```

关键机制：

1. `steering`：当前 turn 后立即插入，适合用户中断或纠偏。
2. `follow_up`：agent 原本要停止时再插入，适合追加任务。
3. `before_tool_call`：工具执行前审批、权限检查、参数修正。
4. `after_tool_call`：工具执行后审计、结果修正、可请求 terminate。
5. `prepare_next_turn`：允许切模型、切 thinking level、更新 context。
6. `toolExecution`：默认并行；工具可声明 `sequential`，例如 edit/bash。

### 4.1 取消语义

不要承诺“取消后无脏状态”。更准确的语义：

1. LLM 请求可 abort，保留 partial assistant message，stop reason 记为 `aborted`。
2. 文件写入工具使用临时文件 + fsync + atomic rename；取消时丢弃临时文件。
3. edit 工具先生成 patch / temp output，最后统一提交。
4. bash 工具以进程组启动；取消时先 SIGTERM，超时后 SIGKILL。
5. bash 的 stdout/stderr 和退出状态写入 error tool result，回灌给 LLM。
6. shell 造成的外部副作用只能通过 L1/L2 强隔离降低风险，不能由 L0 保证回滚。

## 5. 安全、权限与执行器

### 5.1 三层安全模型

| 层级 | 名称 | 能力 | 说明 |
| :--- | :--- | :--- | :--- |
| L0 | Permission Policy | 路径白名单、命令审批、网络开关、危险操作确认 | 不是沙箱，只是能力门禁 |
| L1 | Local Isolation | Docker/Podman/bwrap/本地 VM 工具路由 | 适合不完全可信项目 |
| L2 | Remote or MicroVM Isolation | Firecracker/E2B/Modal/OpenShell 类远端执行 | 适合不可信代码、多租户、强隔离 |

Pi 的隔离思路可以借鉴为两种模式：

1. **整体进沙箱**：CLI、扩展、工具、模型请求都在隔离环境里。
2. **host agent + sandbox tools**：认证和 UI 留在 host，read/write/bash 等工具路由进隔离环境。

lim 应该把这两种模式都抽象出来，而不是把安全逻辑塞进 bash tool。

### 5.2 Executor trait

工具层只依赖 executor，不直接碰 `std::process::Command` 或宿主文件系统。

建议能力：

```rust
trait Executor {
    async fn read_file(&self, path: &Utf8Path) -> Result<Vec<u8>>;
    async fn write_file_atomic(&self, path: &Utf8Path, bytes: &[u8]) -> Result<()>;
    async fn apply_edit(&self, edit: FileEdit) -> Result<EditReport>;
    async fn run_command(&self, command: CommandSpec, signal: CancellationToken) -> Result<CommandOutput>;
    fn capabilities(&self) -> ExecutorCapabilities;
}
```

`LocalExecutor` 第一阶段只实现 L0 policy。`ContainerExecutor` 和 `RemoteExecutor` 后置。

## 6. Workflow 与 Domain Pack

这层是 lim 对小说/剧本应用真正有用的部分，但它不应该污染 `lim-agent`。

### 6.1 Workflow primitives

`lim-workflow` 负责确定性控制流：

```text
Node
Edge
State
Artifact
FanOut
FanIn
RetryPolicy
ReviewGate
Budget
Checkpoint
```

典型模式：

1. Prompt chaining：章节大纲 -> 场景计划 -> 草稿 -> 润色。
2. Orchestrator-worker：编排者拆任务，多 writer 并行写不同 scene。
3. Evaluator-optimizer：reviewer 打分，不达标则生成 rewrite instruction。
4. Consensus review：多个 reviewer 对连续性、风格、节奏分别审核。

### 6.2 Novel / Screenplay pack

小说或剧本应用应该作为 domain pack 注入：

```text
novel-pack
  StoryBible
  CharacterProfile
  PlotThread
  ChapterPlan
  ScenePlan
  SceneDraft
  ContinuityReport
  StyleReport
  RewriteInstruction
  WriterAgentSpec
  ReviewerAgentSpec
  EditorWorkflow
```

核心原则：

1. Writer/Reviewer 只是 workflow 节点配置，不是 core 类型。
2. Artifact 有强类型 schema，prompt 只消费 artifact projection。
3. Review loop 由 Rust workflow 决定，LLM 只产出结构化判断和修改建议。
4. 每次生成都落 event log，方便比较不同 writer 分支。

## 7. Session、压缩与 Replay

会话不要只存最终消息，应存 append-only event log。

```text
sessions/{session_id}.jsonl
```

每行是一个事件：

```json
{"event":"message_end","branch_id":"...","turn_id":"...","message_id":"...","payload":{}}
```

派生视图：

1. 当前分支线性上下文。
2. UI transcript。
3. Artifact index。
4. Tool call audit。
5. Token/cost summary。

压缩策略：

1. 原始 JSONL 永不截断。
2. `ContextWindowManager` 只构建 LLM input projection。
3. 摘要作为 `summary` / `system context` 事件追加，不覆盖原消息。
4. summary 应结构化：目标、已完成事实、未解决问题、关键 artifact、禁止遗忘事项。

测试策略：

1. `FakeProvider` 支持脚本化 assistant message、tool call、partial JSON。
2. replay 测试可以在无 API key 情况下复现 agent loop。
3. 每个 bug 都尽量沉淀成 JSONL fixture。

## 8. 扩展策略

扩展系统不要过早 Wasm-first。推荐顺序：

1. **Rust-native tools**：最快验证核心 trait 和 schema。
2. **Process tools / JSON-RPC / MCP**：对接 Python、Node、现有工具生态。
3. **Wasm tools**：用于不可信插件、轻量分发、稳定 ABI。
4. **Skills / Domain packs**：Markdown + frontmatter + trigger + lazy load，用于 prompt 和 workflow 级能力，不等同于可执行插件。

Wasm 是长期正确方向，但第一阶段不该阻塞 agent runtime 成形。

## 9. 配置体系

配置优先级：

```text
defaults
~/.lim/settings.toml
./.lim/settings.toml
environment variables
CLI args
runtime overrides
```

核心配置：

```text
model.provider
model.id
thinking.level
executor.kind
policy.workspace_roots
policy.network
policy.command_approval
context.max_tokens
context.reserve_tokens
session.path
workflow.max_parallelism
workflow.max_review_rounds
```

## 10. 分阶段路线图

### Phase 1：最小 agent runtime

目标：无 TUI、无真实沙箱、无插件，先跑通闭环。

范围：

```text
lim-core
lim-llm
lim-agent
lim-executor(Local + L0 policy)
lim-tools(read/write/edit/bash)
lim-session(JSONL event log)
lim-cli
```

验收：

1. FakeProvider 跑通 `user -> assistant tool_call -> tool_result -> assistant final`。
2. 支持 partial tool call JSON streaming 的事件更新。
3. 工具参数 schema validation 失败时，错误作为 tool result 回灌。
4. `write/edit` 使用原子提交。
5. `bash` 支持取消，并记录 interrupted tool result。
6. JSONL event log 可 replay 出同一条 transcript。

### Phase 2：workflow 与小说应用 spike

目标：验证 lim 是否真的能服务最初的多 agent 创作应用。

范围：

```text
lim-workflow
novel-pack prototype
ArtifactStore
ReviewGate
fan-out/fan-in
```

验收：

1. Orchestrator 把一章拆成多个 scene。
2. 多个 writer 并行生成 scene draft。
3. Reviewer 输出结构化 report。
4. 不合格 scene 自动进入 rewrite loop，最多 N 次。
5. 最终 editor 汇总为 chapter artifact。
6. 所有中间产物可在 session event log 中追踪。

### Phase 3：上下文压缩与 TUI

目标：长任务可交互、可视化、可持续运行。

范围：

```text
ContextWindowManager
branch summary
lim-tui
approval UI
cost/usage panel
```

验收：

1. 长会话触发 summary，不丢原始 JSONL。
2. TUI 流式渲染不阻塞 agent loop。
3. 工具审批、取消、steering/follow-up 可交互。

### Phase 4：强隔离与外部工具

目标：把 L1/L2 和扩展补上。

范围：

```text
ContainerExecutor
RemoteExecutor
Process JSON-RPC / MCP
Wasm tool runtime
```

验收：

1. host agent + sandbox tools 模式可用。
2. 整体进容器模式可用。
3. 外部进程工具可注册 schema 并被 agent 调用。
4. Wasm tool 只能访问显式授予的 host functions。

## 11. 暂缓事项

这些不是不要做，而是不应进入 MVP：

1. 插件市场。
2. 多 provider 全量适配。
3. 远端 microVM 商业平台集成。
4. 完整 TUI。
5. 可视化 workflow builder。
6. 自动 prompt optimizer。
7. 复杂长期记忆系统。

## 12. 当前最小实现顺序

建议从这 10 步开始：

1. 定义 `lim-core` 的 `AgentMessage`、`ContentBlock`、`AgentEvent`、ID 类型。
2. 定义 `lim-llm::Provider` 与 `FakeProvider`。
3. 定义 `Tool` trait 和 schema validation。
4. 实现单 agent loop。
5. 实现事件 stream。
6. 实现 LocalExecutor 的 read/write/bash。
7. 实现 L0 permission policy。
8. 实现 JSONL session writer。
9. 做一个 `lim-cli run --fake fixtures/basic-tool.json`。
10. 用该 fixture 固化第一条 replay 测试。
