# lim 完整代码规范

阶段性 review 与新模块设计的完整参考。日常开发遵循 [basics.md](basics.md) 即可。

> 基于 Rust 官方 [API Guidelines](https://rust-lang.github.io/api-guidelines/)、社区最佳实践与 lim 项目特性整理。

---

## 目录

1. [命名规范](#1-命名规范)
2. [错误处理](#2-错误处理)
3. [异步编程](#3-异步编程)
4. [迭代器](#4-迭代器)
5. [泛型与类型约束](#5-泛型与类型约束)
6. [所有权与借用](#6-所有权与借用)
7. [模块与可见性](#7-模块与可见性)
8. [集合选型](#8-集合选型)
9. [抽象与文件拆分](#9-抽象与文件拆分)
10. [日志规范](#10-日志规范)
11. [测试](#11-测试)
12. [文档注释](#12-文档注释)
13. [格式化与 Clippy](#13-格式化与-clippy)
14. [Unsafe Rust](#14-unsafe-rust)

---

## 1. 命名规范

### 通用约定

遵循 Rust 标准风格（[RFC 430](https://github.com/rust-lang/rfcs/blob/master/text/0430-finalizing-naming-conventions.md)）：

| 项 | 风格 | 示例 |
|---|---|---|
| 类型、trait、枚举变体 | `UpperCamelCase` | `LocalExecutor`、`ToolGuard`、`ApprovalMode::Always` |
| 函数、方法、变量、字段 | `snake_case` | `run_command`、`trust_level` |
| 常量、静态变量 | `SCREAMING_SNAKE_CASE` | `MAX_TURNS`、`DEFAULT_TIMEOUT` |
| 生命周期 | 单小写字母优先；语义化时用 `'ctx`、`'buf` | `'a`、`'ctx` |
| 泛型参数 | 短大写优先；语义化时用 `CamelCase` | `T`、`Error`、`Context` |
| 模块 | `snake_case` | `lim_core`、`agent_loop` |

### 补充约定

- **布尔值**：以 `is_` / `has_` / `can_` / `should_` 开头。如 `is_valid`、`has_data`、`requires_approval`。
- **谓词函数**（返回 bool）：同上前缀规则。
- **Getter 不加 `get_` 前缀**（Rust 惯例，与 Java 相反）：

  ```rust
  // 好
  fn name(&self) -> &str { &self.name }
  fn policy(&self) -> &Policy { &self.policy }

  // 坏
  fn get_name(&self) -> &str { ... }
  fn get_policy(&self) -> &Policy { ... }
  ```

- **Setter 加 `set_` 前缀**：`fn set_name(&mut self, name: String)`。
- **构造函数**：默认 `new`；多个构造器用 `with_xxx` / `from_xxx` / `workspace` 等语义化命名。
- **类型转换**：`as_xxx`（借用）、`to_xxx`（克隆）、`into_xxx`（消费）。

### lim 现状检查

- [policy.rs](../../crates/lim-executor/src/policy.rs) 若有 `get_policy()` 类方法应改为 `policy()`
- [id.rs](../../crates/lim-core/src/id.rs) 的 `as_ulid()` / `as_str()` 命名正确 ✅

---

## 2. 错误处理

### 核心原则

1. **错误是值，不是异常**：优先 `Result`，避免 panic
2. **错误类型要具象**：内部逻辑错误枚举化，外部来源错误允许 String
3. **错误可追溯**：保留 source 链（`#[source]`），附上下文
4. **闭合枚举**：禁止 catch-all 变体（如 `Other(anyhow::Error)`）

### 错误类型定义

使用 `thiserror` 派生宏定义错误枚举。变体分两类：

- **纯 String 变体**：外部来源不可枚举的错误（provider 消息、配置错误消息）
- **结构体 + `#[source]` 变体**：有明确底层类型错误的场景（io、serde、reqwest）

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LimError {
    // 纯 String：provider 错误（lim-core 不能依赖 lim-llm 的 ProviderError）
    #[error("provider error: {0}")]
    Provider(String),

    // 结构体 + #[source]：保留底层错误链
    #[error("tool `{name}` failed: {message}")]
    ToolExecution {
        name: String,
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("session I/O error: {context}")]
    SessionIo {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("terminal error: {context}")]
    Terminal {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    // 其他变体...
    #[error("configuration error: {0}")]
    Config(String),
}
```

### 便捷构造器

带 `#[source]` 的变体必须提供便捷构造器，避免调用方手写结构体字面量：

```rust
impl LimError {
    /// 带底层 source 的构造器
    pub fn tool_execution(
        name: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self { ... }

    /// 无 source 的构造器（纯消息）
    pub fn tool_execution_msg(name: impl Into<String>, message: impl Into<String>) -> Self { ... }

    pub fn session_io(context, source) -> Self { ... }
    pub fn session_io_msg(context) -> Self { ... }
    pub fn terminal(context, source) -> Self { ... }
    pub fn terminal_msg(context) -> Self { ... }
}
```

### 调用规则

**有底层错误时**：用带 source 的构造器，message 保留完整信息（含 Display）：

```rust
// 好：message 含 e 的 Display（用户友好），source 保留 e（调试友好）
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution(
        "fixture_loader",
        format!("failed to read fixture `{path}`: {e}"),
        e,
    ))?;
```

**无底层错误时**（纯消息）：用 `_msg` 构造器：

```rust
// 好：工具自身定义的错误消息
return Err(LimError::tool_execution_msg(
    "bash",
    format!("command not allowed: {cmd}"),
));
```

**禁止**：直接用结构体字面量构造带 `#[source]` 的变体（必须用构造器）：

```rust
// 坏：手写字面量，容易漏 source 字段
return Err(LimError::ToolExecution {
    name: "bash".into(),
    message: format!("failed: {e}"),
    source: None,  // ← 丢失了 e
});
```

### lim 特化约定

#### 外部错误允许 String

provider 返回的错误消息、tool 执行的错误输出本质是外部字符串，无法枚举所有变体，**允许使用 String**：

```rust
// 允许：外部来源错误
Provider(String),
Config(String),
```

内部逻辑错误必须枚举化，不要用 `String` 作为错误变体。

#### 禁止 catch-all 变体

`LimError` **不设 `Other(anyhow::Error)` 兜底变体**。所有错误必须归类到具体变体；如需传播外部错误，定义新的具体变体并保留 `#[source]`。

#### crate 公共 API 禁止 `anyhow::Result`

- **binary 顶层 main**：可以用 `anyhow::Result`（应用层错误链拼接）
- **crate 公共 API**：必须用 `lim_core::error::Result<T>`（即 `Result<T, LimError>`）
- **apps/tui 是 lib crate**：虽然位于 apps/ 目录，但被 apps/cli 依赖，公共 API 必须用 `lim_core::Result`

#### 跨 crate 错误转换

当错误跨越 crate 边界时，如果目标 crate 不能依赖源 crate（架构分层），允许 flattening 到 String：

```rust
// lim-core 不能依赖 lim-llm，所以 ProviderError → LimError 时 flattening
impl From<ProviderError> for lim_core::error::LimError {
    fn from(err: ProviderError) -> Self {
        // Display 信息足够人类阅读；需要结构化访问的调用方
        // 应在 ProviderError 跨越边界前处理
        lim_core::error::LimError::Provider(err.to_string())
    }
}
```

如果源 crate 的错误类型可以保留（同层或允许依赖），必须用 `#[source]` 保留：

```rust
// ProviderError::Transport 保留 reqwest::Error 作为 source
#[error("transport error: {context}")]
Transport {
    context: String,
    #[source]
    source: reqwest::Error,  // ← 保留原始错误，不字符串化
},
```

### Result 类型别名

模块级统一导出 `Result` 别名：

```rust
// crates/lim-core/src/error.rs
pub type Result<T> = std::result::Result<T, LimError>;
```

各 crate re-export：

```rust
// crates/lim-agent/src/lib.rs
pub use lim_core::error::{LimError, Result};
```

### `?` 操作符使用

- 优先 `?` 传播，避免嵌套 `match`
- 错误必须带上下文：

```rust
// 好：保留 source + 带上下文
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution("read", format!("failed to read {path}: {e}"), e))?;

// 坏：丢失上下文和 source
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution_msg("read", e.to_string()))?;
```

- 跨模块错误**不要**滥用 `From` 自动转换，避免隐式转换掩盖语义
- 仅在错误类型"自然转换"时用 `#[from]`（如 `From<reqwest::Error> for ProviderError`）

### `#[from]` vs `#[source]`

- **`#[from]`**：错误类型 A 可由 B 自然转换时用，提供自动 `From` 实现
- **`#[source]`**：保留来源但不允许自动转换时用（lim 的主要方式）

```rust
// #[from]：自然转换（reqwest::Error → ProviderError::Transport）
impl From<reqwest::Error> for ProviderError {
    fn from(err: reqwest::Error) -> Self {
        ProviderError::Transport { context: "...".into(), source: err }
    }
}

// #[source]：保留来源但禁止自动 From
#[error("parse failed at line {line}")]
Parse {
    line: usize,
    #[source]
    source: serde_json::Error,
},
```

### Panic 政策

| 场景 | 是否允许 panic | 备注 |
|---|---|---|
| 测试代码 | ✅ | `unwrap` 自由使用 |
| 启动期不可恢复初始化 | ✅ | 必须 `expect("reason")` |
| 逻辑上不可能失败 | ✅ | 必须 `expect("reason")` 并注释不变量 |
| 用户输入错误 | ❌ | 用 `Result` |
| IO 失败 | ❌ | 用 `Result` |
| Provider 返回异常 | ❌ | 用 `Result` |
| 业务逻辑分支 | ❌ | 用 `Result` |

### 错误处理反例

```rust
// 坏：静默吞错
let stream = provider.stream(request);
let events: Vec<_> = collect_stream(stream).await;
for event in events {
    if let Ok(StreamEvent::TextDelta { text }) = event {
        summary.push_str(&text);  // 错误被忽略
    }
}

// 好：检查错误并回退
let stream = provider.stream(request);
let events: Vec<_> = collect_stream(stream).await;
let mut summary = String::new();
let mut had_error = false;
for event in events {
    match event {
        Ok(StreamEvent::TextDelta { text }) => summary.push_str(&text),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "summary provider stream failed");
            had_error = true;
        }
    }
}
if had_error && summary.is_empty() {
    return Ok(None);  // 回退为不压缩
}
```

```rust
// 坏：字符串化底层错误，丢失 source 链
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution_msg("read", format!("failed: {e}")))?;

// 好：保留 source 链
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution("read", format!("failed: {e}"), e))?;
```

---

## 3. 异步编程

### 核心原则

1. **Send 约束优先**：async trait 默认 `+ Send`，trait object 默认 `+ Send + Sync`
2. **锁不跨 await**：持有 Mutex/RwLock 守卫时禁止进入 `.await`
3. **背压与限流**：异步并发必须有边界，禁止无限制 `spawn`
4. **统一 tokio runtime**：不混用 async-std / smol

### 异步函数签名

**优先原生 `async fn`**（Rust 1.75+ 支持 trait 内 async fn）：

```rust
// 好：直接 async fn
pub async fn fetch_data(&self, url: &str) -> Result<Vec<u8>> { ... }
```

**需要 dyn-compatible 时**用 `type Future` 别名（lim 已采用的模式）：

```rust
// lim-agent/src/hooks.rs
pub type HookFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub trait AgentHooks: Send + Sync {
    fn before_tool_call(&self, call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>>;
}
```

### lim 特化：不引入 `async-trait` crate

lim 使用 Rust 1.75+ 原生 async fn in trait + 手动 `Pin<Box<...>>` 别名。**禁止引入 `async-trait`**，理由：
- 原生支持已足够
- async-trait 会带来额外 heap allocation
- 与 lim 现有 `HookFuture<T>` 模式冲突

### Send 约束

所有跨 await 的数据必须 `Send`。trait object 默认 `+ Send + Sync`：

```rust
// 好
pub trait Provider: Send + Sync { ... }
let provider: Arc<dyn Provider> = ...;

// 坏：缺失 Send + Sync
pub trait Provider { ... }  // 无法放入 Arc<dyn>
```

### 锁不跨 await

```rust
// 坏：持锁跨 await
let lists = self.shared_lists.lock().unwrap();
let result = provider.call(...).await;  // 锁仍持有，其他线程阻塞
drop(lists);

// 好：先提取数据，释放锁，再 await
let status = {
    let mut lists = self.shared_lists.lock().unwrap();
    lists.allowlist.insert(name.clone());
    format!("allowed: {}", name)
};
self.set_status(status);  // 锁已释放
```

### 阻塞 IO

CPU 密集型计算、同步 IO 必须 `spawn_blocking`：

```rust
// 好
let content = tokio::task::spawn_blocking(move || std::fs::read(&path)).await??;

// 坏：阻塞 async 线程
let content = std::fs::read(&path)?;
```

### 取消

统一用 `CancellationToken`（已有模式），禁止裸 `oneshot::Sender<()>`：

```rust
// lim-agent/src/agent_loop.rs
pub struct AgentLoop {
    cancel: CancellationToken,
    // ...
}

// 工具执行时透传
let result = tokio::select! {
    _ = self.cancel.cancelled() => Err(LimError::Cancelled),
    r = tool.execute(args, self.cancel.clone()) => r,
};
```

### Runtime

- 统一 tokio，启用必要 feature（`rt-multi-thread`、`macros`、`net`、`io-util`、`time`、`sync`）
- main 入口用 `#[tokio::main]` 或手动 Builder
- 测试用 `#[tokio::test]`，禁止单元测试内手动 spawn runtime

### 并发控制

- 无限制 `spawn` 禁止；用 `Semaphore` 或 `FuturesUnordered` 限并发
- `select!` 中丢弃的 future 会被 drop，需确保资源清理正确

### 超时

所有外部 IO 调用必须有超时：

```rust
let result = tokio::time::timeout(Duration::from_secs(30), provider.stream(req)).await;
match result {
    Ok(Ok(stream)) => { ... }
    Ok(Err(e)) => return Err(LimError::Provider(e.to_string())),
    Err(_) => return Err(LimError::Provider("request timed out".into())),
}
```

---

## 4. 迭代器

### 核心原则

1. **优先迭代器，而非手写循环**：零成本抽象，可读性强
2. **延迟求值**：避免不必要的 `collect`，尽量在链末尾一次性收集
3. **借用优先**：`iter()` 优先，消费所有权用 `into_iter()`

### 迭代器选择

| 方法 | 产出 | 适用 |
|---|---|---|
| `iter()` | `&T` | 只读访问 |
| `iter_mut()` | `&mut T` | 原地修改 |
| `into_iter()` | `T` | 消费所有权 |

### 推荐组合子

- **过滤映射**：优先 `filter_map` 而非 `filter().map()`
- **扁平化**：嵌套结构用 `flat_map`，`Option`/`Result` 用 `flatten`
- **索引迭代**：用 `enumerate()` 而非手动计数器
- **排序去重**：`sort` + `dedup` 优于手动去重；需要稳定排序用 `sort_by`
- **聚合**：`fold` 替代可变累加器

### 允许显式循环的场景

- 需要多次 `break` / `continue` 的复杂控制流
- 累加状态涉及多个变量且迭代器链可读性明显下降
- 性能敏感的热路径（迭代器链无法 inline 优化时，需 benchmark 证明）

```rust
// 允许：复杂控制流
for msg in &messages {
    if msg.is_error {
        if let Some(reason) = extract_error_reason(msg) {
            errors.push(reason);
            if errors.len() >= MAX_ERRORS {
                break;
            }
            continue;
        }
    }
    // ...
}
```

### 性能注意

- 避免在迭代器链中间多次 `collect`
- 预知容量时用 `Vec::with_capacity` + `extend` 替代 `collect`
- `collect` 到 `Vec` 时 `size_hint` 会自动预分配，通常无需手动处理

### 自定义迭代器

- 实现 `Iterator` trait 时必须提供 `next()`，尽量实现 `size_hint()`
- 关联类型 `Item` 明确，不要用 `Box<dyn ...>` 包装元素

---

## 5. 泛型与类型约束

### 核心原则

1. **约束最小化**：只声明实际用到的 trait bound
2. **入参宽松，返回具体**：入参用 `IntoIterator` 等，返回类型尽量具体
3. **静态分发优先，动态分发在边界用**

### 静态 vs 动态分发

**静态分发（泛型 `<T>`）**用于：
- crate 内部 hot path（如 `StreamAccumulator` 工具调用累积）
- 零成本抽象收益明显
- 编译期已知的少量类型集合

**动态分发（`dyn Trait`）**用于：
- crate 公共 API 边界（如 `Arc<dyn Provider>`）
- 用户可注入不同实现（`Arc<dyn ToolGuard>`）
- 类型集合开放（运行时注册）

### lim 特化：crate 边界用 `dyn`

lim 跨 crate 边界优先用 `Arc<dyn Trait>`，支持依赖注入：

```rust
// 好：公共 API 用 dyn，支持运行时注入
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    // ...
}

// 好：内部 hot path 用泛型
pub struct StreamAccumulator<T: Iterator<Item = StreamEvent>> {
    events: T,
}
```

### 约束写法

- 短约束直接写在参数处：`fn foo<T: Clone>(x: T)`
- 多约束或多泛型参数用 `where` 子句：

```rust
// 好：where 子句分离
fn process<K, V>(map: HashMap<K, V>) -> Result<()>
where
    K: Eq + Hash + fmt::Display,
    V: Serialize + Send + 'static,
{
    ...
}
```

### `impl Trait` 使用

- **参数位置**：允许用于单 trait bound 简化签名（`fn foo(it: impl Iterator<Item = u8>)`）
- **返回位置**：用于隐藏具体类型（`fn iter(&self) -> impl Iterator<Item = &T>`）
- **避免**：泛型 + 生命周期 + async 混用（编译时间爆炸）

### 泛型参数数量

- 超过 3 个类型参数考虑拆分（类型参数过多通常是抽象泄漏）
- 关联类型 vs 泛型参数：
  - trait 只有一种实现方式 → 关联类型
  - trait 可以有多种实现方式 → 泛型参数

```rust
// 关联类型：每个实现者只有一种 Output
pub trait Parser {
    type Output;
    fn parse(&self, input: &str) -> Result<Self::Output>;
}

// 泛型参数：可以为多种目标类型实现
impl From<String> for ToolCallId { ... }
```

### 避免预设扩展点

不为了"未来可能需要"提前留 hook，需要时再加（YAGNI）。

---

## 6. 所有权与借用

### 核心原则

1. **借用优先，复制其次，克隆最后**
2. **借用检查器是朋友**：遇到借用错误先重构数据结构，不要急着用 `RefCell`
3. **生命周期标注尽量少**：让编译器推导，必要时再补充

### 参数传递选择

| 场景 | 推荐签名 |
|---|---|
| 只读访问 | `&T` |
| 修改数据 | `&mut T` |
| 函数需要持有所有权 | `T` |
| 可能持有也可能不持有 | `Cow<'_, T>` |
| 字符串入参 | `&str`（调用方自动解引用） |
| 集合入参 | `&[T]` 或 `impl IntoIterator` |
| 集合可能持有 | `impl IntoIterator<Item = T>` |

### 智能指针选型

| 类型 | 用途 |
|---|---|
| `Box<T>` | 堆分配、递归类型、trait 对象 |
| `Rc<T>` | 单线程共享只读数据 |
| `Arc<T>` | 多线程共享只读数据 |
| `RefCell<T>` | 单线程内部可变性 |
| `RwLock<T>` / `Mutex<T>` | 多线程内部可变性 |
| `Cell<T>` | 单线程 Copy 类型内部可变性（性能优于 RefCell） |

lim 常用模式：

```rust
// 共享 Provider
let provider: Arc<dyn Provider> = Arc::new(OpenAiProvider::new(...));

// 共享可变状态（如 ToolLists）
let lists: Arc<Mutex<ToolLists>> = Arc::new(Mutex::new(ToolLists::default()));
```

### 生命周期标注

- 遵循生命周期省略规则，能不写就不写
- 结构体包含引用时必须标注：

```rust
pub struct Ctx<'a> {
    pub config: &'a Config,
    pub runtime: &'a Runtime,
}
```

- 多个引用优先用同一生命周期，确有必要再区分
- 长生命周期持有引用时，考虑改为持有 ID 或索引

### 规避借用失败

- 不要在持有集合引用的同时修改集合
- 避免自引用结构体，必要时用 `Pin` 或 Ouroboros 库
- 持有 `&mut` 时禁止同时有其他借用（即使未使用）

---

## 7. 模块与可见性

### 核心原则

1. **最小可见性**：默认私有，只暴露必要的公开 API
2. **按功能组织模块**，而非按类型组织
3. **`lib.rs` 只做声明和重导出**

### 模块组织

- **不用 `mod.rs`**，用 `foo.rs` + `foo/` 同名文件夹组织：

```text
src/
├── lib.rs          ← 仅 mod 声明 + pub use 重导出
├── config.rs       ← 模块入口（含 pub mod loader; 等）
└── config/
    ├── loader.rs
    └── validator.rs
```

- 每个模块对应一个文件或同名文件夹
- `lib.rs` 只做 `pub mod` + `pub use` 重导出，不放实现

### 重导出

使用 `pub use` 扁平化对外 API，隐藏内部模块结构：

```rust
// crates/lim-core/src/lib.rs
pub mod error;
pub mod id;
pub mod guard;

pub use error::{LimError, Result};
pub use guard::{ToolGuard, TrustLevel};
pub use id::{SessionId, ToolCallId};
```

### prelude 模块

lim 引入 prelude 模式，集中导出最常用类型：

```rust
// crates/lim-core/src/prelude.rs
pub use crate::error::{LimError, Result};
pub use crate::id::{MessageId, SessionId, ToolCallId, TurnId};
pub use crate::message::AgentMessage;
pub use crate::tool::{Tool, ToolCall, ToolResult};
```

**复杂度关注**：
- prelude 只放"几乎所有消费者都会用"的类型
- 超过 20 个导出项说明 prelude 过载，需要重新评估
- 内部 crate 之间不通过 prelude 互引，用具体路径

### 最小可见性

- 默认私有（不加 `pub`）
- 仅暴露外部需要的项：`pub(crate)` 优先于 `pub`
- struct 字段默认私有，提供 accessor 方法而非暴露字段
- enum 变体如果对外公开，整个 enum 必须 `pub`

### 命名空间冲突

- 类型与方法重名时，方法名加语义前缀（如 `Session::id()` 返回 `SessionId`）
- 跨 crate 同名类型用 crate 前缀消歧（`lim_core::Result` vs `std::result::Result`）

---

## 8. 集合选型

### 选型表

| 需求 | 推荐类型 |
|---|---|
| 顺序存储、尾部增删 | `Vec<T>` |
| 双端队列 | `VecDeque<T>` |
| 键值对、快速查找 | `HashMap<K, V>` |
| 有序键值对 | `BTreeMap<K, V>` |
| 去重集合 | `HashSet<T>` |
| 有序集合 | `BTreeSet<T>` |
| 小容量栈上数组 | `[T; N]` |
| 固定容量栈上动态数据 | `arrayvec::ArrayVec` |

### 使用约定

- 预知容量时用 `with_capacity` 构造，减少重新分配
- `HashMap` 自定义键必须同时实现 `Eq` 和 `Hash`
- 优先使用标准库集合，第三方集合（如 `indexmap`）需有明确理由
- **避免 `Vec<Vec<T>>` 嵌套**，优先用扁平向量 + 索引计算

### lim 常用模式

```rust
// 黑白名单：HashSet 提供 O(1) 查找
pub struct ToolLists {
    pub allowlist: HashSet<String>,
    pub denylist: HashSet<String>,
}

// 工具注册表：HashMap 索引
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}
```

### 字符串集合

- 优先 `HashSet<String>` / `HashMap<String, V>`
- 键为字面量时考虑 `&'static str`，但 lim 多数场景需要运行时构造，用 `String`

---

## 9. 抽象与文件拆分

### 核心原则

1. **YAGNI**：不为"未来可能需要"提前抽象
2. **单一职责**：一个模块一个职责
3. **拆分有度**：避免机械拆分导致碎片化

### 抽象时机

- **3 次重复才抽象**：同一段逻辑出现 3 次以上才考虑提取函数/trait
- **2 个实现才 trait**：trait 必须有 2 个真实实现（不算 mock），避免猜测式抽象
- **不预设扩展点**：不为了"未来可能需要"提前留 hook，需要时再加

### Trait 设计

- trait 必须有明确职责（单一职责原则）
- trait 方法数量超过 5 个考虑拆分
- trait 的默认方法仅在"绝大多数实现都用这个逻辑"时提供

```rust
// 好：单一职责，有 2+ 实现
pub trait ToolGuard: Send + Sync {
    fn trust_level(&self) -> TrustLevel;
}
// 实现：PermissiveGuard, WorkspaceGuard, DefaultPolicy

// 坏：猜测式抽象
pub trait FutureExtensibleTool: Tool {
    fn future_extension_hook(&self) { }  // 无实现者
}
```

### 文件长度

#### 统计口径

使用 `tokei` 统计 **Code 列**（不含注释和空行）：

```bash
tokei <path> -t Rust -e target
```

**豁免**：
- `#[cfg(test)] mod tests` 块不计入
- doc-example（`/// ```rust` 代码块）不计入

检查脚本：

```bash
tokei crates apps packs -t Rust -e target --files -o json \
  | jq -r '.Rust.reports[] | select(.stats.code > 800) | "\(.stats.code)\t\(.name)"' \
  | sort -rn
```

#### 分级阈值

| 级别 | 代码行 | 处理策略 |
|---|---|---|
| **舒适** | < 400 | 无需关注 |
| **观察** | 400-600 | review 时检查职责是否单一 |
| **警示** | 600-800 | 新增功能必须拆分到其他文件，不允许继续增长 |
| **红线** | > 800 | 必须拆分，技术债记录 |

#### 函数长度

| 级别 | 代码行 | 处理策略 |
|---|---|---|
| **舒适** | < 30 | 无需关注 |
| **观察** | 30-60 | 检查是否职责过多 |
| **警示** | 60-100 | 必须有注释说明为何不拆 |
| **红线** | > 100 | 必须拆分 |

### 拆分维度

- **按职责拆**：如 `policy.rs` 只管策略，不混 IO
- **按实现拆**：如 `openai_chat.rs` / `openai_responses.rs` / `anthropic.rs`
- **不按大小机械拆**：避免出现 `utils.rs` 这种垃圾抽屉

### 触发拆分的硬条件

不论行数，出现以下情况必须拆分：

1. 单文件有 3 个以上不相关的 pub 项
2. 单函数有 4 个以上参数（考虑用 struct 聚合）
3. 嵌套 if/loop 超过 4 层
4. 同文件内有 3 个以上互不调用的私有函数（说明职责杂糅）

### 拆分反例

```text
# 坏：按大小机械拆
utils.rs
utils_helpers.rs
utils_more.rs

# 好：按职责拆
policy.rs          ← 权限策略
local.rs           ← 本地执行器
guard.rs           ← TrustGuard trait
```

---

## 10. 日志规范

### 核心原则

1. **结构化字段**：不用字符串拼接
2. **分级合理**：默认 INFO，调试时 DEBUG
3. **不泄密**：敏感信息脱敏

### 工具

统一 `tracing` crate（不用 `log`）：

```rust
use tracing::{info, warn, error, debug, instrument};
```

`#[instrument]` 自动 span，跨 await 也能保留上下文。

### 级别约定

| 级别 | 用途 | 示例 |
|---|---|---|
| `ERROR` | 不可恢复错误，影响主流程 | session 写入失败、provider 致命错误 |
| `WARN` | 可恢复异常，值得注意 | 黑名单命中、降级回退、超时重试 |
| `INFO` | 关键业务节点 | session start/end、turn 开始、tool 调用 |
| `DEBUG` | 调试细节 | tool args 内容、provider 响应摘要 |
| `TRACE` | 极度详细 | 流式 chunk、内部状态变更 |

### 结构化字段

```rust
// 好：结构化字段
tracing::info!(
    session_id = %id,
    turn = n,
    tool_name = %name,
    "turn started"
);

// 坏：字符串拼接
tracing::info!("turn {} started for session {}", n, id);
```

### 约定字段名

跨模块统一字段名：

| 字段 | 类型 | 说明 |
|---|---|---|
| `session_id` | Display | Session ID |
| `turn_id` | Display | Turn ID |
| `tool_name` | str | 工具名 |
| `provider` | str | Provider 类型（openai/anthropic） |
| `model` | str | 模型名 |
| `duration_ms` | u64 | 耗时（毫秒） |
| `error` | Display | 错误对象 |

### 敏感信息

- ❌ **禁止 log**：API key、密码、完整 prompt 内容、用户私有数据
- ✅ **允许 log**：tool name、命令前缀（去参数，截断 100 字符）、错误消息摘要
- ✅ **允许 log**：session ID、turn ID、时间戳等元数据

### `#[instrument]` 使用

- 公共 API 入口推荐 `#[instrument(skip(self))]`
- 跳过 self 避免大对象日志
- 跨 await 自动保留 span：

```rust
#[instrument(skip(self, provider))]
pub async fn run_turn(&self, provider: &dyn Provider) -> Result<()> {
    // ...
}
```

### 禁止项

- 禁止 `println!` / `eprintln!` / `dbg!` 进入 crates/
- apps/ 下仅允许程序启动 banner（如 `eprintln!("Trust: workspace ...")`）
- 禁止用 `log` crate（已统一用 `tracing`）

---

## 11. 测试

### 核心原则

1. **单元测试与被测代码同文件**：`#[cfg(test)] mod tests`
2. **集成测试放在 `tests/` 目录**：只测公开 API
3. **测试命名描述行为**：场景式命名优先

### 测试命名

**优先场景式**（描述具体场景，可读性强）：

```rust
#[test]
fn curl_pipe_sh_blocked() { ... }

#[test]
fn git_c_inside_workspace_allowed() { ... }

#[test]
fn permissive_policy_allows_all() { ... }
```

**允许** `test_<场景>_<期望结果>` 格式：

```rust
#[test]
fn test_empty_input_returns_error() { ... }
```

两种风格可混用，但同一模块内保持一致。

### 测试组织

- **单元测试**：`#[cfg(test)] mod tests` 跟随源文件
- **集成测试**：`tests/` 目录，仅测 pub API
- **不独立 tests/ 目录存放单元测试**（除集成测试）

```rust
// crates/lim-core/src/id.rs
pub struct SessionId(Ulid);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip() {
        let id = SessionId::new();
        let s = id.to_string();
        let parsed: SessionId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }
}
```

### 断言约定

- 使用 `assert_eq!` / `assert_ne!` / `assert!`
- 错误信息要包含上下文：

```rust
// 好
assert_eq!(result.len(), expected, "expected {} items, got {}", expected, result.len());

// 坏：无上下文
assert_eq!(result.len(), expected);
```

### 异步测试

使用 `#[tokio::test]`，禁止单元测试内手动 spawn runtime：

```rust
#[tokio::test]
async fn provider_stream_returns_events() {
    let provider = MockProvider::new(vec![...]);
    let stream = provider.stream(request);
    let events: Vec<_> = collect_stream(stream).await;
    assert!(!events.is_empty());
}
```

### 预期 panic

```rust
#[test]
#[should_panic(expected = "unknown tool")]
fn unknown_tool_panics() {
    registry.execute("nonexistent", args).unwrap();
}
```

### Mock 约定

- 用项目内 `MockProvider` 等轻量 mock，禁止引入 `mockall` 等重型 mock 框架
- Mock 实现 trait，注入被测对象，不修改被测代码
- 测试中允许 `unwrap`，但生产代码不允许

### 基准测试

性能敏感模块用 `criterion`：

```text
benches/
└── stream_accumulator.rs
```

```toml
# Cargo.toml
[[bench]]
name = "stream_accumulator"
harness = false
```

非性能敏感模块不强制。

---

## 12. 文档注释

### 核心原则

1. **第一行一句话概述**：空一行后写详细说明
2. **使用 Markdown 格式**
3. **包含 `# Examples` / `# Errors` / `# Panics` / `# Safety` 章节**
4. **示例代码可运行**：`cargo test` 自动测试 doc example

### 文档注释格式

```rust
/// Fetches a resource by its unique identifier.
///
/// Queries the underlying storage for the resource associated with `id`.
///
/// # Errors
///
/// Returns `DbError::NotFound` if no resource matches the given id.
///
/// # Examples
///
/// ```
/// let db = Database::open("data.db")?;
/// let resource = db.fetch_by_id(42)?;
/// ```
pub fn fetch_by_id(&self, id: u64) -> Result<Resource> { ... }
```

### 分级要求

| 项 | 要求 |
|---|---|
| `pub fn` 返回 `Result` | 必须有 `# Errors` 章节 |
| `pub fn` 可能 panic | 必须有 `# Panics` 章节 |
| `unsafe fn` | 必须有 `# Safety` 章节 |
| `pub struct` / `pub enum` | 必须有 `///` 一行概述 |
| `pub trait` | 必须有 `///` 概述 + 每个方法 `///` |
| 内部 helper `fn` | 可只写一行 `///` 概述 |
| 私有项 | 不要求 |

### lib.rs 模块文档

所有 `lib.rs` 必须有 `//!` 模块文档说明 crate 用途：

```rust
//! lim-core: core types and primitives for the lim agent runtime.
//!
//! Provides IDs, errors, message types, tool abstractions,
//! and the [`ToolGuard`] trait for permission control.

pub mod error;
pub mod id;
// ...
```

### 普通注释

- 解释"为什么这么做"，而非"做了什么"
- 复杂算法、非直观逻辑必须补充注释
- TODO 注释格式：`// TODO(作者): 描述` 或 `// TODO: 描述`

### 反例

```rust
// 坏：无文档
pub fn run_command(cmd: &str) -> Result<Output> { ... }

// 坏：注释描述"做了什么"
pub fn run_command(cmd: &str) -> Result<Output> {
    // 调用 executor 执行命令
    executor.execute(cmd)
}

// 好：注释解释"为什么"
pub fn run_command(cmd: &str) -> Result<Output> {
    // 通过 executor 而非直接 std::process 是为了统一权限审计
    executor.execute(cmd)
}
```

---

## 13. 格式化与 Clippy

### rustfmt 配置

项目根目录 [rustfmt.toml](../../rustfmt.toml)：

```toml
max_width = 100
use_small_heuristics = "Max"
edition = "2024"
```

**可选 unstable 选项**（需要 nightly rustfmt）：

```toml
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
wrap_comments = true
format_code_in_doc_comments = true
```

启用方式：

```bash
rustup component add rustfmt-preview --toolchain nightly
cargo +nightly fmt -- --config-path rustfmt.toml
```

### 强制检查

```bash
# 格式化
cargo fmt --check

# Clippy 严格模式
cargo clippy --all-targets -- -D warnings
```

### Clippy 豁免

允许 `#[allow(clippy::xxx)]`，但必须附理由注释：

```rust
// 允许：理由清晰
#[allow(clippy::too_many_arguments)]  // 6 个参数来自 OpenAI API 规范，无法聚合
fn build_openai_request(...) -> serde_json::Value { ... }

// 禁止：无理由
#[allow(clippy::too_many_arguments)]
fn build_request(...) { ... }
```

### Import 顺序

推荐（unstable 选项启用后强制）：

```rust
// 1. std
use std::collections::HashMap;
use std::path::PathBuf;

// 2. 外部 crate
use serde::{Deserialize, Serialize};
use thiserror::Error;

// 3. 当前 crate
use crate::error::Result;
use crate::id::SessionId;

// 4. super / self
use super::provider::Provider;
```

---

## 14. Unsafe Rust

### 核心原则

1. **能不用就不用**：安全抽象优先
2. **unsafe 块必须有安全注释**
3. **unsafe 函数文档必须说明前置条件**

lim 项目预计在 FFI、平台特定系统调用（如 sandbox 集成）、性能关键路径等场景使用 unsafe，本节作为重点规范保留。

### SAFETY 注释

每个 `unsafe` 块上方必须有 `// SAFETY: ...` 注释，说明满足的不变量：

```rust
// SAFETY: `ptr` was obtained from `Box::into_raw` and has not been freed.
// The raw pointer is valid for the lifetime of the surrounding unsafe block.
unsafe {
    let value = ptr.read();
    // ...
}
```

### `unsafe fn` 文档

`unsafe fn` 的文档中必须有 `# Safety` 章节：

```rust
/// Writes a value to a raw memory location.
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` is valid for writes
/// - `ptr` is properly aligned
/// - No other references to `ptr` exist
pub unsafe fn write_raw<T>(ptr: *mut T, value: T) {
    // SAFETY: caller guarantees ptr is valid and aligned
    unsafe { ptr.write(value) };
}
```

### 范围最小化

- unsafe 代码尽量封装在极小范围内
- 对外暴露**安全接口**，把 unsafe 隐藏在内部：

```rust
// 好：对外安全，内部 unsafe
pub fn read_config(path: &Path) -> Result<Config> {
    let content = std::fs::read(path)?;
    // 内部可能用 unsafe 优化，但对外是安全的
    Ok(parse_config(&content)?)
}
```

### 禁止项

- **禁止为了"性能"随意写 unsafe**，必须有 benchmark 数据支撑收益
- **禁止无注释的 unsafe 块**
- **禁止 unsafe 暴露在公共 API**（除非是 FFI crate）

### FFI 场景

- 所有跨语言边界的类型必须 `#[repr(C)]`
- C 字符串用 `std::ffi::CString` / `CStr`，不裸 `*const c_char`
- 跨边界错误码用 `i32`，转换到 Rust 错误类型

### lim 预期使用场景

1. **Sandbox 集成**：Linux Landlock/seccomp、macOS Seatbelt 系统调用
2. **MXC 集成**：调用微软 Execution Container 的 native API
3. **Windows 进程管理**：CreateProcessW 等 Win32 API
4. **性能关键路径**：如 SSE 流式解析的零拷贝优化（需 benchmark 证明）

这些场景的 unsafe 代码必须有完整的 SAFETY 注释和 # Safety 文档。

---

## 附录：参考资源

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Rust Style Guide](https://github.com/rust-dev-tools/fmt-rfcs/blob/master/guide-guide.md)
- [The Rust Reference](https://doc.rust-lang.org/reference/)
- [Rust Unsafe Code Guidelines Reference](https://rust-lang.github.io/unsafe-code-guidelines/)
- [tokio Tutorial](https://tokio.rs/tokio/tutorial)
- [tracing Documentation](https://docs.rs/tracing)
