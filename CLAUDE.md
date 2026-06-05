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
  - `reqwest` (blocking, rustls-tls) — fetch GitHub releases for `agira update`
  - `toml` — parse `~/.agira/config.toml` global config
  - `libc` — low-level OS calls
- **Dev deps**: `tempfile` — integration tests using real temp dirs

## Project structure

```
agira/
├── src/
│   ├── main.rs          — CLI entry point, subcommand routing, exit codes
│   ├── core/
│   │   ├── advance.rs       — shared phase-advance output helpers
│   │   ├── config.rs        — config.json schema + reader/migration
│   │   ├── global_config.rs — ~/.agira/config.toml reader
│   │   ├── hooks.rs         — lifecycle hook resolution + execution
│   │   ├── pick.rs          — next actionable task selection and prompt formatting
│   │   ├── project.rs       — git root resolution, slug derivation, state dir creation
│   │   └── tasks.rs         — tasks.json schema, atomic read/write, state machine transitions
│   └── commands/
│       ├── init.rs        — `agira init` (bare agent-prompt path + flag-driven write path)
│       ├── add.rs         — `agira task add`
│       ├── block.rs       — `agira task block`
│       ├── unblock.rs     — `agira task unblock`
│       ├── fail.rs        — `agira task fail`
│       ├── hook.rs        — `agira hook` (manage lifecycle hooks)
│       ├── phase.rs       — `agira phase get` / `agira phase update`
│       ├── self_update.rs — `agira update`
│       ├── status.rs      — `agira task status`
│       ├── update.rs      — `agira task update`
│       └── todo.rs        — `agira task todo` (print prompt / advance with --artifact)
├── tests/               — integration tests against real temp dirs
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

No Makefile. The only CI is `.github/workflows/release.yml`, which builds release
binaries for macOS arm64 + Linux amd64 and publishes a GitHub Release on `v*` tags —
it does **not** run tests or lint, so run the verification commands above locally.

## Local run / start

`agira` is a CLI binary, not a server — "starting" it means building and invoking it:

```sh
cargo build              # produces ./target/debug/agira
./target/debug/agira -v  # smoke check → prints e.g. "agira 0.9.0 (aarch64-apple-darwin)"
```

Verified: `cargo build` succeeds and `agira -v` prints the version + build target.
No env vars or credentials are required to run. Runtime state lives in `~/.agira/<slug>/`;
this project's own config is at `~/.agira/agira/config.json`.

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

Phases: `pending → enriching → in_progress → verifying → done` (or `→ failed` from any phase).
`pending` is always the first phase and `done` is always terminal; config may omit either and agira will insert them at startup.

- **pending** — task created, ready for the first real workflow step
- **enriching** — architect elaborates the spec before implementation begins
- **in_progress** — implementer writes the code
- **verifying** — verifier runs checks and acceptance tests
- **done** — task complete

### Common CLI commands

```sh
agira task status              # show task table
agira task status --json       # raw JSON
agira task todo                # print prompt for current task
agira task todo --artifact ... # advance current task with evidence
agira task add "title" --description "..." --prd FM-001 --depends-on task-001,task-002
agira task update task-001 --title "new title"
agira task fail task-001 --reason "..."
agira phase get
agira phase update --add <phase:model> --after <existing>
agira phase update --remove <phase>
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
