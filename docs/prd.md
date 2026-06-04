# PRD: Agira CLI

## Overview
Agira is a Rust CLI tool that orchestrates AI-assisted software development workflows. It stores all project state in `~/.agira/<project-id>/` — never inside the target repo — and drives a structured multi-phase workflow (enrich → implement → verify → done) by outputting role-specific prompts to stdout. Both humans and AI agents (Claude Code, Codex) interact through the same CLI surface. The agent reads a prompt from `agira next`, does the work, then calls `agira done` or `agira fail` to advance state.

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
**Description:** One-time project setup. Scans the target repo to detect stack, test/build/lint commands, and commit conventions. Then interviews the user (interactively via stdin/stdout) to configure the state machine, model assignments, verification commands, acceptance testing strategy, and an optional default PRD path. Writes the result to `~/.agira/<slug>/config.json`. Does NOT write any files into the target repo.
**Constraints:**
- Scan detects: language/runtime marker (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `pubspec.yaml`, etc.), test command, lint/format command, and commit message pattern from `git log --no-merges -10 --format="%s"`
- Interview must cover: (1) state machine — propose a default phase list based on detected stack, let user confirm or edit; (2) model per agent role — propose defaults, let user confirm; (3) verification commands — propose from scan, let user confirm/override; (4) acceptance testing strategy (`cli` / `api` / `ui` / `hybrid` / `none`); (5) default PRD path (optional, may be blank)
- State machine is stored as an ordered array of phase name strings; the final phase is the terminal-done phase
- If `~/.agira/<slug>/config.json` already exists, print `Config already exists. Overwrite? [y/N]` and abort on anything other than `y`/`Y`
- In non-tty mode (stdout is not a terminal), exit 1 with: `error: agira init requires an interactive terminal`
**Acceptance Criteria:**
- After `agira init` in a TypeScript/Bun repo, `~/.agira/<slug>/config.json` exists and contains `stack`, `state_machine` (array), `models` (object), `verification` (object with `commands` array), `acceptance_testing` (string), and optionally `prd_path`
- Running `agira init` a second time prompts the user to confirm overwrite; answering `n` leaves config unchanged
- In a non-tty context (e.g., `echo "" | agira init`), exits 1 with the error message

---

### FM-003: Task State Store
**Priority:** P0
**Dependencies:** FM-001
**Description:** Manages `~/.agira/<slug>/tasks.json` — the runtime source of truth for all task state. Provides validated read/write operations and enforces state machine transitions defined in `config.json`. Every state change is recorded in the task's `history` array with a timestamp and reason.
**Constraints:**
- Task schema: `id` (string, e.g. `task-001`), `title` (string), `description` (string), `state` (string, must match a phase in config or `"failed"`), `prd_module_id` (optional string), `dependencies` (string array), `retry_count` (u32), `max_retries` (u32, default from config or 3), `phases` (object keyed by phase name, each `{artifact: string, completed_at: ISO8601 string}`), `history` (array of `{from, to, timestamp, reason}`), `created_at` (ISO8601 string)
- IDs are auto-assigned as `task-001`, `task-002`, ... in insertion order, zero-padded to 3 digits
- A task may only advance to the next phase in the configured order, or to `"failed"` from any phase
- A task with any dependency not in the terminal-done phase cannot advance past the first phase
- `history` is append-only; existing entries are never modified
- `max_retries` defaults to 3 if absent from config
- `tasks.json` is pretty-printed with 2-space indentation
- All writes to `tasks.json` use atomic replacement: write to `tasks.json.tmp` in the same directory, then `rename()` into place. This guarantees crash-safety — a process killed mid-write leaves the previous `tasks.json` intact
**Acceptance Criteria:**
- Adding a task writes it to `tasks.json` with `state` equal to the first configured phase and `retry_count: 0`
- Attempting to call `agira done` on a task that would skip a phase exits 1 with: `error: invalid transition: <current> → <requested>` (only the sequential next phase is valid)
- A task with a dependency in `"failed"` state is reported as blocked by `agira next` and cannot be advanced
- After any state change, the last entry in `history` reflects the transition with a non-null ISO8601 timestamp
- Killing the process during a write (e.g., `kill -9`) leaves `tasks.json` either fully updated or fully unchanged — never a partial/corrupt file

---

