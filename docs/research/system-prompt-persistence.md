# System Prompt Persistence in AI Agents — Research

> Research date: 2026-06-30
> Scope: How mainstream AI coding agents keep system prompt instructions effective across multi-turn conversations.
> Related: [../draft/trust-and-approval.md](../draft/trust-and-approval.md), [../architecture.md](../architecture.md)

## TL;DR

**Relying on a single system prompt injection is far from enough.** Mainstream agents make instructions "appear" to persist after 20+ turns via three stacked layers:

1. **Per-turn resending + Prompt Caching** — LLM API is stateless; every request resends the full system prompt (prefix caching cuts cost to 1/10). So "instruction is always present" is a physical fact, not memory.
2. **Message-layer reminder + Recency anchor** — put volatile/reinforced instructions at the dynamic segment end (e.g. `CLAUDE.md`, `AGENTS.md`) or insert via `<system-reminder>` tags to leverage Recency attention.
3. **Tool-layer hard constraint fallback** — truly inviolable constraints (file write, command exec, network) are NOT in system prompt; they're refused by permission mode / sandbox / tool implementation itself — even if LLM "forgets," tools block.

**One sentence**: system prompt guides behavior; tool layer holds the bottom line. Both are indispensable.

## How Mainstream Agents Do It

### Claude Code (Anthropic) — 10-step system prompt assembly

From leaked `rust/crates/runtime/src/prompt.rs`, assembled by `SystemPromptBuilder`:

| Step | Content | Nature |
|---|---|---|
| 1. Intro | Role definition "You are an interactive agent…" | **Static** (hardcoded) |
| 2. Output Style | Output style (Concise etc.) | Static |
| 3. System Section | Tool permissions, security reminders, injection vigilance | Static |
| 4. Doing Tasks | No reckless code changes, no redundant abstraction, diagnose before fixing | Static |
| 5. Actions | Reversibility assessment, high-risk ops need authorization | Static |
| **— SYSTEM_PROMPT_DYNAMIC_BOUNDARY —** | | |
| 6. Environment | Model name / cwd / OS / date | Dynamic |
| 7. Project Context | git status / git diff snapshot | Dynamic |
| 8. Instruction Files | `CLAUDE.md` (collected upward) | Dynamic |
| 9. Runtime Config | Permission mode, MCP server | Dynamic |

**Key design points**:
- **Static/dynamic boundary**: steps 1–5 are cross-session stable, cacheable (very high cache hit); steps 6–9 re-injected per turn based on environment.
- **User-writable files at end (step 9)**: `CLAUDE.md` sits at prompt end, leveraging Recency attention — its constraint's actual weight ≥ steps 1–5 hardcoded rules. This is the mechanism by which "users can't change hardcoded segments but can effectively reshape most behavior."
- **Dynamic info via message layer, not system prompt edit**: when time changes or user edits files, Claude Code does NOT update system prompt (would zero the entire prefix cache); instead inserts `<system-reminder>` tag into messages. Info conveyed, prefix intact.

### Codex (OpenAI) — Five-layer input injection with role priority

Codex prompt is not a single string but a role-tagged **item list** with explicit priority:

| Role | Priority | Notes |
|---|---|---|
| system | highest | model base instructions |
| developer | high | developer-level instructions |
| user | medium | actual user input |
| assistant | lowest | model's own replies |

**Five-layer injection order** (first inference):
1. **Permissions message** (role=developer): sandbox permissions, network scope, writable dirs
2. **Developer Instructions** (optional): from `config.toml`
3. **User Instructions** (aggregated): `AGENTS.override.md` + `AGENTS.md` + every `AGENTS.md` from git root down to cwd
4. **Environment context**: cwd, shell type
5. **Actual user message**

**Philosophy**: "more specific instructions go later, but system-level guardrails always first." Later content has lower weight — a loud user message shouldn't override system.

**Cross-platform native sandbox** (Rust rewrite): macOS Seatbelt / Linux Landlock / Windows ACL — OS-level hard constraints, not Docker add-on. Even if LLM tries to escalate, kernel refuses.

### Cursor, Aider, Continue

- **Cursor**: `.cursor/rules` as "long-term, implicit system prompt"; rule types (Always / Auto Attached / Agent Requested) control injection timing. Engineering focus is on **external measurement**: Keep Rate + semantic satisfaction + A/B testing.
- **Aider**: `CONVENTIONS.md` as project spec entry; **Repo Map** compresses codebase into tree structure; multiple **edit formats** (diff/whole/udiff) as output format hard constraints.
- **Continue / Windsurf / Copilot**: `config.json` + `.continue/rules` / `.windsurfrules` / `.github/copilot-instructions.md`.
- **Unified trend** (2025+): OpenAI + Google jointly push `AGENTS.md` as cross-tool project memory standard; Claude Code / Codex / Gemini CLI support it.

