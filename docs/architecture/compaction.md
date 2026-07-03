# 上下文压缩（Compaction）设计

lim 的 compaction 子系统负责在对话历史接近模型上下文窗口上限时，将其压缩到可接受的范围内。本文档记录设计原理、当前实现、未实现的策略，以及迭代检查清单。

> **本文档与 [behavior-trace.md](behavior-trace.md) 互补**：behavior-trace 关注"行为可观测"，本文档关注"压缩策略本身"。

---

## 1. 设计原则

### C1. 原始消息不可变（History is Sacred）

`AgentMessage` 历史一旦产生，**永不修改**。压缩只发生在 `LlmMessage` 投影层——即每次调用 provider 前，把 `Vec<AgentMessage>` 投影成 `Vec<LlmMessage>`。这保证了：

- session JSONL 的回放永远是完整的
- 用户编辑（`ContextEdit`）作用于投影层，不破坏原始历史
- 压缩前后的 token 数、消息数、摘要模型等都在 `ContextCompacted` event 中留痕

### C2. 触发与策略分离（Trigger ≠ Strategy）

`ContextWindowManager` 拥有**触发逻辑**（"什么时候压缩"），压缩**策略**（"怎么压缩"）委托给 `CompactionStrategy` trait。这样：

- 触发条件可以独立于策略演进（例如从 `trigger_ratio` 切换到 `compact_buffer_tokens`）
- 策略可以独立替换（默认 `SummarizeStrategy`，可注入远程压缩器、截断策略等）
- 测试时可以注入 mock 策略，不依赖真实 LLM 调用

### C3. 真实 token 优先（Real Tokens > Heuristic）

触发判断使用 provider 返回的 `usage.input_tokens`（通过 `observe_usage()` 注入），而非字符数估算。这避免了：

- CJK 文本被高估（4 字符 ≈ 1 token 的假设对中文偏高）
- 代码块、JSON 等结构化文本估算偏差大
- 不同 tokenizer（Claude vs GPT-4o）的差异

只有在第一轮（provider 还没返回过 usage）时，才回退到 `estimate_tokens()` 字符估算。

### C4. 缓冲区而非比例（Buffer, Not Ratio）

阈值公式：`context_window - max_output_tokens - compact_buffer_tokens`

默认 `compact_buffer_tokens = 13_000`，对齐 Claude Code 的 `window - 13000`。**不使用百分比**，因为：

- 百分比对小窗口（8K）过于激进（13000 的 13% vs 1M 的 1.3%）
- 缓冲区是绝对值，保证摘要调用本身有足够空间（摘要 prompt 可能 5-10K tokens）
- 缓冲区随模型输出能力线性增长，不随窗口大小波动

---

## 2. 架构

```text
┌──────────────────────────────────────────────────────────┐
│                    AgentLoop::run_inner                   │
│                                                           │
│  ┌─────────────────┐    ┌─────────────────┐               │
│  │  Layer 1: Hooks │ →  │ Layer 2: Edits  │              │
│  │  (transient)    │    │ (persistent)    │              │
│  └────────┬────────┘    └────────┬────────┘              │
│           ↓                      ↓                        │
│  ┌──────────────────────────────────────────┐            │
│  │  Layer 3: ContextWindowManager::compact  │            │
│  │                                           │            │
│  │  1. observe_usage(input_tokens)  ←────── TurnEnd     │
│  │  2. if tokens > threshold:                │           │
│  │       └─ strategy.compact(req) ──┐        │           │
│  │  3. else: convert_to_llm(msgs)   │        │           │
│  │                                   ↓        │           │
│  │  ┌────────────────────────────────────┐   │           │
│  │  │      CompactionStrategy            │   │           │
│  │  │  ┌──────────────────────────────┐  │   │           │
│  │  │  │  SummarizeStrategy (default) │  │   │           │
│  │  │  │  1. find_recent_start()      │  │   │           │
│  │  │  │  2. old msgs → text          │  │   │           │
│  │  │  │  3. hidden LLM call          │  │   │           │
│  │  │  │  4. summary + recent → LlmMsg│  │   │           │
│  │  │  └──────────────────────────────┘  │   │           │
│  │  └────────────────────────────────────┘   │           │
│  └──────────────────────────────────────────┘            │
│                     ↓                                     │
│              provider.stream(LlmRequest)                  │
└──────────────────────────────────────────────────────────┘
```

