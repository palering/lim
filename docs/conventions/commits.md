# Commit Message 规范

本项目遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范，并要求 commit message 保证至少有一份英文版本，以便后续开源与 AI 索引。

---

## 格式

```
<type>(<scope>): <subject>

<body>

<footer>
```

- **type**（必填）：commit 类别
- **scope**（可选）：影响范围，如 `ci`、`deps`、`agent`、`tui`
- **subject**（必填）：简短描述，**英文**，祈使句，小写开头，不加句号，≤ 72 字符
- **body**（可选）：详细说明，解释"为什么"和"做了什么"
- **footer**（可选）：`BREAKING CHANGE:`、关联 issue 等

---

## type 列表

| type | 用途 |
|---|---|
| `feat` | 新功能 |
| `fix` | bug 修复 |
| `refactor` | 重构（不改变外部行为）|
| `docs` | 文档变更 |
| `test` | 测试相关 |
| `chore` | 构建、依赖、配置等杂项 |
| `ci` | CI/CD 配置 |
| `perf` | 性能优化 |
| `style` | 代码风格（不改逻辑，如 fmt）|
| `build` | 构建系统或外部依赖变更 |

---

## 语言要求

### 核心原则：**至少一份英文**

| 来源 | body 语言 | 要求 |
|---|---|---|
| 人工（母语中文）| 中文 + 英文 | 双语，英文可由 AI 协助生成 |
| 人工（母语英文）| 英文 | 仅英文即可 |
| Bot（dependabot 等）| 英文 | 仅英文即可 |

- **subject**：始终英文（与 dependabot、GitHub 生态一致）
- **body**：原始语言不限，但**必须包含至少一份英文版本**（可后续补充或由 AI 生成）
- **changelog**：多语言版本，发布时统一生成

### 示例

**双语 body（人工提交）**：

```
chore(ci): bump MSRV to 1.95

## 中文说明

学 bevy 紧贴 stable 的做法。lim 是应用端项目，无下游兼容压力，
将 MSRV 从 1.88 上调到 1.95，让 resolver v3 选到更新的传递依赖版本。

不加独立 MSRV check job：1.95 与 CI stable(1.96) 仅差 1 个版本，
CI 在 stable 跑 clippy+test 已隐含覆盖 1.95 兼容性。

## English summary

Follow bevy's approach of tracking stable closely. lim is an application
project with no downstream compatibility pressure. Bump MSRV from 1.88 to
1.95 so resolver v3 picks newer transitive dependencies.

No dedicated MSRV check job: 1.95 is one version behind CI stable (1.96),
CI running clippy+test on stable implicitly covers 1.95 compatibility.
```

**纯英文 body（bot 或英文母语）**：

```
chore(deps): update reqwest requirement from 0.12 to 0.13

Updates the requirements on reqwest to permit the latest version.
```

---

## 模板

复制以下模板用于双语 commit：

```
<type>(<scope>): <english subject>

## 中文说明

<中文详细说明：背景、变更、理由>

## English summary

<English description: background, changes, rationale>
```

---

## PR 流程（合并前检查）

合并 PR 前自检：

- [ ] subject 是英文，符合 Conventional Commits 格式
- [ ] body 包含至少一份英文版本（bot PR 除外）
- [ ] CI 通过（clippy / fmt / test）
- [ ] 涉及行为变更的，对照 [behavior-trace.md](../architecture/behavior-trace.md) 第 9 节自检可观测性

---

## Changelog

| 日期 | 变更 | 备注 |
|---|---|---|
| 2026-07-03 | 初始版本 v1.0 | 建立 Conventional Commits + 双语规范 |