### FM-004: `agira next`
**Priority:** P0
**Dependencies:** FM-003
**Description:** The main driver. Reads current task state, selects the highest-priority actionable task, and writes a complete, self-contained role prompt to stdout. The prompt tells the AI agent exactly what role to play, what the task requires, what success looks like, and which command to call when done. Pure stdout output — no user interaction.
**Constraints:**
- Accepts optional flag `--prd <path>` (absolute or CWD-relative); if given, reads the file and injects its content verbatim into the prompt as a `## Requirements Context` section
- Priority: tasks with satisfied dependencies in the earliest non-terminal phase come first; within the same phase, lower task number wins (task-001 before task-002)
- If all tasks are in terminal-done state, outputs a completion summary listing all tasks with their IDs, titles, and final artifacts, then exits 0
- If no tasks exist and `--prd` is given, outputs a decomposition prompt instructing the agent to break the PRD into tasks using `agira add`
- If no tasks exist and `--prd` is absent, prints to stdout: `No tasks found. Add tasks with \`agira add "<title>"\` or provide requirements with \`agira next --prd <path>\``
- The prompt must include: task id and title, current phase name and agent role (from config), task description, acceptance criteria if the `phases` object has prior enrichment data, the verification commands if the current phase is a verification phase, and the exact command(s) to call to advance state
- Output contains no ANSI escape sequences (verified by piping through `cat -v`)
- Exits 0 in all non-error cases; exits 1 only on config/file read errors
**Acceptance Criteria:**
- `agira next` with no tasks and no `--prd` prints the no-tasks message and exits 0
- `agira next --prd docs/prd.md` with no tasks prints a decomposition prompt that contains text from the PRD file
- `agira next` with one task in phase 2 prints a prompt naming the agent role configured for phase 2, the task description, and a line containing `agira done task-001 --artifact`
- `agira next` output piped through `cat -v` shows no `^[` sequences (no ANSI codes)

---

### FM-005: `agira done <id> --artifact <evidence>`
**Priority:** P0
**Dependencies:** FM-003
**Description:** Advances a task from its current phase to the next phase in the configured state machine. Records the provided artifact as evidence for the completed phase. When called on the final configured phase, transitions the task to terminal-done state. `--artifact` is required and must be non-empty.
**Constraints:**
- `--artifact` is a required flag; omitting it exits 1 on stderr: `error: --artifact is required`
- Empty string artifact exits 1 on stderr: `error: artifact must not be empty`
- The artifact is stored in `phases[<current_phase>].artifact` and `phases[<current_phase>].completed_at`
- On non-terminal advance, prints to stdout: `task-001 → <next_phase>`
- On terminal-done transition, prints to stdout: `task-001 done ✓`
- If the task does not exist, exits 1: `error: task <id> not found`
- If the task is already in terminal-done or `failed` state, exits 1: `error: task <id> is already <state>`
**Acceptance Criteria:**
- `agira done task-001 --artifact "tests pass"` on a task in a non-final phase advances it to the next phase, stores the artifact, and `agira status` shows the updated state
- `agira done task-001 --artifact "tests pass"` on a task in the final phase transitions it to terminal-done; `agira status` marks it with `✓`
- `agira done task-001` without `--artifact` exits 1 with the error on stderr
- `agira done task-999 --artifact "x"` on a nonexistent task exits 1 with the not-found message on stderr

---

### FM-006: `agira fail <id> --reason <reason>`
**Priority:** P0
**Dependencies:** FM-003
**Description:** Records a failure for a task's current phase. If `retry_count < max_retries`, resets the task back to the first configured phase and increments `retry_count`. If `retry_count >= max_retries`, transitions to terminal `"failed"` state. `--reason` is required and must be non-empty.
**Constraints:**
- `--reason` is a required flag; omitting it exits 1: `error: --reason is required`
- Empty string reason exits 1: `error: reason must not be empty`
- On retry: `retry_count` increments, task state resets to first phase, history entry reason: `"retry <n>/<max>: <reason>"`; prints: `task-001 retrying (<n>/<max>): <reason>`
- On terminal fail: state set to `"failed"`, history entry reason: `"failed (max retries): <reason>"`; prints: `task-001 failed — max retries reached`
- Calling `agira done` or `agira fail` on a terminal-failed task exits 1: `error: task <id> is already failed`
**Acceptance Criteria:**
- `agira fail task-001 --reason "compilation error"` on a task with `retry_count: 0` and `max_retries: 3` resets it to the first phase with `retry_count: 1`; `agira status` shows updated state and count
- `agira fail task-001 --reason "still broken"` on a task with `retry_count: 2` and `max_retries: 3` sets `state` to `"failed"`
- `agira fail task-001` without `--reason` exits 1 with the required error message
- Calling `agira done task-001 --artifact "x"` on a terminal-failed task exits 1 with the already-state error

