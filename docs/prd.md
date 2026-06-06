# PRD: Agira CLI

## Overview
Agira is a Rust CLI tool that orchestrates AI-assisted software development workflows. It stores all project state in `~/.agira/<project-id>/` — never inside the target repo — and drives a structured multi-phase workflow (pending → enrich → implement → verify → done) by outputting role-specific prompts to stdout. Both humans and AI agents (Claude Code, Codex) interact through the same CLI surface. The agent reads a prompt from `agira task work`, does the work, then calls `agira task work --artifact` or `agira task fail` to advance state.

## Tech Decisions
- **Frontend:** N/A (CLI only)
- **Backend:** Rust
- **Database:** JSON files on disk (`tasks.json`, `config.json`)
- **Deployment:** Binary (cargo install / direct download)
- **Monorepo:** No

## Functional Modules

### FM-001: Project Resolution
**Priority:** P0
**Dependencies:** none
**Description:** Locate the current project's state directory by walking up from CWD to find the nearest `.git` root. Derive a stable, human-readable project slug from the root directory name, with a short hash suffix appended only on collision. Create `~/.agira/<slug>/` on first use.
**Constraints:**
- Slug is the git root's directory basename, lowercased, with non-alphanumeric characters replaced by `-` (e.g., `My App` → `my-app`)
- On slug collision (two different git roots that produce the same slug), append `-<first-6-chars-of-sha256-of-absolute-path>` to the conflicting entry
- If CWD is not inside a git repo, exit 1 with message on stderr: `error: not inside a git repository`
- `~/.agira/` and `~/.agira/<slug>/` are created if absent; failure to create exits 1 with the OS error
**Acceptance Criteria:**
- Running any agira subcommand inside a git repo creates `~/.agira/<slug>/` if it doesn't exist
- Running any agira subcommand outside a git repo prints the error message to stderr and exits 1
- Two projects with the same directory name but different absolute paths get distinct slugs

---

### FM-002: `agira init`
**Priority:** P0
**Dependencies:** FM-001
**Description:** One-time project setup. Accepts six flags that fully specify the project configuration and writes `~/.agira/<slug>/config.json` non-interactively. When called with no flags, instead of running an interactive interview it emits a Markdown agent-prompt to stdout that instructs the calling agent to scan the repo, interview the user for each value, and re-invoke `agira init` with all flags filled in. Does NOT write any files into the target repo.
**Constraints:**
- Flags (all required when any flag is present; bare invocation with none is valid — see below):
  - `--stack <name>` — one of: `rust`, `typescript`, `javascript`, `go`, `python`, `dart`, `flutter`, `unknown`
  - `--phases <phase1:model1,phase2:model2,...>` — ordered comma-separated `phase:model` pairs for the middle workflow phases; phase name has no spaces; model is a Claude model shortname (e.g. `opus`, `sonnet`, `haiku`); minimum one pair; `pending` and `done` are mandatory and auto-inserted if omitted
  - `--verification-commands <cmd1;cmd2;...>` — semicolon-separated shell commands, or the literal string `none` for an empty list
  - `--acceptance-testing <value>` — one of: `cli`, `api`, `ui`, `hybrid`, `none`
  - `--prd-path <path>` — optional; omit to leave `prd_path` absent from config
- When all required flags are provided: validate values, write config atomically (`.tmp` → rename), print `config written to <path>` to stdout, exit 0. If config already exists it is silently overwritten — no confirmation prompt.
- When called with no flags (bare invocation): print a Markdown-formatted agent prompt to stdout and exit 0. The prompt must:
  1. Instruct the agent to scan the repo root for stack markers (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `pubspec.yaml`) and derive sensible defaults
  2. List each flag and its purpose; for `--phases`, explain that `pending` is always first and `done` is always last, and include model recommendations per phase type: pending → `sonnet`, enriching (design/planning) → `opus`, in_progress (implementation) → `sonnet`, verifying (mechanical checks) → `haiku`, done → `haiku`; agent should confirm or override with the user
  3. End with a fenced `sh` code block containing the fully-formed `agira init` command template with every flag shown as a placeholder (e.g., `agira init --stack <stack> --phases <phase1:model1,phase2:model2,...> ...`)
