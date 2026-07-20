# lim 集成指南：从单次调用到多 agent 编排

本文档面向**外部程序开发者**——如果你在写一个 GUI 应用（如小说编写工具）、一个 Web 服务、或任何需要 AI agent 能力的产品，该如何引入 lim？

我们按复杂度演进，从最简单的"调一次 LLM"到最复杂的"多角色 agent 团体作战"，每一层都用**小说编写工具**作为贯穿案例。

> **核心思路**：lim 不是 monolithic 框架，而是按 crate 拆分的层次化能力。你可以**按需依赖**某一层，不需要一次性引入全部。

## 复杂度演进路线

```
┌─────────────────────────────────────────────────────────────┐
│  L3: 多 agent 编排                                          │
│  编排者 + 编辑 + 写手 + 灵感 + 市场反馈 团体作战             │
│  → lim multiagent / lim_workflow / loop_runner              │
├─────────────────────────────────────────────────────────────┤
│  L2: Agent + 工具                                           │
│  agent 调用拆书/绘图/改编工具，带审批/压缩/持久化            │
│  → lim_agent + lim_tools + 自定义 Tool                      │
├─────────────────────────────────────────────────────────────┤
│  L1: 单次 LLM 调用                                          │
│  调不同 AI 写一段范本，流式输出                              │
│  → lim_llm                                                  │
├─────────────────────────────────────────────────────────────┤
│  L0: 数据类型                                               │
│  消息/内容块/ID 等纯数据形状                                │
│  → lim_core                                                 │
└─────────────────────────────────────────────────────────────┘
```

| 层 | 用到的 crate | 典型场景 |
|---|---|---|
| L0 | `lim-core` | 只需要消息类型（如自建 agent 的人） |
| L1 | `lim-core` + `lim-llm` | 调 LLM 生成文本，无工具 |
| L2 | + `lim-agent` + `lim-executor` + `lim-tools`（可选） | Agent 调用工具完成任务 |
| L3 | + `lim-workflow` 或 `lim-agent/multiagent` 或 `lim-agent/loop_runner` | 多 agent 编排协作 |

---

## 准备工作

### Cargo.toml 依赖

```toml
[dependencies]
# L1 起步
lim-core = { path = "../lim/crates/lim-core" }   # 或 git = "..."
lim-llm   = { path = "../lim/crates/lim-llm" }

# L2 起步
lim-agent   = { path = "../lim/crates/lim-agent" }
lim-executor = { path = "../lim/crates/lim-executor" }
lim-tools   = { path = "../lim/crates/lim-tools" }   # 可选：内置工具

# L3 起步（按编排模式选其一）
lim-workflow = { path = "../lim/crates/lim-workflow" }   # 静态 DAG

tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### Provider 配置

lim 的 provider 配置走 TOML。可以在程序内构造，或从文件加载：

```rust
use lim_llm::providers::{LimConfig, ProviderInstanceConfig, ProviderType, create_provider};

// 方式 1：代码内构造配置
let mut config = LimConfig::default();
config.providers.insert("anthropic".into(), ProviderInstanceConfig {
    name: "anthropic".into(),
    provider_type: ProviderType::Anthropic,
    api_key_env: Some("ANTHROPIC_API_KEY".into()),
    api_key: None,
    base_url: None,
    full_url: None,
    default_model: "claude-sonnet-4-20250514".into(),
    protocol_suffix: "/v1".into(),
    extra_headers: Default::default(),
});

// 方式 2：从 .lim/config.toml 加载（推荐，让用户可自定义）
// let config = LimConfig::load_from_default_paths()?;

let provider = create_provider(&config, "anthropic")?;  // Arc<dyn Provider>
```

---

## L1：单次 LLM 调用

### 场景

小说编写工具的"范本生成"功能：用户选一个模型（Claude/GPT），点"生成"，得到一段小说开篇。

### 用到的能力

- `lim_llm::Provider` trait — 统一 LLM 调用接口
- `lim_llm::LlmRequest` — 请求构造
- `lim_llm::StreamEvent` — 流式响应
- `lim_core::message` — 消息类型

### 代码示例

```rust
use std::sync::Arc;
use futures::StreamExt;
use lim_core::message::{AgentMessage, Role, ContentBlock};
use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