---

### FM-007: `agira add`
**Priority:** P1
**Dependencies:** FM-003
**Description:** Adds a new task to `tasks.json`. Used by the agent after receiving a decomposition prompt from `agira next --prd`, or by humans adding tasks manually.
**Constraints:**
- Synopsis: `agira add "<title>" [--description "<desc>"] [--prd <module-id>] [--depends-on <id>[,<id>...]]`
- `<title>` is a required positional argument
- IDs are auto-assigned as the next sequential `task-NNN`; if the last task is `task-005`, the new one is `task-006`
- `--depends-on` accepts comma-separated existing task IDs; exits 1 if any listed ID does not exist: `error: unknown dependency: <id>`
- Prints to stdout: `added task-006: <title>`
**Acceptance Criteria:**
- `agira add "Implement login endpoint"` creates a task with the next sequential ID in the first configured phase; `agira status` shows it
- `agira add "Deploy" --depends-on task-001,task-002` creates the task with `dependencies: ["task-001", "task-002"]`
- `agira add "Blocked" --depends-on task-999` exits 1 with: `error: unknown dependency: task-999`
- Running `agira status` after `agira add` shows the new task in the first phase

---

### FM-008: `agira status`
**Priority:** P1
**Dependencies:** FM-003
**Description:** Prints a human-readable table of all tasks with their current state, retry count, and the reason from the most recent history entry. Intended for human inspection.
**Constraints:**
- Columns: `ID`, `Title` (truncated at 40 chars with `…`), `State`, `Retries`, `Last Action`
- Rows sorted by task ID ascending
- Terminal-done tasks show `✓` prefix on state; terminal-failed tasks show `✗` prefix
- If no tasks exist, prints: `No tasks. Run \`agira add\` or \`agira next --prd <path>\` to get started.`
- `--json` flag outputs the raw contents of `tasks.json`
**Acceptance Criteria:**
- `agira status` in a project with 3 tasks prints a table with exactly 3 rows and correct states
- `agira status --json` outputs valid JSON parseable by `jq .`
- `agira status` in a project with no tasks prints the no-tasks message and exits 0

---

### FM-009: Global User Config
**Priority:** P1
**Dependencies:** FM-001
**Description:** User-wide defaults stored in `~/.agira/config.toml`. Provides fallback values for any project that does not specify them in its own `config.json`.
**Constraints:**
- Location: `~/.agira/config.toml`; created with defaults on first run if absent
- Supported keys: `default_max_retries` (integer, default 3), `default_model` (string, default `"sonnet"`)
- Project-level `config.json` values always override global config
- Malformed TOML exits 1: `error: invalid global config at ~/.agira/config.toml: <parse error>`
**Acceptance Criteria:**
- On first run with no existing `~/.agira/config.toml`, the file is created with default values
- Setting `default_max_retries = 5` in `config.toml` causes new projects without an explicit `max_retries` to use 5 as the default
- A malformed `config.toml` causes any agira command to exit 1 with the parse error message

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
- `agira next --help` shows a description for `--prd`
- `agira version` prints `agira 0.1.0` (current version) to stdout and exits 0
- `agira --version` prints the same version string and exits 0
- `agira -V` prints the same version string and exits 0

---

## Changelog
### Round 2 — 2026-06-04
- FM-010: add help descriptions to all commands/flags and add `version` subcommand + `--version` flag

### Round 1 — 2026-06-03
- Full rewrite of PRD for Rust CLI. Replaces prior TypeScript/Bun protocol library.
- Core model: state stored in `~/.agira/<slug>/`, prompts emitted to stdout, agents drive workflow via `agira next` / `agira done` / `agira fail`
- `agira done` requires `--artifact`, `agira fail` requires `--reason`; both are enforced at the CLI layer
- PRD path is a runtime flag (`--prd`), not hardcoded; can be passed on any `agira next` invocation
- `agira init` is interactive-only; no files written to the target repo
- Storage format confirmed as JSON (not SQLite): scale is small, no concurrent writers, human-inspectability is a first-class concern; crash-safety achieved via atomic rename (write to `.tmp`, then `rename()`) rather than WAL
