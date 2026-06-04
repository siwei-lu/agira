# CLAUDE.md — agira

## Stack

- **Language**: Rust (edition 2024)
- **Binary**: `agira` — a CLI tool, no frontend or server
- **Key dependencies**:
  - `clap` v4 (derive macros) — argument parsing
  - `anyhow` — application-level error propagation
  - `thiserror` — domain error types that callers match on
  - `serde` / `serde_json` — JSON state files
  - `chrono` — ISO 8601 timestamps
  - `sha2` — slug collision hashing
- **Dev deps**: `tempfile` — integration tests using real temp dirs

## Project structure

```
agira/
├── src/
│   ├── main.rs          — CLI entry point, subcommand routing, exit codes
│   ├── project.rs       — git root resolution, slug derivation, state dir creation
│   ├── config.rs        — config.json schema + reader
│   ├── config_phases.rs — `agira config phases` subcommand
│   ├── tasks.rs         — tasks.json schema, atomic read/write, state machine transitions
│   ├── init.rs          — `agira init` (bare agent-prompt path + flag-driven write path)
│   ├── add.rs           — `agira task add`
│   ├── advance.rs       — shared phase-advance logic
│   ├── fail.rs          — `agira task fail`
│   ├── pick.rs          — next actionable task selection
│   ├── status.rs        — `agira task status`
│   ├── update.rs        — `agira task update`
│   ├── work.rs          — `agira task work` (print prompt / advance with --artifact)
│   └── global_config.rs — ~/.agira/config.toml reader
├── docs/
│   ├── prd.md           — requirements; FM-IDs used to tag tasks
│   └── conventions.md   — project taste decisions (error style, output format, etc.)
└── build.rs             — injects BUILD_TARGET env var into binary version string
```

All runtime state is written to `~/.agira/<slug>/` — **never** inside the repo.

## Build, test, and lint commands

```sh
cargo build                  # debug build
cargo build --release        # release build
cargo test                   # run all tests (unit + integration)
cargo fmt                    # format in-place
cargo fmt -- --check         # format check (used in verification)
cargo clippy -- -D warnings  # lint
```

No Makefile or CI config exists; use the above directly.

## Commit conventions

Conventional Commits with optional scope:

```
feat: add new feature
fix: fix a bug
chore: tooling / version bumps / cleanup
feat(fm-002): scoped to a functional module
```

Scopes are typically `fm-NNN` referencing a PRD functional module.

## PRD

Full requirements: `docs/prd.md`

Functional modules (FM-001, FM-002, ...) define acceptance criteria for each feature. Tag tasks with `--prd FM-NNN` when adding them so they trace back to specs.

## Development workflow

### agira state machine

Phases: `enriching → in_progress → verifying → done` (or `→ failed` from any phase)

- **enriching** — architect elaborates the spec before implementation begins
- **in_progress** — implementer writes the code
- **verifying** — verifier runs checks and acceptance tests
- **done** — task complete

### Common CLI commands

```sh
agira task status              # show task table
agira task status --json       # raw JSON
agira task work                # print prompt for current task
agira task work --artifact ... # advance current task with evidence
agira task add "title" --description "..." --prd FM-001 --depends-on task-001,task-002
agira task update task-001 --title "new title"
agira task fail task-001 --reason "..."
agira config phases --add <phase> --after <existing>
agira config phases --remove <phase>
agira -v                       # version
```

**Never edit `~/.agira/agira/tasks.json` directly.** Always use `agira task add` (or other subcommands) so the state machine, history, and IDs are managed correctly.

### Error handling conventions

- `anyhow` for propagation; `thiserror` for matchable domain errors
- User-facing messages: lowercase, no trailing period, actionable
  - Good: `error: not inside a git repository`
  - Bad: `Error: Git repository not found.`
- Exit codes: `0` success, `1` user error, `2` internal/IO error

### CLI output conventions

- Prompts and structured output → stdout
- Error messages → stderr (`eprintln!`)
- No ANSI escape sequences in stdout output

### File I/O conventions

- All JSON writes use atomic rename: write `<file>.tmp`, then `rename()` into place
- JSON is pretty-printed with 2-space indentation

### Slug derivation

Git root directory basename → lowercased → non-alphanumeric chars → `-`. On collision, append `-<first-6-chars-of-sha256-of-absolute-path>`.

## TDD

Tests are written before implementation. Test files are colocated with source (inside `src/`) or in the same module using `#[cfg(test)]`. Do not commit implementation without accompanying tests.
