# lim 代码规范

本项目采用**两层规范结构**：日常开发遵循基础准则，阶段性 review 按完整规范执行。

## 文档索引

| 文档 | 用途 | 适用场景 |
|---|---|---|
| [basics.md](basics.md) | 日常基础准则（13 条） | 每次 commit 前自检 |
| [full-guide.md](full-guide.md) | 完整规范（14 节） | 阶段性 review、新模块设计、争议仲裁 |
| [commits.md](commits.md) | Commit message 规范（Conventional Commits + 双语）| 每次 commit |
| [../../rustfmt.toml](../../rustfmt.toml) | 代码格式化配置 | `cargo fmt` 自动应用 |
| [../architecture.md](../architecture.md) | 架构分层与依赖图 | 模块边界决策 |

## 使用方式

### 日常开发

commit 前对照 [basics.md](basics.md) 自检 13 条。CI 会强制执行其中可自动化的部分（clippy / fmt / 文件长度）。

### 阶段性 review

按 [full-guide.md](full-guide.md) 全文执行。重点关注：
- 新增模块的抽象层次是否符合第 5、7、9 节
- 错误处理是否满足第 2 节（外部错误 String、内部错误枚举化）
- 异步代码是否满足第 3 节（锁不跨 await、CancellationToken）
- 文档注释是否满足第 12 节分级要求

### 争议仲裁

当 basics 与 full-guide 冲突时，以 full-guide 为准。full-guide 未覆盖的边界 case，参考 Rust 官方 [API Guidelines](https://rust-lang.github.io/api-guidelines/)。

## 工具支持

```bash
# 安装 tokei（代码行统计，用于文件长度检查）
brew install tokei    # macOS
# 或 cargo install tokei

# 检查超长文件（> 800 代码行）
tokei crates apps packs -t Rust -e target --files -o json \
  | jq -r '.Rust.reports[] | select(.stats.code > 800) | "\(.stats.code)\t\(.name)"' \
  | sort -rn

# clippy 严格模式
cargo clippy --all-targets -- -D warnings

# 格式化检查
cargo fmt --check
```

## Changelog

规范动态迭代，新增/修改条目在此记录。

| 日期 | 变更 | 备注 |
|---|---|---|
| 2026-07-03 | 新增 commits.md | Commit message 规范（Conventional Commits + 双语要求）|
| 2026-06-30 | 初始版本 v1.0 | 建立 basics + full-guide 两层结构，14 节完整规范 |