async fn generate_opening(provider: Arc<dyn Provider>, model: &str, prompt: &str) -> anyhow::Result<String> {
    // 1. 构造消息——lim 的消息就是 AgentMessage Vec
    let messages = vec![
        AgentMessage::system("你是一位小说家，擅长写科幻开篇。"),
        AgentMessage::user(prompt),
    ];

    // 2. 转成 LLM 投影（AgentMessage → LlmMessage）
    let llm_messages = lim_core::message::convert_to_llm(&messages);

    // 3. 构造请求
    let request = LlmRequest {
        model: model.to_string(),
        messages: llm_messages,
        tools: vec![],           // L1 不用工具
        system: None,            // 已经放在 messages 里
        system_prompt_layout: None,
        max_tokens: None,
        temperature: Some(0.8),
    };

    // 4. 流式调用
    let mut stream = provider.stream(request);
    let mut buffer = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta { text } => {
                buffer.push_str(&text);
                // GUI 这里可以实时渲染：tx.send(text) 到 UI 线程
                println!("{text}");
            }
            StreamEvent::Usage(usage) => {
                eprintln!("[token usage] input={} output={}", usage.input_tokens, usage.output_tokens);
            }
            StreamEvent::Done => break,
            _ => {}
        }
    }
    Ok(buffer)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = /* 加载配置 */;
    let provider = create_provider(&config, "anthropic")?;
    let text = generate_opening(provider, "claude-sonnet-4-20250514", "写一段太空歌剧的开篇").await?;
    println!("---\n{text}");
    Ok(())
}
```

### L1 演进点

| 需求 | 怎么做 |
|---|---|
| 切换模型/provider | 同一个 `Provider` 实例传不同 `model`，或换 `create_provider(&config, "openai")` |
| 多轮对话 | 维护 `Vec<AgentMessage>`，每次 append 用户消息和 assistant 响应，再 `convert_to_llm` |
| Prompt cache | 用 `LlmRequest::system_prompt_layout`（Anthropic）或保证 system 前缀字节稳定（OpenAI） |
| 错误重试 | lim-llm 的 `ProviderError` 已分类（RateLimit/Transport/Server），可按类型退避重试 |

**L1 不适合的场景**：需要 LLM 调用工具（读文件、查数据库、调用你的业务 API）——升级到 L2。

---

## L2：Agent + 工具

### 场景

小说编写工具的"AI 助手"：agent 能调用程序内置工具——拆解书籍结构、绘制人物关系图谱、改编续写、查询素材库等。

### 用到的能力

- `lim_agent::AgentLoop` — 核心循环（消息→LLM→工具→消息，自动 multi-turn）
- `lim_agent::Tool` trait — 自定义工具
- `lim_agent::ToolRegistry` — 工具注册
- `lim_agent::AgentHooks` — 策略扩展（审批、停止条件、路径校验）
- `lim_executor::LocalExecutor` — 执行能力（如果你的工具需要跑命令/读写文件）
- `lim_core::event::AgentEvent` — 事件流（GUI 订阅渲染）
- `tokio::broadcast` — 事件分发

### 关键概念：自定义 Tool

L2 的核心是**实现 `Tool` trait** 把你的业务能力暴露给 LLM。每个工具有：name + schema（给 LLM 看的 JSON Schema）+ execute（实际执行）。

```rust
use async_trait::async_trait;
use lim_agent::tool::{Tool, ToolCategory};
use lim_core::tool::{ToolSchema, ToolResult};
use lim_core::ToolCallId;
use tokio_util::sync::CancellationToken;
use tokio_util::future::ToolFuture;

/// 拆解书籍结构工具——把一本书的章节拆成场景单元
pub struct AnalyzeBookTool {
    db: Arc<NovelDatabase>,  // 你的业务依赖
}

impl Tool for AnalyzeBookTool {
    fn name(&self) -> &str { "analyze_book" }

    fn schema(&self) -> ToolSchema {
        // 这个 schema 会发给 LLM，决定它什么时候调、怎么调
        ToolSchema {
            name: "analyze_book".into(),
            description: "拆解指定书籍的章节结构，返回场景单元列表。用于分析叙事节奏。".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "book_id": {
                        "type": "string",
                        "description": "要分析的书籍 ID"
                    },
                    "focus": {
                        "type": "string",
                        "enum": ["plot", "character", "pacing"],
                        "description": "分析焦点"
                    }
                },
                "required": ["book_id"]
            }),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadOnly  // 不修改状态，无需审批
    }

    fn execute(
        &self,
        _tool_call_id: ToolCallId,
        args: serde_json::Value,
        _cancel: CancellationToken,
    ) -> ToolFuture<lim_core::error::Result<ToolResult>> {
        let db = self.db.clone();
        Box::pin(async move {
            let book_id = args["book_id"].as_str()
                .ok_or_else(|| anyhow::anyhow!("missing book_id"))?;
            let focus = args["focus"].as_str().unwrap_or("plot");

            // 调用你的业务逻辑
            let scenes = db.analyze_book(book_id, focus).await?;

            // 返回给 LLM 的结果（LLM 会基于此继续对话）
            Ok(ToolResult::success(serde_json::to_string(&scenes)?))
        })
    }
}
```

### 完整 L2 示例

```rust
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use lim_agent::{AgentLoop, AgentConfig, registry::ToolRegistry};
use lim_core::event::{AgentEvent, EventEnvelope};