- Partial flag sets (some but not all required flags present) exit 1 with: `error: agira init requires all flags or none; missing: --<flag> [--<flag> ...]`
- `max_retries` is not a flag; it is read from global config (`~/.agira/config.toml`) at init time and written into `config.json`
- No TTY requirement — the command is fully non-interactive in both the flag-driven and bare-invocation paths
**Acceptance Criteria:**
- `agira init --stack rust --phases "enriching:opus,in_progress:sonnet,verifying:haiku" --verification-commands "cargo fmt -- --check;cargo test" --acceptance-testing cli` writes a valid `config.json` with `phases` as an array of `{name, model}` objects ordered `pending,enriching,in_progress,verifying,done` and exits 0
- `agira init --stack rust --phases "in_progress:sonnet,verifying:haiku" --verification-commands none --acceptance-testing cli` writes a `config.json` with `verification.commands: []` and auto-inserted `pending`/`done` phases, then exits 0
- `agira init` (bare, no flags) prints Markdown to stdout containing instructions for the agent including per-phase model recommendations, and a fenced `sh` block with the `agira init` command template; exits 0
- `agira init --stack rust` (partial flags) exits 1 with `error: agira init requires all flags or none; missing: --phases --verification-commands --acceptance-testing`
- Re-running `agira init` with all flags when config already exists overwrites it silently; `agira task status` reflects the new config
- `agira init --stack rust ... --prd-path docs/prd.md` writes `prd_path: "docs/prd.md"` into config; omitting `--prd-path` leaves the key absent
- `agira init --phases "enriching:badmodel"` exits 1 with `error: unknown model: badmodel` (valid shortnames: `opus`, `sonnet`, `haiku`)

---

### FM-003: Task State Store
**Priority:** P0
**Dependencies:** FM-001
**Description:** Manages `~/.agira/<slug>/tasks.json` — the runtime source of truth for all task state. Provides validated read/write operations and enforces state machine transitions defined in `config.json`. Every state change is recorded in the task's `history` array with a timestamp and reason.
**Constraints:**
- Task schema: `id` (string, e.g. `task-001`), `title` (string), `description` (string), `state` (string, must match a phase name in config or `"failed"`), `prd_module_id` (optional string), `dependencies` (string array), `retry_count` (u32), `max_retries` (u32, default from config or 3), `phases` (object keyed by phase name, each `{artifact: string, completed_at: ISO8601 string}`), `history` (array of `{from, to, timestamp, reason}`), `created_at` (ISO8601 string)
- Config `phases` field schema: array of `{name: string, model?: string, duty?: string}` objects in workflow order (e.g. `[{"name": "pending"}, {"name": "enriching", "model": "opus", "duty": "Prepare an implementation plan and cite required evidence."}, {"name": "in_progress", "model": "sonnet"}, {"name": "done"}]`); `duty` is an optional freeform paragraph describing the subagent duty and required evidence/artifact; `pending` is mandatory first and `done` is mandatory last, and the loader auto-inserts either if omitted while stripping `model` and `duty` from both mandatory phases while preserving configured models and duties on middle phases; replaces the former flat `string[]` representation
- IDs are auto-assigned as `task-001`, `task-002`, ... in insertion order, zero-padded to 3 digits
- A task may only advance to the next phase in the configured order, or to `"failed"` from any phase
- A task with any dependency not in the terminal-done phase cannot advance past `pending`
- `history` is append-only; existing entries are never modified
- `max_retries` defaults to 3 if absent from config
- `tasks.json` is pretty-printed with 2-space indentation
- All writes to `tasks.json` use atomic replacement: write to `tasks.json.tmp` in the same directory, then `rename()` into place. This guarantees crash-safety — a process killed mid-write leaves the previous `tasks.json` intact
**Acceptance Criteria:**
- Adding a task writes it to `tasks.json` with `state: "pending"` and `retry_count: 0`
- Attempting to call `agira task work --artifact` on a task that would skip a phase exits 1 with: `error: invalid transition: <current> → <requested>` (only the sequential next phase is valid)
- A task with a dependency in `"failed"` state is reported as blocked by `agira task work` and cannot be advanced
- After any state change, the last entry in `history` reflects the transition with a non-null ISO8601 timestamp
- Killing the process during a write (e.g., `kill -9`) leaves `tasks.json` either fully updated or fully unchanged — never a partial/corrupt file

