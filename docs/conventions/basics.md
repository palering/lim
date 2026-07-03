# 日常基础准则

每次 commit 前对照以下 13 条自检。CI 会强制执行其中可自动化的部分。

完整规范见 [full-guide.md](full-guide.md)，争议时以完整规范为准。

---

## 质量门

### 1. Clippy 零警告

```bash
cargo clippy --all-targets -- -D warnings
```

允许 `#[allow(clippy::xxx)]` 但必须附理由注释。

### 2. 格式化零差异

```bash
cargo fmt --check
```

配置见 [rustfmt.toml](../../rustfmt.toml)。

### 3. 禁止占位代码

禁止 `todo!` / `unimplemented!` / `FIXME` / `HACK` / `XXX` 进入 main 分支。

`TODO` 注释允许在分支开发期使用，但 PR 合并前必须清理。

---

## 错误与 panic

### 4. 公共 API 用 `lim_core::Result<T>`

crate 公共 API 必须返回 `lim_core::error::Result<T>`（即 `Result<T, LimError>`），禁止 `anyhow::Result` 跨 crate 边界。

`anyhow` 仅允许在 binary 顶层 main（apps/cli、packs/novel 的 bin）使用。apps/tui 虽位于 apps/ 但是 lib crate，公共 API 也必须用 `lim_core::Result`。

### 5. 非测试 `unwrap` 必须配 `expect("reason")`

非测试代码中 `unwrap` 仅允许用于：
- 逻辑上不可能失败的情形（前置检查已保证 Some/Ok）
- 启动期不可恢复的初始化

且必须改为 `expect("说明为何安全")`，理由写明。

### 6. 错误用 `?` 传播，保留 source 链

优先 `?` 操作符，避免嵌套 `match`。带底层错误的必须用构造器保留 source：

```rust
// 好：保留 source + 带上下文
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution("read", format!("failed to read {path}: {e}"), e))?;

// 坏：字符串化底层错误，丢失 source 链
let content = std::fs::read_to_string(&path)
    .map_err(|e| LimError::tool_execution_msg("read", format!("failed: {e}")))?;
```

无底层错误的纯消息用 `_msg` 构造器。详见 [full-guide.md 错误处理](full-guide.md#2-错误处理)。

---

## 异步

### 7. 锁不跨 `.await`

持有 `Mutex` / `RwLock` 守卫时禁止进入 `.await`。需要异步操作时，先在锁作用域内提取数据，释放锁后再 await。

### 8. 阻塞 IO 必须 `spawn_blocking`

CPU 密集型计算、同步 IO（如 `std::fs::read`）必须用 `tokio::task::spawn_blocking`，禁止在 async 上下文中直接调用。

---

## 结构与文档

### 9. 单文件代码行 ≤ 800

使用 `tokei` 统计 Code 列（不含注释和空行）：

```bash
tokei <path> -t Rust -e target
```

- **豁免**：`#[cfg(test)] mod tests` 块和 doc-example（`/// ```rust` 代码块）不计入
- **超过 800**：必须拆分到其他文件
- **600-800**：新增功能优先拆分到其他文件

### 10. 函数长度

- < 30 行：舒适
- 30-60 行：检查职责
- 60-100 行：必须注释说明为何不拆
- \> 100 行：必须拆分

### 11. `pub fn` 必须有文档

所有 `pub fn` / `pub struct` / `pub enum` / `pub trait` 必须有 `///` 文档注释，至少一行概述。

公共 API（被外部 crate 调用的）必须包含 `# Errors` 章节（如果返回 Result）。

### 12. 模块用 `foo.rs` + `foo/`，不用 `mod.rs`

```text
src/
├── lib.rs          ← 仅 mod 声明 + pub use 重导出
├── config.rs       ← 模块入口
└── config/
    ├── loader.rs   ← 子模块
    └── validator.rs
```

---

## 日志

### 13. 日志用 `tracing`，禁止 `println!`

crates/ 下禁止 `println!` / `eprintln!` / `dbg!`，必须用 `tracing` 宏。

apps/ 下仅允许在程序启动时输出 banner 信息（如 `eprintln!("Trust: workspace ...")`），业务逻辑仍用 tracing。

敏感信息（API key、密码、完整 prompt 内容）禁止 log；tool args 可摘要打印但截断 100 字符。