async fn run_novelist_assistant(
    provider: Arc<dyn Provider>,
    db: Arc<NovelDatabase>,
    user_message: &str,
) -> anyhow::Result<String> {
    // 1. 注册工具
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(AnalyzeBookTool { db: db.clone() }));
    registry.register(Arc::new(DrawCharacterGraphTool { db: db.clone() }));
    registry.register(Arc::new(AdaptWritingStyleTool { db: db.clone() }));

    // 2. 事件通道（broadcast）——GUI 订阅这个就能实时渲染
    let (event_tx, mut event_rx) = broadcast::channel::<EventEnvelope>(256);

    // 3. 取消令牌
    let cancel = CancellationToken::new();

    // 4. 构造 AgentLoop
    let agent = AgentLoop::new(provider, Arc::new(registry), event_tx, cancel)
        .with_config(AgentConfig {
            max_turns: 20,
            system_prompt: Some("你是一位小说创作助手，可以调用工具分析书籍、绘制图谱、改写风格。".into()),
            model: "claude-sonnet-4-20250514".into(),
        });

    // 5. 后台 task 消费事件（GUI 这里改成 send 到 UI 线程）
    let ui_handle = tokio::spawn(async move {
        let mut assistant_text = String::new();
        while let Ok(envelope) = event_rx.recv().await {
            match &envelope.event {
                AgentEvent::MessageUpdate { delta, .. } => {
                    // 流式增量——GUI 实时追加
                    print!("{delta}");
                    assistant_text.push_str(delta);
                }
                AgentEvent::ToolExecutionStart { tool_name, .. } => {
                    eprintln!("\n[工具调用] {tool_name}");
                }
                AgentEvent::ToolExecutionEnd { result, .. } => {
                    eprintln!("[工具结果] {}", result.output.chars().take(100).collect::<String>());
                }
                AgentEvent::TurnEnd { stop_reason, usage, .. } => {
                    eprintln!("\n[turn 结束] {stop_reason:?} tokens={}", usage.input_tokens + usage.output_tokens);
                }
                AgentEvent::AgentEnd => break,
                _ => {}
            }
        }
        assistant_text
    });

    // 6. 跑一轮
    let messages = vec![lim_core::message::AgentMessage::user(user_message)];
    agent.run(messages).await?;

    // 7. 等 UI task 结束
    let final_text = ui_handle.await?;
    Ok(final_text)
}
```

### L2 演进点

| 需求 | 怎么做 |
|---|---|
| **工具审批**（写操作要用户确认） | 实现 `AgentHooks::before_tool_call`，返回 `BeforeToolDecision::Reject` 或 `Proceed`；GUI 弹审批框 |
| **路径/命令校验** | 实现 `AgentHooks::check_path` / `check_command`，或用 `lim_executor::DefaultPolicy` |
| **长对话压缩** | 构造 `ContextWindowManager`，`.with_context_window(cwm)` 注入；超预算自动 LLM 摘要 |
| **会话持久化** | spawn 后台 task 消费 `event_rx` 写 JSONL（用 `lim_session::SessionWriter`），支持 resume |
| **运行时干预**（用户中途插话） | `.with_steering_channel(rx)` 注入 mpsc，turn 边界自动 drain 并入消息流 |
| **上下文编辑**（用户手动改历史） | `AgentLoop::apply_user_edit(ContextEdit)`，投影层生效但原始历史不变 |
| **用便宜模型做摘要** | `SummarizeStrategy::default().with_model("gpt-4o-mini")`，注入到 `ContextWindowManager` |
| **复用内置工具**（read/write/shell） | `lim_tools::register_builtins(&mut registry, executor)`，注册 read/write/edit/shell |
| **多角色同一进程** | 每个 role 一个 `AgentLoop` 实例 + `ToolRegistry::filtered_clone(&whitelist)` 限定工具 |

### L2 不适合的场景

- 需要**多个 agent 同时跑**并互相通信（如编排者派任务给写手） → L3
- 需要**固定流水线**（写手→编辑→审核 三段） → L3 的 `lim-workflow`
- 需要**跨 session 追目标**（写到直到审核通过） → L3 的 `loop_runner`

---

## L3：多 agent 编排

L3 有三种编排模式，按"动态程度"递增：

```
┌──────────────────┬──────────────────┬──────────────────┐
│  L3a: 静态 DAG   │  L3b: 多 agent   │  L3c: Loop       │
│  (lim-workflow)  │  (multiagent)    │  Engineering     │
│                  │                  │  (loop_runner)   │
├──────────────────┼──────────────────┼──────────────────┤
│  固定节点 + 边   │  动态派发任务    │  目标驱动循环    │
│  并发分层执行    │  agent 间通信    │  verify → 重试   │
│  适合"流水线"    │  适合"团队协作"  │  适合"完成目标"  │
└──────────────────┴──────────────────┴──────────────────┘
```

### L3a：静态 DAG 工作流（`lim-workflow`）

**场景**：固定的"章节生产流水线"——灵感收集 → 写手起草 → 编辑润色 → 审核打分。每个节点是一个 LLM 调用或工具调用，并发执行无依赖的节点。

**适用**：流程明确、不需要 agent 自主决策、不需要重试直到达成目标。

```rust
use lim_workflow::{WorkflowGraph, Node, NodeKind, WorkflowExecutor, WorkflowContext};
use lim_core::NodeId;