---

### FM-004: `agira task work`
**Priority:** P0
**Dependencies:** FM-003
**Description:** The main driver. Reads current task state, selects the highest-priority actionable task, and writes a complete, self-contained role prompt to stdout. The prompt tells the AI agent exactly what role to play, what the task requires, what success looks like, and which command to call when done. Pure stdout output — no user interaction.
**Constraints:**
- Accepts optional flag `--prd <path>` (absolute or CWD-relative); if given, reads the file and injects its content verbatim into the prompt as a `## Requirements Context` section
- Priority: tasks with satisfied dependencies in the earliest non-terminal phase come first; within the same phase, lower task number wins (task-001 before task-002)
- If all tasks are in terminal-done state, outputs a completion summary listing all tasks with their IDs, titles, and final artifacts, then exits 0
- If no tasks exist and `--prd` is given, outputs a decomposition prompt instructing the agent to break the PRD into tasks using `agira task add`
- If no tasks exist and `--prd` is absent, prints to stdout: `No tasks found. Add tasks with \`agira task add "<title>"\` or provide requirements with \`agira task work --prd <path>\``
- The prompt must include: task id and title, current phase name and agent role (from config), task description, acceptance criteria if the `phases` object has prior enrichment data, the verification commands if the current phase is a verification phase, and the exact command(s) to call to advance state
- Output contains no ANSI escape sequences (verified by piping through `cat -v`)
- Exits 0 in all non-error cases; exits 1 only on config/file read errors
**Acceptance Criteria:**
- `agira task work` with no tasks and no `--prd` prints the no-tasks message and exits 0
- `agira task work --prd docs/prd.md` with no tasks prints a decomposition prompt that contains text from the PRD file
- `agira task work` with one task in phase 2 prints a prompt naming the agent role configured for phase 2, the task description, and a line containing `agira task work --artifact`
- `agira task work` output piped through `cat -v` shows no `^[` sequences (no ANSI codes)

---

### FM-005: `agira task work --artifact <evidence>`
**Priority:** P0
**Dependencies:** FM-003
**Description:** Advances the current actionable task from its current phase to the next phase in the configured state machine. Records the provided artifact as evidence for the completed phase. When called on the final configured phase, transitions the task to terminal-done state. `--artifact` must be non-empty when provided.
**Constraints:**
- Omitting `--artifact` prints the current actionable task prompt instead of advancing state
- Empty string artifact exits 1 on stderr: `error: artifact must not be empty`
- The artifact is stored in `phases[<current_phase>].artifact` and `phases[<current_phase>].completed_at`
- On non-terminal advance, prints to stdout: `task-001 → <next_phase>`
- On terminal-done transition, prints to stdout: `task-001 done ✓`
- If no actionable task exists, exits 1: `error: no actionable task — all tasks are done or blocked`
**Acceptance Criteria:**
- `agira task work --artifact "tests pass"` on a task in a non-final phase advances it to the next phase, stores the artifact, and `agira task status` shows the updated state
- `agira task work --artifact "tests pass"` on a task in the final phase transitions it to terminal-done; `agira task status` marks it with `✓`
- `agira task work` without `--artifact` prints the current task prompt instead of advancing
- `agira task work --artifact "x"` with no actionable task exits 1 with the no-actionable-task message on stderr

---