### 2.1 三层投影管线

每次 `run_inner` 调用 LLM 前，依次应用三层投影：

| 层 | 作用 | 持久性 | 实现位置 |
|---|---|---|---|
| Layer 1 | Hooks 临时编辑（如注入当前时间） | 瞬时（仅本轮） | `AgentHooks::edit_context()` |
| Layer 2 | 用户编辑（exclude / replace） | 持久（每轮重放） | `compose_edits(&self.user_edits)` |
| Layer 3 | 上下文压缩 | 按需（触发时） | `ContextWindowManager::compact()` |

### 2.2 CompactionStrategy trait

```rust
pub trait CompactionStrategy: Send + Sync {
    fn compact<'a>(
        &'a self,
        req: CompactRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CompactionResult>> + Send + 'a>>;
}

pub struct CompactRequest<'a> {
    pub messages: &'a [AgentMessage],
    pub config: &'a ContextConfig,
    pub main_provider: &'a Arc<dyn Provider>,
    pub main_model: &'a str,
    pub observed_input_tokens: Option<u64>,
}
```

策略只负责"被调用时怎么压缩"，**不负责判断是否触发**。触发判断由 `ContextWindowManager` 拥有。

### 2.3 SummarizeStrategy（默认实现）

工作流程：
1. `find_recent_start(messages, keep_recent_pairs)` — 找到需要保留的最近 N 对 user/assistant 消息的起始索引
2. 把 `messages[..recent_start]` 转成纯文本（`message_to_text`）
3. 用 main_provider（或 `with_provider`/`with_model` 覆盖的更便宜模型）调用 LLM 生成摘要
4. 返回 `[summary_message, *recent_messages]` 作为 `LlmMessage` 列表

**扩展点**：
- `SummarizeStrategy::with_provider(provider)` — 用不同的 provider 做摘要（如 OpenAI 摘要 Claude 对话）
- `SummarizeStrategy::with_model(model)` — 用更便宜的模型做摘要（如 `gpt-4o-mini`）

### 2.4 ContextWindowManager

```rust
pub struct ContextWindowManager {
    config: ContextConfig,
    strategy: Box<dyn CompactionStrategy>,
    last_known_input_tokens: Option<u64>,  // observe_usage() 注入
}
```

关键方法：
- `new(config)` — 用默认 `SummarizeStrategy` 构造
- `with_strategy(config, strategy)` — 注入自定义策略
- `observe_usage(input_tokens)` — AgentLoop 在 TurnEnd 后调用，让触发判断用真实 token 数
- `compact(messages, provider, model)` — 触发判断 + 委托给策略

---

## 3. 触发逻辑

### 3.1 阈值计算

```rust
fn auto_compact_threshold(&self) -> u64 {
    self.context_window
        .saturating_sub(self.max_output_tokens)
        .saturating_sub(self.compact_buffer_tokens)
}
```

默认配置（200K 窗口示例）：

| 参数 | 默认值 | 说明 |
|---|---|---|
| `context_window` | 128,000 | 模型上下文窗口大小 |
| `max_output_tokens` | 8,192 | 为模型输出预留的 token |
| `compact_buffer_tokens` | 13,000 | 为摘要调用本身预留的缓冲 |
| `keep_recent_pairs` | 4 | 保留的最近 user/assistant 对数 |

阈值 = 128,000 - 8,192 - 13,000 = **106,808**（约 83.4%）

### 3.2 token 来源优先级

```rust
let tokens_before = self.last_known_input_tokens
    .unwrap_or_else(|| estimate_tokens(messages));
```

1. **优先**：`observe_usage()` 注入的真实 `usage.input_tokens`
2. **回退**：`estimate_tokens()` 字符估算（chars / 4）

### 3.3 手动压缩

TUI `/compact` 命令和 CLI 等价物调用 `AgentLoop::compact_now()`，走相同的 Layer 2 + Layer 3 管线，但**不调用 LLM 做新一轮**——只触发隐藏的摘要调用（如果超阈值）。