// 1. 构造 DAG
let mut graph = WorkflowGraph::new();

let inspiration = graph.add_node(Node {
    id: NodeId::new(),
    kind: NodeKind::Llm {
        prompt: "基于以下主题生成 3 个章节灵感：{topic}".into(),
        model: "claude-sonnet-4-20250514".into(),
    },
    deps: vec![],
});

let writer = graph.add_node(Node {
    id: NodeId::new(),
    kind: NodeKind::Llm {
        prompt: "基于灵感 {inspiration.output} 写 2000 字章节草稿".into(),
        model: "claude-sonnet-4-20250514".into(),
    },
    deps: vec![inspiration],
});

let editor = graph.add_node(Node {
    id: NodeId::new(),
    kind: NodeKind::Llm {
        prompt: "润色章节草稿：{writer.output}".into(),
        model: "claude-sonnet-4-20250514".into(),
    },
    deps: vec![writer],
});

let reviewer = graph.add_node(Node {
    id: NodeId::new(),
    kind: NodeKind::Llm {
        prompt: "审核章节质量并打分（1-10）：{editor.output}".into(),
        model: "gpt-4o".into(),  // 用不同模型做独立评审
    },
    deps: vec![editor],
});

// 2. 执行（自动分层并发：inspiration → writer → editor → reviewer）
let mut ctx = WorkflowContext::new();
ctx.set("topic", "星际旅行中的人性");

let executor = WorkflowExecutor::new(provider);
let result = executor.run(graph, ctx).await?;

// 3. 取结果
let chapter = result.get_string(&editor)?;
let score = result.get_string(&reviewer)?;
```

**特点**：
- 节点是声明式的，不持有状态
- 同层节点并发，跨层等待
- 适合"一次跑完出结果"，不支持"跑失败重试到通过"

### L3b：多 agent 拓扑（`lim-agent/multiagent`）

**场景**：动态团队协作——编排者 agent 接收用户请求，根据任务派发给写手/灵感收集/市场反馈等子 agent，子 agent 之间可以通信，编排者汇总结果。

**适用**：需要 agent 自主决策任务分配、agent 间有通信需求、任务结构动态变化。

```rust
use lim_agent::multiagent::{
    Scheduler, SpawnOptions, LocalTopology, into_topology_arc,
};
use lim_core::id::AgentId;

// 1. 构造拓扑（本地拓扑——所有 agent 同进程，channel 通信）
let topology = into_topology_arc(LocalTopology::new());

// 2. 构造调度器
let scheduler = Scheduler::new();

// 3. Spawn 编排者 agent
let orchestrator_handle = topology.clone();
scheduler.spawn(SpawnOptions {
    id: AgentId::new(),
    run: Box::new(move |cancel| {
        let topo = orchestrator_handle.clone();
        Box::pin(async move {
            // 编排者循环：接收用户请求 → 拆任务 → 派发 → 汇总
            let agent = build_orchestrator_agent(topo.clone()).await;
            agent.run_until_cancelled(cancel).await
        })
    }),
}).await?;