### FM-006: `agira task fail <id> --reason <reason>`
**Priority:** P0
**Dependencies:** FM-003
**Description:** Records a failure for a task's current phase. If `retry_count < max_retries`, resets the task back to `pending` and increments `retry_count`. If `retry_count >= max_retries`, transitions to terminal `"failed"` state. `--reason` is required and must be non-empty.
**Constraints:**
- `--reason` is a required flag; omitting it exits 1: `error: --reason is required`
- Empty string reason exits 1: `error: reason must not be empty`
- On retry: `retry_count` increments, task state resets to `pending`, history entry reason: `"retry <n>/<max>: <reason>"`; prints: `task-001 retrying (<n>/<max>): <reason>`
- On terminal fail: state set to `"failed"`, history entry reason: `"failed (max retries): <reason>"`; prints: `task-001 failed — max retries reached`
- Calling `agira task fail` on a terminal-failed task exits 1: `error: task <id> is already failed`
- Calling `agira task work --artifact` when no actionable task remains exits 1: `error: no actionable task — all tasks are done or blocked`
**Acceptance Criteria:**
- `agira task fail task-001 --reason "compilation error"` on a task with `retry_count: 0` and `max_retries: 3` resets it to `pending` with `retry_count: 1`; `agira task status` shows updated state and count
- `agira task fail task-001 --reason "still broken"` on a task with `retry_count: 2` and `max_retries: 3` sets `state` to `"failed"`
- `agira task fail task-001` without `--reason` exits 1 with the required error message
- Calling `agira task work --artifact "x"` when no actionable task remains exits 1 with the no-actionable-task error

---

### FM-007: `agira task add`
**Priority:** P1
**Dependencies:** FM-003
**Description:** Adds a new task to `tasks.json`. Used by the agent after receiving a decomposition prompt from `agira task work --prd`, or by humans adding tasks manually.
**Constraints:**
- Synopsis: `agira task add "<title>" [--description "<desc>"] [--prd <module-id>] [--depends-on <id>[,<id>...]]`
- `<title>` is a required positional argument
- IDs are auto-assigned as the next sequential `task-NNN`; if the last task is `task-005`, the new one is `task-006`
- `--depends-on` accepts comma-separated existing task IDs; exits 1 if any listed ID does not exist: `error: unknown dependency: <id>`
- Prints to stdout: `added task-006: <title>`
**Acceptance Criteria:**
- `agira task add "Implement login endpoint"` creates a task with the next sequential ID in `pending`; `agira task status` shows it
- `agira task add "Deploy" --depends-on task-001,task-002` creates the task with `dependencies: ["task-001", "task-002"]`
- `agira task add "Blocked" --depends-on task-999` exits 1 with: `error: unknown dependency: task-999`
- Running `agira task status` after `agira task add` shows the new task in `pending`

---

### FM-008: `agira task status`
**Priority:** P1
**Dependencies:** FM-003
**Description:** Prints a human-readable table of all tasks with their current state, retry count, and the reason from the most recent history entry. Intended for human inspection.
**Constraints:**
- Columns: `ID`, `Title` (truncated at 40 chars with `…`), `State`, `Retries`, `Last Action`
- Rows sorted by task ID descending
- `--limit <n>` limits the latest-first table after sorting; default is `20`; `--limit 0` shows all tasks
- `--offset <n>` skips tasks from the latest-first sorted list before applying `--limit`; default is `0`
- Terminal-done tasks show `✓` prefix on state; terminal-failed tasks show `✗` prefix
- If no tasks exist, prints: `No tasks. Run \`agira task add\` or \`agira task work --prd <path>\` to get started.`
- `--json` flag outputs the raw contents of `tasks.json` and bypasses pagination
**Acceptance Criteria:**
- `agira task status` in a project with 3 tasks prints a table with exactly 3 rows and correct states
- `agira task status --json` outputs valid JSON parseable by `jq .`
- `agira task status` in a project with no tasks prints the no-tasks message and exits 0

---

### FM-009: Global User Config
**Priority:** P1
**Dependencies:** FM-001
**Description:** User-wide defaults stored in `~/.agira/config.toml`. Provides fallback values for any project that does not specify them in its own `config.json`.
**Constraints:**
- Location: `~/.agira/config.toml`; created with defaults on first run if absent
- Supported keys: `default_max_retries` (integer, default 3)
- `default_model` is removed — model is now configured per phase in `config.json`
- Project-level `config.json` values always override global config
- Malformed TOML exits 1: `error: invalid global config at ~/.agira/config.toml: <parse error>`
**Acceptance Criteria:**
- On first run with no existing `~/.agira/config.toml`, the file is created with default values
- Setting `default_max_retries = 5` in `config.toml` causes new projects without an explicit `max_retries` to use 5 as the default
- A malformed `config.toml` causes any agira command to exit 1 with the parse error message

