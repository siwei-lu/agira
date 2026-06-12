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

Do NOT rely on any cached or remembered file tree — it goes stale fast. Read the source
directly: `src/main.rs` routes every subcommand, `src/core/` holds shared domain logic,
`src/commands/` holds one module per subcommand. List those directories and read the
relevant modules before making changes. `docs/conventions.md` records project taste
decisions (error style, output format, etc.).

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
./target/debug/agira -v  # smoke check → prints "agira <version> (<build target>)"
```

No env vars or credentials are required to run. Runtime state lives in `~/.agira/<slug>/`;
this project's own config is at `~/.agira/agira/config.json`. Note the globally installed
`agira` binary may lag the repo version — use `./target/debug/agira` to test local changes.

## Commit conventions

Conventional Commits with optional scope:

```
feat: add new feature
fix: fix a bug
chore: tooling / version bumps / cleanup
```

## Development workflow

### agira state machine

`pending` is always the first phase and `done` is always terminal; `failed` is reachable
from any phase. Config may omit `pending`/`done` and agira inserts them at startup.

This repo's own workflow (defined in `~/.agira/agira/config.json` — read it for the
authoritative phase list, models, and duties) is currently:

`pending → enriching → implementing → reviewing → verifying → done`

- **pending** — task created, ready for the first real workflow step
- **enriching** — architect rewrites the description as a complete spec
- **implementing** — implementer writes tests first, then code (TDD)
- **reviewing** — reviewer checks correctness, coverage, and conventions
- **verifying** — verifier runs fmt/test/clippy plus an end-to-end acceptance run
- **done** — task complete

### Common CLI commands

Run `agira --help` (and `agira <command> --help`) for the full, current surface —
top-level commands include `task`, `init`, `phase`, `config`, `hook`, `project`,
`workflow`, `runner`, `skill`, `update`. Most-used:

```sh
agira task list                # task table (latest 20; --json for raw)
agira task inspect task-001    # detailed view of one task
agira task todo                # print prompt for current task
agira task todo --artifact ... # advance current task with evidence
agira task add "title" --description "..." --depends-on task-001,task-002
agira task update task-001 --title "new title"
agira task fail task-001 --reason "..."
agira project list             # all initialized projects
agira workflow list            # named workflows in project config
agira runner start|stop|status # manage the tmux-backed runner
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