---

## 4. 主流 agent 的 compact 做法（调研）

### 4.1 Claude Code

- **阈值**：`window - 13000`（动态，非百分比）
- **两阶段摘要 prompt**：先摘要对话，再摘要摘要（应对超长历史）
- **tool_result 外置**：超长 tool result 单独存储，不进摘要
- **熔断器**：如果摘要本身超时或失败，降级为截断
- **可观测**：每次压缩都通知用户

> lim 的 `compact_buffer_tokens` 默认值 13,000 即对齐此设计。

### 4.2 Cursor

- **DCD（Dev Context Document）**：文件作为上下文原语，不做对话压缩
- **文件级 RAG**：对话历史不压缩，而是把代码库索引到向量库，按需检索
- **不适用于 lim**：lim 是 agent runtime，不是 IDE；对话压缩仍是必要的

### 4.3 Aider

- **repo map**：把代码库结构（函数签名、类定义）作为上下文
- **简单截断**：超长时直接截断早期消息，不做摘要
- **分片编辑**：每次只关注一个文件的编辑，减少上下文压力

### 4.4 OpenAI Assistants API

- **服务端托管**：压缩逻辑在 OpenAI 侧，不透明
- **无控制**：用户无法指定压缩策略或触发时机

### 4.5 lim 的取舍

| 方面 | lim 选择 | 理由 |
|---|---|---|
| 压缩方式 | 摘要（Summarize） | 保留语义连续性，比截断更智能 |
| 阈值方式 | 绝对缓冲（buffer_tokens） | 对齐 Claude Code，跨窗口大小稳定 |
| token 来源 | 真实 usage 优先 | 避免估算偏差 |
| 策略可替换 | trait + 默认实现 | 不锁死实现，未来可接远程压缩器 |
| 摘要模型 | 支持覆盖（with_model） | 可用更便宜模型降低成本 |

---

## 5. 未实现的策略（Future Work）

以下策略已设计扩展点但**尚未实现**。实现时只需实现 `CompactionStrategy` trait，通过 `ContextWindowManager::with_strategy()` 注入即可。

### 5.1 TruncateStrategy（截断策略）

最简单的策略：直接丢弃早期消息，不做摘要。

```rust
pub struct TruncateStrategy {
    keep_recent_pairs: usize,
}

impl CompactionStrategy for TruncateStrategy {
    async fn compact(&self, req: CompactRequest) -> Result<CompactionResult> {
        let start = find_recent_start(req.messages, self.keep_recent_pairs);
        let recent = &req.messages[start..];
        let messages = convert_to_llm(recent);
        // ... 返回 CompactionResult，无隐藏 LLM 调用
    }
}
```

**适用场景**：
- 测试环境（不依赖 LLM）
- 对话历史不重要（如一次性脚本）
- provider 不可用时的降级

### 5.2 MapReduceStrategy（分片摘要策略）

超长历史（如 500K tokens）单次摘要可能超窗口。MapReduce 策略分片摘要后再合并：

1. 把历史分成 N 个 chunk（每个 ≤ 窗口的 1/3）
2. 每个 chunk 单独摘要（可并行）
3. 把 N 个摘要再摘要成最终摘要
4. 最终摘要 + 最近消息 → LlmMessage

**适用场景**：
- 1M 窗口的长对话（如 Claude 3.5 Sonnet 1M）
- 单次摘要超窗口的极端情况

### 5.3 RemoteCompactor（远程压缩服务）

把压缩委托给专门的远程服务：

```rust
pub struct RemoteCompactor {
    endpoint: String,
    api_key: String,
}
```

**适用场景**：
- 自托管压缩服务（如用本地 Ollama 跑摘要）
- 跨 session 的语义检索（如 ChromaDB / Pinecone）
- 企业内部部署（数据不出境）

### 5.4 HybridStrategy（混合策略）

根据上下文长度选择不同策略：

```rust
pub struct HybridStrategy {
    summarize: SummarizeStrategy,
    mapreduce: MapReduceStrategy,
    threshold: u64,  // 超过此值用 MapReduce
}
```

### 5.5 SelectiveKeepStrategy（选择性保留策略）