---

### FM-011: Orchestrator-only prompt pattern in `agira task work`
**Priority:** P1
**Dependencies:** FM-002, FM-003
**Description:** The calling agent is always the orchestrator — it reads `agira task work` output and spawns a subagent to do the actual phase work. `agira task work` must structure its stdout so the orchestrator knows exactly what to delegate and to which model, without ever doing the work itself.
**Constraints:**
- The prompt emitted by `agira task work` is split into two logical sections separated by a visible delimiter (`--- SUBAGENT PROMPT ---` / `--- END SUBAGENT PROMPT ---`):
  1. **Orchestrator preamble** (before the delimiter): instructs the calling agent that it is the orchestrator, must NOT perform the work itself, must spawn a generic subagent using the model specified in this phase's config (`phase.model`), pass the subagent prompt verbatim, collect the subagent's output, and call `agira task work --artifact "<subagent summary>"` when done
  2. **Subagent prompt** (between the delimiters): the task instructions the subagent will receive — task id, title, current phase name, description, acceptance criteria (if prior enrichment exists), verification commands (if this is a verification phase), and the exact `agira task work --artifact` command the orchestrator should call after
- The orchestrator preamble includes the model shortname from `config.phases[current].model` (e.g. `opus`, `sonnet`, `haiku`) so the calling agent knows which model to use when spawning the subagent
- The format must be stable and machine-parseable: the delimiter lines are exact ASCII strings on their own lines
- No ANSI escape sequences anywhere in the output
- Exits 0 in all non-error cases; exits 1 only on config/file read errors
**Acceptance Criteria:**
- `agira task work` output contains the exact line `--- SUBAGENT PROMPT ---` and later `--- END SUBAGENT PROMPT ---`
- The orchestrator preamble section contains the model shortname from the current phase's config
- The orchestrator preamble explicitly states "do not perform this work yourself" (or equivalent unambiguous instruction)
- The subagent prompt section contains the task title, current phase name, and the `agira task work --artifact` command
- Output piped through `cat -v` shows no `^[` sequences

---

### FM-012: Hook Configuration Schema
**Priority:** P1
**Dependencies:** FM-001, FM-009
**Description:** Task 创建和状态变更时触发的 shell hook 配置规范。分两层：全局 hook 写在 `~/.agira/config.toml` 的 `[[hooks]]` 数组中，对所有项目生效；per-project hook 写在 `~/.agira/<slug>/hooks.toml`，仅对当前项目生效。两者格式相同。
**Constraints:**
- 每条 hook 是 TOML `[[hooks]]` 条目，两个必填字段：
  - `on` — 字符串：具体 phase 名（如 `"done"`、`"verifying"`）、`"task_added"`、`"failed"`，或 `"*"`（任意 lifecycle event 都触发）
  - `run` — 非空 shell 命令字符串，通过 `sh -c "<run>"` 执行
- 全局 hook：`~/.agira/config.toml` 的 `[[hooks]]` 段
- Per-project hook：`~/.agira/<slug>/hooks.toml`（文件不存在 = 无项目级 hook，不报错）
- `on` 值与任何 phase 都不匹配时静默忽略（不报错、不警告）
- hooks 配置 TOML 解析失败时退出 1：`error: invalid hooks config at <path>: <parse error>`
**Acceptance Criteria:**
- `~/.agira/config.toml` 中加入 `[[hooks]] on = "done" run = "echo done"` 后，所有项目的 done 转换都会触发该 hook
- `~/.agira/config.toml` 中加入 `[[hooks]] on = "task_added" run = "echo created"` 后，所有项目通过 `agira task add` 成功创建 task 后都会触发该 hook
- `~/.agira/<slug>/hooks.toml` 中加入 `[[hooks]] on = "*" run = "echo changed"` 后，该项目每次 task 创建或状态变更都触发
- `~/.agira/<slug>/hooks.toml` 不存在时，任何命令均不报错、正常运行
- hooks 段 TOML 格式错误时，任何命令退出 1 并输出解析错误