// 4. Spawn 写手 agent（多个，不同风格）
for style in &["科幻", "悬疑", "言情"] {
    let topo = topology.clone();
    scheduler.spawn(SpawnOptions {
        id: AgentId::new(),
        run: Box::new(move |cancel| {
            let style = style.to_string();
            let topo = topo.clone();
            Box::pin(async move {
                let agent = build_writer_agent(&style, topo).await;
                agent.run_until_cancelled(cancel).await
            })
        }),
    }).await?;
}

// 5. Spawn 编辑/审核/灵感/市场反馈 agent
// ...

// 6. agent 间通过 transport 通信（LocalChannelTransport）
//    编排者 → 写手：派发写作任务
//    写手 → 编辑：提交草稿
//    编辑 → 审核员：提交润色稿
//    市场反馈 → 编排者：反馈读者偏好
```

**特点**：
- 每个 agent 是独立 task，有自己的 `AgentLoop` + `ToolRegistry`
- agent 间通过 `AgentTransport` 通信（本地用 channel，未来可扩展网络）
- 支持 `Worktree` 隔离（多 agent 在不同 git worktree 并行写代码不冲突）
- 拓扑可插拔：`LocalTopology` / `CentralizedTopology`（中心化调度）/ `NetworkTopology`（跨机）

### L3c：Loop Engineering（`lim-agent/loop_runner`）

**场景**：长程目标驱动——"写完一本 10 章小说，每章都通过审核"。单次 session 写不完，需要跨多次 session 迭代，每次用 Verifier 验证是否达成目标，失败重试，有预算控制。

**适用**：目标明确但路径不确定、需要自动重试、需要预算控制、需要持久化状态可恢复。

```rust
use lim_agent::loop_runner::{
    LoopRunner, AgentFactory, Budget, AlwaysPassVerifier,
    CommandVerifier, ReviewerVerifier, NoProgressDetector,
    StateLedger, GoalRecipe,
};
use lim_core::id::GoalId;

// 1. 构造 GoalRecipe（目标配方）
let recipe = GoalRecipe {
    goal_id: GoalId::new(),
    goal_text: "完成小说《星际迷航：人性》第 3 章，包含 3 个场景，每个场景 ≥800 字，通过审核".into(),
    max_sessions: Some(5),       // 最多重试 5 次
    max_tokens: Some(200_000),   // 总 token 预算
    min_interval_sec: Some(2),   // session 间最小间隔（退避）
};

// 2. 构造 AgentFactory（每个 session 用它造 agent）
let factory = AgentFactory::new(move |model| {
    let registry = build_novelist_registry();
    AgentLoop::new(provider.clone(), Arc::new(registry), event_tx.clone(), cancel.clone())
        .with_config(AgentConfig {
            max_turns: 30,
            system_prompt: Some(novelist_system_prompt().into()),
            model,
        })
});

// 3. 选 Verifier
// 方式 A：CommandVerifier（跑外部命令判断）
let verifier = CommandVerifier::new("python check_chapter.py chapter3.md");

// 方式 B：ReviewerVerifier（独立 reviewer agent 评审，有工具）
let verifier = ReviewerVerifier::new(
    reviewer_provider,
    "claude-sonnet-4-20250514".into(),
    "你是小说编辑，审核章节质量。通过标准：结构完整、人物立体、节奏得当。".into(),
    reviewer_registry,  // reviewer 可用 read 工具检查文件
);

// 方式 C：LlmVerifier（LLM-as-judge，无工具）
let verifier = LlmVerifier::new(provider, model, "审核章节是否达成目标".into());

// 方式 D：AlwaysPassVerifier（one-shot，不重试）
let verifier = AlwaysPassVerifier;

// 4. 构造 LoopRunner
let ledger = StateLedger::new(".lim/loop/".into());  // 状态持久化目录
let runner = LoopRunner::new(factory, verifier, ledger)
    .with_no_progress_detector(NoProgressDetector::new(3));  // 连续 3 次无进展 → NeedsHuman

// 5. 跑 goal loop
let result = runner.run_goal_loop(recipe).await?;