基于规则而非摘要：
- 保留所有 tool call / tool result
- 保留所有 error 事件
- 只摘要纯文本对话
- 保留 system message

**适用场景**：
- 工具密集型对话（tool result 比 assistant 文本更重要）
- 调试场景（需要完整工具调用链）

---

## 6. 已知问题与改进方向

### 6.1 摘要 prompt 单一风险

当前 `summarize_compact()` 使用单一 prompt 摘要全部旧消息。如果旧消息本身已经超过摘要模型的窗口，摘要调用会失败。

**改进方向**：
- 实现 MapReduceStrategy（§5.2）
- 检测旧消息 token 数，超过摘要模型窗口时分片

### 6.2 摘要失败降级

当前摘要失败时，`SummarizeStrategy` 会 fallback 到保留原始消息（verbatim）。这是安全的，但可能导致下一轮仍然超阈值，触发循环。

**改进方向**：
- 摘要失败后自动降级到 TruncateStrategy
- 在 `ContextCompacted` event 中记录 `fallback_reason`

### 6.3 缺少 token 估算的 tokenizer 支持

`estimate_tokens()` 用 chars/4 估算，对 CJK 文本偏高。理想情况下应使用真实 tokenizer：
- `tiktoken`（OpenAI）
- `@anthropic-ai/tokenizer`（Anthropic）

**改进方向**：
- 在 `lim-llm` 中加 `Provider::count_tokens(text) -> u64` 方法
- `observe_usage()` 优先使用真实值，但首轮仍需估算

### 6.4 跨 session 的上下文持久化

当前每个 session 独立压缩，无法跨 session 复用摘要。如果用户 resume 一个已压缩的 session，压缩状态会丢失。

**改进方向**：
- 在 session JSONL 中持久化 `ContextCompacted` event 的 summary
- resume 时检测最后的 `ContextCompacted`，直接用 summary 作为历史

---

## 7. 迭代检查清单

每次修改 compaction 相关代码时，按此清单自检：

- [ ] **触发逻辑**：`auto_compact_threshold()` 公式是否正确？`compact_buffer_tokens` 默认值是否合理？
- [ ] **token 来源**：`observe_usage()` 是否在每次 TurnEnd 后调用？回退到 `estimate_tokens()` 的路径是否覆盖？
- [ ] **策略隔离**：新策略是否只依赖 `CompactRequest` 的字段？是否泄露了 `ContextWindowManager` 的内部状态？
- [ ] **可观测性**：压缩是否 emit `ContextCompacted` event？是否包含 `summary_model` / `summary_prompt_tokens` / `summary_completion_tokens`？
- [ ] **错误处理**：策略失败时是否 fallback？fallback 路径是否 emit event 或 log？
- [ ] **测试覆盖**：新策略是否有至少 3 个测试（over_budget / under_budget / fallback）？
- [ ] **历史不可变**：压缩是否只修改 `LlmMessage` 投影？`AgentMessage` 历史是否被保留？
- [ ] **hidden LLM call**：新增的隐藏 LLM 调用是否在本文档 §5 或 behavior-trace.md 中登记？

---

## 8. 参考实现

| 组件 | 文件 | 说明 |
|---|---|---|
| `CompactionStrategy` trait | `crates/lim-agent/src/compaction/strategy.rs` | trait 定义 + `CompactRequest` |
| `SummarizeStrategy` | `crates/lim-agent/src/compaction/summarize.rs` | 默认实现 |
| `ContextWindowManager` | `crates/lim-agent/src/context_window.rs` | 触发逻辑 + 策略调度 |
| `ContextConfig` | `crates/lim-agent/src/context_window.rs` | 配置（窗口大小、缓冲区、保留对数） |
| `CompactionResult` | `crates/lim-agent/src/context_window.rs` | 压缩结果（含统计字段） |
| `ContextCompacted` event | `crates/lim-core/src/event.rs` | 可观测事件 |
| `AgentLoop::compact_now()` | `crates/lim-agent/src/agent_loop.rs` | 手动压缩入口 |
| `AgentLoop::observe_usage()` | `crates/lim-agent/src/agent_loop.rs` | TurnEnd 后注入真实 token 数 |