---

### FM-013: Hook Execution
**Priority:** P1
**Dependencies:** FM-012, FM-003
**Description:** Task 创建或状态写入磁盘后，agira 收集所有匹配的 hook（全局 + per-project），依次以 `sh -c` 启动 detached 子进程。Fire-and-forget：agira 不等待、不检查 exit code，立即返回。
**Constraints:**
- Hook 触发时机：`tasks.json` 原子 rename 成功**之后**
- 匹配规则：`on == "*"` 或 `on == <event>`；状态转换的 `<event>` 是目标 phase 名（含 `"done"` 和 `"failed"`），task 创建的 `<event>` 是 `"task_added"`
- 执行顺序：全局 hook 按文件顺序先执行，然后是 per-project hook
- 每个子进程注入以下环境变量（叠加到父进程 env 上）：
  - `AGIRA_TASK_ID` — 如 `task-001`
  - `AGIRA_TASK_TITLE` — task 标题
  - `AGIRA_TASK_DESCRIPTION` — task 描述
  - `AGIRA_TASK_STATE` — lifecycle event 完成后的 task state
  - `AGIRA_TASK_PRD_MODULE_ID` — task 的 PRD module ID；未设置则为空字符串
  - `AGIRA_TASK_DEPENDENCIES` — 逗号分隔的 dependency task IDs；无依赖则为空字符串
  - `AGIRA_TASK_RETRY_COUNT` — 当前 retry count
  - `AGIRA_TASK_MAX_RETRIES` — task 的最大 retry 次数
  - `AGIRA_TASK_CREATED_AT` — task 创建时间，RFC3339 字符串
  - `AGIRA_PROJECT_SLUG` — 项目 slug
  - `AGIRA_PROJECT_PATH` — canonical git root 路径
  - `AGIRA_FROM_PHASE` — 变更前 phase 名；`task_added` 时为空字符串
  - `AGIRA_TO_PHASE` — 变更后 phase 名；`task_added` 时为新 task 的初始 phase
  - `AGIRA_ARTIFACT` — `--artifact` 传入的字符串；若不适用则为空字符串
- 子进程完全 detached（等价于 `nohup sh -c "..." &disown`），与 agira 进程生命周期无关
- Hook 子进程的 stdout/stderr 不影响 agira 自身输出
- `sh` 启动失败时向 stderr 输出单行警告，不影响 agira exit code
**Acceptance Criteria:**
- 全局配置 `[[hooks]] on = "done" run = "touch /tmp/agira-hook-fired"` 后，任意项目的 task advance 到 done，`/tmp/agira-hook-fired` 被创建
- 全局配置 `[[hooks]] on = "task_added" run = "touch /tmp/agira-task-added"` 后，任意项目通过 `agira task add` 成功创建 task，`/tmp/agira-task-added` 被创建
- Hook 子进程中 `AGIRA_TO_PHASE=done`、`AGIRA_TASK_ID=task-001` 等变量可正确读取
- `task_added` hook 子进程中 `AGIRA_TASK_DESCRIPTION`、`AGIRA_TASK_PRD_MODULE_ID`、`AGIRA_FROM_PHASE=""`、`AGIRA_TO_PHASE=<initial phase>` 等变量可正确读取
- phase 不匹配的 hook 不触发（如 `on = "done"` 的 hook 在 `in_progress → verifying` 时不触发）
- `on = "*"` 的 hook 在 task 创建和 `failed` 转换时也触发
- agira 打印输出后立即退出 0，不等待 hook 子进程

---

## Non-Functional Requirements
- All agira subcommands resolve project and load config in under 100ms (no network I/O on the hot path)
- `tasks.json` and `config.json` are pretty-printed, human-readable JSON (2-space indent)
- All error messages go to stderr; all prompts and structured output go to stdout
- Exit codes: 0 = success, 1 = user error (bad args, invalid state transition, file not found), 2 = internal/IO error
- macOS (arm64 + x86_64) and Linux (x86_64) are supported targets; Windows is out of scope for v1