## Technical Mechanisms

### Q1: Is single system prompt injection enough?

**Not enough, but from API perspective instructions never "disappear."**

LLM API is **stateless**: no cross-request memory; every request must resend full prompt (system + history + new input). So strictly, "is instruction still there at turn 20" — it's always there because it's resent every turn. **This is exactly why Prompt Caching exists**: cache stable prefix (system prompt + tool definitions) to reuse KV state, otherwise prefill cost explodes.

### Q2: Is system prompt re-injected every turn?

**Yes, but via two "looks unchanged" methods:**
1. **Static prefix resent as-is + cache hit**: system prompt text unchanged token-by-token, hits prefix cache, compute cost ~1/10 of normal.
2. **Dynamic info via message-layer `<system-reminder>`**: Anthropic engineer Thariq: "**Never modify system prompt mid-conversation; use message injection instead.**" System prompt is prefix front; changing one char zeros the entire session cache. Correct: put dynamic info ("now it's Wednesday" / "user just edited X") in `<system-reminder>` tag inserted into next turn's message.

### Q3: Prompt reinforcement (insert reminder before/after user message)?

**Yes, mainstream practice.** Forms:
- **Claude Code**: `<system-reminder>` tag for dynamic reminders; `CLAUDE.md` at prompt end for Recency effect.
- **Codex**: permissions / developer instructions explicitly before user message; `AGENTS.md` loaded from git root down, closer to cwd = later = higher weight.
- **Cursor**: Always-type rules injected every time; Agent Requested type called by model on demand.

### Q4: Tool-layer hard constraints?

**Yes, and this is the real security baseline.** Claude Code's 6 permission modes exemplify this:

| Mode | Behavior | Use case |
|---|---|---|
| `default` | ask before file edit / command exec | unfamiliar projects |
| `plan` | read-only, plan only, no source edits | before big changes |
| `acceptEdits` | auto-accept file edits | small rapid edits |
| `auto` | reduce confirmations, background safety checks | long tasks |
| `dontAsk` | only pre-approved tools | scripts/CI |
| `bypassPermissions` | basically no restrictions | **isolated env only** |

**Key philosophy**: entering Plan mode does NOT remove write tools (would bust cache prefix); instead `EnterPlanMode`/`ExitPlanMode` are independent tools that inform model via message of current plan state. **Tool set never changes; state expressed via messages.**

Codex uses OS-level sandbox (macOS Seatbelt / Linux Landlock / Windows ACL) as fallback — even if LLM wants to escalate to write system dirs, kernel refuses.

### Q5: How are key instructions preserved after context compaction?

**Codex's auto-compact**:
- When `auto_compact_limit` exceeds threshold, calls Responses API `/responses/compact` endpoint.
- Returns compressed item list with `type=compaction` special item containing `encrypted_content` — **not simple summary, but model's "latent understanding" preserved during compression**; higher info density than traditional summarization (doesn't lose tone, emphasis, reasoning path).

**Claude Code's Cache-Safe Forking**:
- Compaction uses **identical system prompt + identical tool definitions + identical context structure**, only appends one compaction message.
- From API perspective, this request is nearly identical to previous, cache fully hits, only compaction instruction itself billed as new tokens.
- **Iron rule: fork must share prefix**, otherwise cost multiplies.

## Failure Cases

### Public prompt injection bypass cases

1. **Bing Chat (Sydney) system prompt full leak** (Feb 2023): user via prompt injection made Bing spit full system prompt. Simon Willison formally named "Prompt Injection" attack category.
2. **OWASP LLM Top 10**: Prompt Injection listed as one of most dangerous vulnerability categories for LLM applications (2025–2026).
3. **MCP tool description injection**: Simon Willison analyzed MCP prompt injection — malicious instructions can hide in tool descriptions; multi-tool environments have **confused deputy** risk; putting private data, executable tools, and untrusted content in same context is an attack surface.
4. **Logs can attack Agent**: jqwik event — malicious instructions injected via tool-returned log text, hijacking agent behavior.

### Limitations of relying solely on system prompt

> "Your System Prompt is transparent paper to hackers. Don't think adding 'ignore all previous instructions' defense is safe — hackers have 10,000 ways to bypass it (nested encoding, multi-language switching, role-playing)."

- **"ignore previous instructions" defense ineffective**: bypass via nested encoding, multi-language switching, role-playing.
- **Multi-turn dilution**: in long conversations, earlier instructions have lower attention weight (everything beyond Primacy gets diluted).
- **Untrusted content pollution**: tool-returned web/file content may itself contain injection instructions.
- **Cache hit vs security contradiction**: to preserve cache, system prompt can't change; reinforcement only via message layer.