match result.status {
    GoalStatus::Completed => println!("章节完成！"),
    GoalStatus::NeedsHuman => println!("需要人工介入：{}", result.reason),
    GoalStatus::BudgetExhausted => println!("预算耗尽"),
    GoalStatus::Error(e) => eprintln!("错误：{e}"),
}
```

**特点**：
- 状态持久化到 `.lim/loop/<goal_id>/`，中断后可 `--resume`
- 每个 session 的事件流独立持久化（`session-N.jsonl`）
- Verifier 可组合：`AllPassVerifier` 串联多个，全部通过才算 Pass
- Controller 决定失败后下一 session 怎么走（`FixClippyController` / `WriteTestsController` / 自定义）
- Backoff 策略避免频繁重试
- 支持 daemon 模式跨 workspace 管理多个 goal

### L3 三种模式对比

| 维度 | L3a 静态 DAG | L3b 多 agent 拓扑 | L3c Loop Engineering |
|---|---|---|---|
| **任务结构** | 固定流水线 | 动态派发 | 目标驱动循环 |
| **agent 数** | 节点数 | 任意 | 每 session 1 个（+ verifier agent） |
| **agent 通信** | 通过 context 传值 | transport 消息 | 不直接通信（通过 goal state） |
| **重试** | 不支持 | 不直接支持 | ✅ verify 失败自动重试 |
| **预算控制** | 无 | 无 | ✅ max_sessions / max_tokens |
| **持久化** | in-memory | in-memory | ✅ 状态 + 事件 JSONL |
| **可恢复** | 否 | 否 | ✅ `--resume <goal_id>` |
| **适用场景** | 流水线生产 | 团队协作 | 长程目标达成 |
| **小说案例** | 章节→润色→审核 一次过 | 编排者派写手+编辑协作 | 写到审核通过，最多 5 轮 |

### L3 混合模式

实际产品中可以混合用：

```rust
// 1. 用 Loop Engineering 驱动整本书的进度（每章一个 goal）
// 2. 每个 goal 内部用静态 DAG 跑"灵感→起草→润色"流水线
// 3. 失败的章节用多 agent 拓扑让编排者+多个写手协作重写
```

---

## 选型决策树

```
你的需求是什么？
│
├─ 只调 LLM 生成文本，无工具
│  └─► L1 (lim-llm)
│
├─ 需要 LLM 调用工具完成任务
│  ├─ 单 agent 够用
│  │  └─► L2 (lim-agent + Tool trait)
│  │
│  └─ 多 agent 协作
│     │
│     ├─ 流程固定，一次跑完
│     │  └─► L3a (lim-workflow)
│     │
│     ├─ 动态派发，agent 间通信
│     │  └─► L3b (multiagent)
│     │
│     └─ 目标驱动，需要重试 + 预算 + 持久化
│        └─► L3c (loop_runner)
│
└─ 我只要消息类型，自己实现 agent
   └─► L0 (lim-core)
```

---

## 集成注意事项

### 1. 事件流消费

lim 的所有运行时行为通过 `broadcast::Sender<EventEnvelope>` 发出。GUI 集成时：

```rust
let (event_tx, mut event_rx) = broadcast::channel(256);

// UI 线程订阅
tokio::spawn(async move {
    while let Ok(envelope) = event_rx.recv().await {
        // 转发到 GUI 的事件循环（如 iced/tauri/egui 的 message channel）
        gui_tx.send(envelope).await.unwrap();
    }
});
```

**关键事件**（详见 [execution-flow.md §2.3](architecture/execution-flow.md)）：
- `MessageUpdate` — 流式增量，GUI 实时追加
- `ToolExecutionStart/End` — 工具调用进度
- `TurnEnd` — 一轮结束（含 token usage）
- `ContextCompacted` — 触发压缩（含成本）
- `Error` — 可恢复错误（含重试次数）

### 2. 取消与超时

`CancellationToken` 是 lim 的统一取消机制。GUI 应该：
- 启动时创建 `cancel`
- "停止"按钮调用 `cancel.cancel()`
- 长任务工具在 execute 内检查 `_cancel` 并提前返回

### 3. 错误处理

lim 的错误分两层：
- `LimError` — 业务错误（配置错、IO 错、参数错）
- `ProviderError` — LLM 调用错误（RateLimit / Transport / Server / Auth / ContextLengthExceeded）

RateLimit/Transport/Server 是可重试的，`AgentLoop` 内置 MAX_RETRIES=3 + 指数退避。Auth/InvalidRequest 不重试，直接上报。

### 4. 平台依赖

- `lim-executor` 的 shell 工具在 Unix 用 POSIX sh，Windows 用 Git Bash/WSL/PowerShell
- `LocalExecutor::workspace(roots)` 限制写入 workspace 内（默认 trust level）
- 跨 workspace 操作需要 `--trust permissive`（GUI 集成时慎用）

### 5. 性能特征

| 操作 | 典型延迟 | 备注 |
|---|---|---|
| 单 turn（无工具） | 1-5s | LLM 响应时间主导 |
| 单 turn（含工具） | 2-10s | + 工具执行 |
| Compaction | 3-15s | 隐藏 LLM 调用 |
| Session resume | <100ms | 读 JSONL + replay |
| Provider 切换 | <10ms | 仅替换 Arc |

### 6. 持久化与隐私

- `SessionWriter` 写 `sessions/<id>.jsonl`，包含**完整对话内容**
- 敏感数据（API key）走环境变量，不进 JSONL
- system prompt 只记长度不记内容（`TurnStart.system_prompt_len`）
- 如需 GDPR 合规，需自行删除 session 文件

### 7. 客户端身份声明与代理供应商兼容

#### 背景

部分 API 供应商（尤其是国内中转、聚合服务）采用**客户端白名单风控**：只适配 Claude Code / Cursor / Codex 等知名工具，检查请求特征（User-Agent / 特定 header）来识别"已知客户端"，不在清单的请求会被拒绝或降级。

这让 lim 这类开源框架处于劣势——不是用户要做坏事，是供应商没适配，导致用户没法用自己的 key 调自己买的 API。lim 的应对策略是：

1. **默认诚实标识**：所有 provider 默认发 `User-Agent: lim/<version>`，让供应商能识别 lim 的存在。lim 用量上来后，供应商有动力主动适配。
2. **用户可配置覆盖**：通过 `extra_headers` 字段，用户可以声明为任何客户端身份。这是用户在自己机器上用自己的 key 调自己的 API，lim 只提供配置能力，不对用户的选择做价值判断。

#### 配置方式

在 `.lim/config.toml` 的 provider 配置段加 `extra_headers`：

```toml
[[providers]]
name = "anthropic-via-proxy"
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://some-proxy.com"