## Out of Scope
- HTTP/socket API or web UI
- Claude Code hook script generation (the CLI is self-contained; hooks are not required)
- Jira or GitHub Issues integration
- Multi-project parallel agent execution in a single invocation
- Windows support in v1
- Encrypting or access-controlling `~/.agira/` contents
- Migration tooling from the old TypeScript Agira

### FM-010: Help Descriptions and Version Command
**Priority:** P1
**Dependencies:** FM-001
**Description:** Add descriptive help text to all CLI subcommands and their flags so `agira --help` and `agira <cmd> --help` are useful to AI agents and humans alike. Add a `version` subcommand and enable clap's built-in `--version` / `-V` flag, both printing `agira <semver>` baked in from `Cargo.toml` at compile time.
**Constraints:**
- Every subcommand has a one-line `about` description shown in `agira --help`
- Every flag/arg has a one-line `help` description shown in `agira <cmd> --help`
- `agira version` prints `agira <semver>` to stdout and exits 0
- `agira --version` and `agira -V` print the same string (clap built-in format: `agira <semver>`)
- Version is embedded at compile time via `env!("CARGO_PKG_VERSION")`; it always matches `Cargo.toml`
**Acceptance Criteria:**
- `agira --help` lists all subcommands each with a non-empty description
- `agira task work --help` shows a description for `--prd`
- `agira version` prints `agira 0.1.0` (current version) to stdout and exits 0
- `agira --version` prints the same version string and exits 0
- `agira -V` prints the same version string and exits 0

---

## Changelog
### Round 6 — 2026-06-07
- FM-003: add optional per-phase `duty` field for freeform subagent duty/evidence instructions; loader strips it from mandatory `pending` and `done` phases, and prompt injection is reserved for a later task

### Round 5 — 2026-06-05
- FM-012 (new): hook configuration schema — global (`~/.agira/config.toml`) and per-project (`~/.agira/<slug>/hooks.toml`) TOML `[[hooks]]` tables with `on` and `run` fields
- FM-013 (new): hook execution — fire-and-forget detached `sh -c` subprocess after atomic write, with `AGIRA_TASK_ID/TITLE/PROJECT_SLUG/PROJECT_PATH/FROM_PHASE/TO_PHASE/ARTIFACT` env vars; global hooks first then per-project
- FM-003: add mandatory `pending` and `done` phases; startup normalizes configs so `pending` is first and `done` is terminal, preserving user-provided models without duplication

### Round 4 — 2026-06-05
- FM-002: remove `--models` flag; fold model into `--phases` as `phase:model` pairs (e.g. `enriching:opus,in_progress:sonnet`); bare invocation now recommends model per phase type; remove `default_model` from global config
- FM-003: config `phases` field changes from `string[]` to `[{name, model}]`
- FM-009: remove `default_model` from `~/.agira/config.toml` — model is now per-phase in project config
- FM-011 (new): orchestrator-only prompt pattern — `agira task work` wraps task instructions in a subagent block with an explicit preamble telling the calling agent to delegate, never execute

### Round 3 — 2026-06-04
- FM-002: redesign `agira init` from interactive-TTY to flag-driven. Six required flags replace the stdin interview. Bare invocation emits a Markdown agent-prompt to stdout with a fenced command template. Silent overwrite replaces the interactive overwrite confirmation. TTY requirement removed.

### Round 2 — 2026-06-04
- FM-010: add help descriptions to all commands/flags and add `version` subcommand + `--version` flag

### Round 1 — 2026-06-03
- Full rewrite of PRD for Rust CLI. Replaces prior TypeScript/Bun protocol library.
- Core model: state stored in `~/.agira/<slug>/`, prompts emitted to stdout, agents drive workflow via `agira task work` / `agira task work --artifact` / `agira task fail`
- `agira task work --artifact` requires a non-empty artifact, `agira task fail` requires `--reason`; both are enforced at the CLI layer
- PRD path is a runtime flag (`--prd`), not hardcoded; can be passed on any `agira task work` invocation
- `agira init` is interactive-only; no files written to the target repo
- Storage format confirmed as JSON (not SQLite): scale is small, no concurrent writers, human-inspectability is a first-class concern; crash-safety achieved via atomic rename (write to `.tmp`, then `rename()`) rather than WAL