## Best Practices: system prompt + tool-layer hard constraint combo

### What belongs in system prompt

- **Identity and role definition** ("you are an X-domain assistant")
- **Output format preferences** (language, conciseness, comment style)
- **Task execution guidelines** (no reckless code changes, no redundant abstraction, diagnose before fixing)
- **Coding style, tech stack conventions** (best in `CLAUDE.md` / `AGENTS.md` at end for Recency boost)
- **Scope guidance** ("only modify `src/auth/`")
- **Reversibility reminders** (confirm before destructive ops) — "soft guidance," model follows but not enforced

### What MUST be in tool-layer hard constraints

| Constraint type | Tool-layer implementation |
|---|---|
| File write permission | permission mode (`plan`/`acceptEdits`/`dontAsk`), writable dir allowlist |
| Command execution | `--allowedTools`, command blocklist, shell sandbox |
| Network access | sandbox network isolation (Codex Seatbelt/Landlock) |
| Destructive ops | `rm -rf` / `git reset --hard` default confirm popup |
| Tool exposure surface | `defer_loading: true` lazy load, avoid full exposure |
| State transitions | model as tools (e.g. `EnterPlanMode`) not tool removal |

### Seven iron rules of the combo strategy

1. **Prompt caching is prefix match, don't touch prefix** — dynamic info via message-layer `<system-reminder>`.
2. **Don't add/remove tools mid-conversation** — model state transitions as tools.
3. **More specific instructions go later**, but system-level guardrails always first.
4. **Writable files (`CLAUDE.md` / `AGENTS.md`) at prompt end**, leveraging Recency for strongest attention.
5. **Compaction must share prefix** (cache-safe forking), avoid losing key instructions in compression.
6. **Monitor cache hit rate like uptime** — hit rate drops a few percent means cost doubles and indicates prompt structure problems.
7. **Put "bottom-line constraints" in tool layer** — even if LLM completely forgets system prompt, permission mode/sandbox/tool implementation refuses escalation.

## Implications for lim

Combined with lim's `AGENTS.md` "Containment Principle" — "if any code path creates files outside the project tree, it is a bug" — the correct landing approach:

- **System prompt layer**: state in `AGENTS.md` "all config/artifacts/sessions must stay within project dir" (already done).
- **Tool-layer hard constraint**: in lim's file write tool implementation, validate target path must be prefixed by project root; reject out-of-bounds (this is the real "even if LLM forgets, it holds" guarantee).
- **CI layer**: test cases covering "attempt to write to `/tmp` / `$HOME` must fail."

Writing "don't write outside project" in system prompt alone will be bypassed under long conversations or prompt injection.

## Sources

- [Claude source leak: System Prompt from ordinary perspective (part 1)](https://blog.csdn.net/FILOMENA/article/details/160400265)
- [OpenAI official: Unrolling the Codex agent loop](https://openai.com/index/unrolling-the-codex-agent-loop/)
- [Codex Agent Loop deep dive: Context Window management and compaction](http://m.toutiao.com/group/7645470093249446452/)
- [Anthropic engineer reveals: Claude Code key design - how important is Prompt caching?](http://m.toutiao.com/group/7612302476603163142/)
- [Plain language: Claude Code's 6 modes](http://m.toutiao.com/group/7656813628577858063/)
- [Claude Code June prompt evolution detailed解读](http://m.toutiao.com/group/7656455867814822443/)
- [Codex CLI source breakdown: OpenAI's Rust Agent runtime](https://blog.csdn.net/llm765800916/article/details/162361964)
- [Cursor Rules four principles / writing guide](http://m.toutiao.com/group/7530556105647211060/)
- [Cursor official: Continually improving our agent harness](https://www.cursor.com/blog/continually-improving-the-agent-harness)
- [Aider Repo Map official docs](https://aider.chat/2023/10/22/repomap.html)
- [OpenAI + Google jointly launch AGENTS.md](http://m.toutiao.com/group/7540717835988582947/)
- [Microsoft ChatGPT Bing hacked, full prompt leaked (Simon Willison names Prompt Injection)](https://cloud.tencent.cn/developer/article/2222705)
- [Prompt injection attack: AI Agent security boundary's most dangerous vulnerability](http://m.toutiao.com/group/7614445549428703771/)
- [When logs can attack Agent (MCP prompt injection)](http://m.toutiao.com/group/7646112692545012276/)
- [Prompt Caching architectural design cuts AI Agent cost 80%](http://m.toutiao.com/group/7629323966347264552/)