# 声明客户端身份（覆盖默认的 lim/<version>）
[providers.extra_headers]
User-Agent = "claude-code/1.0.0"
```

所有三个 provider 类型（`anthropic` / `openai-chat` / `openai-responses`）都支持。`extra_headers` 是 merge 语义——用户提供的 header 覆盖默认值，其他默认 header 保留。

#### 知名工具客户端标识参考

以下是部分知名工具的客户端标识，**仅供排查兼容性参考**。是否使用由用户自行决定，lim 不内置预设、不鼓励伪装。

| 工具 | 关键 header | 用途 |
|---|---|---|
| Claude Code | `User-Agent: claude-code/<version>` | Anthropic 官方 CLI |
| Codex CLI | `User-Agent: codex_cli_rs/<version>` | OpenAI 官方 CLI |
| Cursor | `User-Agent: cursor/<version>` | Cursor IDE |
| OpenRouter | `HTTP-Referer: https://your-app.com` + `X-Title: Your App Name` | OpenRouter 聚合服务**要求**这两个 header |
| Cline | `User-Agent: cline/<version>` | VSCode 插件 |

#### OpenRouter 特别说明

OpenRouter 是个特例——它不限制客户端，但**要求** `HTTP-Referer` 和 `X-Title` 用于仪表板统计。配置示例：

```toml
[[providers]]
name = "openrouter"
type = "openai-chat"
base_url = "https://openrouter.ai/api"
api_key_env = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-sonnet-4-20250514"

[providers.extra_headers]
HTTP-Referer = "https://my-novel-app.com"
X-Title = "My Novel App"
```

#### 编程式覆盖

如果你在代码内构造 provider（不走配置文件），同样可以覆盖：

```rust
use std::collections::HashMap;
use lim_llm::providers::AnthropicProvider;

let mut headers = HashMap::new();
headers.insert("User-Agent".to_string(), "my-app/1.0".to_string());

let provider = AnthropicProvider::new(api_key)
    .with_base_url("https://some-proxy.com")
    .with_extra_headers(headers);
```

#### 合规边界

- lim **不内置**"伪装成 Claude Code"的快捷选项
- lim **不鼓励**伪装成其他工具来绕过供应商的客户端限制
- lim **支持**用户自行声明客户端身份，这是用户对自己 API 调用的合法控制权
- 如果你开发的商业产品基于 lim，建议在产品文档中**如实声明你的客户端身份**，让供应商能识别你

---

## 案例：小说编写工具的完整集成路线

### Phase 1：范本生成器（L1）

```
用户选模型 → 输入主题 → 调 LLM → 流式显示
```

依赖：`lim-core` + `lim-llm`

### Phase 2：AI 助手（L2）

```
用户对话 → agent 调工具（拆书/绘图/改编）→ 多轮交互
```

新增：
- `AnalyzeBookTool` / `DrawCharacterGraphTool` / `AdaptWritingStyleTool`（自定义 Tool）
- `AgentHooks`（写操作审批弹窗）
- `ContextWindowManager`（长对话压缩）
- `SessionWriter`（会话持久化 + resume）

### Phase 3：章节流水线（L3a）

```
灵感 → 写手 → 编辑 → 审核 一次性产出
```

新增：`lim-workflow`，构造 4 节点 DAG，并发执行无依赖节点。

### Phase 4：协作团队（L3b）

```
编排者 ←→ 写手/编辑/灵感/市场反馈 动态协作
```

新增：`multiagent`，多个 agent 各司其职，通过 transport 通信。

### Phase 5：自动完本（L3c）

```
目标"完成 10 章小说" → 跨 session 迭代 → 每章 verify → 失败重试
```

新增：`loop_runner`，每个章节一个 goal，Verifier 用 `ReviewerVerifier`，预算控制 max_sessions=5。

---

## L4：serve 模式（对外暴露 agent 能力）

当你的应用需要把 lim 作为**后台服务**暴露给其他程序调用时，使用 `lim serve` 统一入口。这适合：

- **编辑器集成**：VS Code / Zed 通过 MCP 协议调用 lim 的工具
- **多 agent 协作**：其他 agent 通过 A2A 协议向 lim 提交任务
- **长期运行的 goal daemon**：后台持续处理 goal 提交

### 用到的 crate

| crate | 用途 |
|---|---|
| `lim-serve` | ServeRunner + 协议 handler 适配器 |
| `lim-transport` | MCP/A2A 协议实现 |
| `lim-agent` | AgentFactory（被 handler 包装） |

### 最小集成：启动 serve

```rust
use lim_serve::{ServeOptions, ServeRunner, RunMode, ProtocolSet};
use lim_serve::{ToolRegistryMcpHandler, AgentFactoryA2AHandler};

// 1. 构建 AgentFactory（同 L2/L3）
let factory = Arc::new(build_agent_factory(/* ... */)?);
let registry = factory.registry(); // 假设有 accessor

// 2. 包装为协议 handler
let mcp_handler = Arc::new(ToolRegistryMcpHandler::new(Arc::clone(&registry)));
let a2a_handler = Arc::new(AgentFactoryA2AHandler::new(factory, default_card()));

// 3. 启动 serve（foreground，all 协议 + goal daemon）
let options = ServeOptions {
    protocol: ProtocolSet::all(),
    mode: RunMode::Foreground,
    // ...
};
ServeRunner::new(options)
    .with_mcp_handler(mcp_handler)
    .with_a2a_handler(a2a_handler)
    .run()
    .await?;
```

### 配置驱动：从 `[serve]` 段读取默认值

```toml
# .lim/config.toml
[serve]
protocol = "all"        # 默认协议（CLI --protocol 可覆盖）
mode = "foreground"     # 默认模式（CLI --daemon 可覆盖）

[serve.goal_daemon]
enabled = true          # goal daemon 默认开启
storage = ".lim/daemon" # 存储路径
```

CLI 层（`lim serve`）已内置 config 加载逻辑：CLI flag > `[serve]` 配置 > 内置默认。

### 嵌入到自定义应用

如果你的应用不是 CLI 而是长期运行的服务（如 Web 后端），可以直接嵌入 ServeRunner：

```rust
// 你的 Web 服务启动时，同时 spawn lim serve
tokio::spawn(async {
    let runner = ServeRunner::new(options)
        .with_mcp_handler(mcp_handler)
        .with_a2a_handler(a2a_handler);
    if let Err(e) = runner.run().await {
        tracing::error!(error = %e, "lim serve exited with error");
    }
});

// 同时启动你的 Web 服务...
```

### 集成注意事项

1. **Handler 是 Arc 共享的**：`ToolRegistryMcpHandler` 和 `AgentFactoryA2AHandler` 包装 `Arc`，多个协议 server 共享同一份 agent runtime
2. **A2A 任务是内存态**：`AgentFactoryA2AHandler` 用 `Mutex<HashMap<String, A2ATask>>` 存储任务，进程重启后丢失（v0.1 限制）
3. **goal daemon 独立存储**：`.lim/daemon/` 目录，与 `sessions/` 分离，遵循 containment 原则
4. **shutdown 信号**：foreground 模式自动监听 Ctrl+C + SIGTERM，嵌入时可用 `shutdown_token()` 外部触发

---

## 相关文档

- [architecture/modules.md](architecture/modules.md) — 每个 crate 的子模块清单（API 参考）
- [architecture/execution-flow.md](architecture/execution-flow.md) — 运行时链路（调试参考）
- [architecture/overview.md](architecture/overview.md) — 分层架构与设计原则
- [functional-testing.md](functional-testing.md) — 功能测试用例（验证集成正确性）
