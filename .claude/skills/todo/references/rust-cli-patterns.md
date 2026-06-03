# Rust CLI Patterns — agira

Stack-specific guidance for the architect and implementer sub-agents.

## Crate Recommendations

| Need | Crate | Notes |
|------|-------|-------|
| CLI argument parsing | `clap` (derive feature) | Idiomatic for Rust CLIs; auto-generates help |
| Error handling (app-level) | `anyhow` | `Result<T>` propagation with context |
| Error types (domain) | `thiserror` | When callers need to match on error variants |
| Serialization | `serde` + `serde_json` | JSON read/write for tasks.json and config.json |
| TOML parsing | `toml` | For `~/.agira/config.toml` (FM-009) |
| SHA-256 hashing | `sha2` | For slug collision suffix (FM-001) |

## File I/O Invariant

Every write to a JSON file **must** use atomic rename:

```rust
let tmp_path = path.with_extension("tmp");
let content = serde_json::to_string_pretty(&data)?;
std::fs::write(&tmp_path, content)?;
std::fs::rename(&tmp_path, &path)?;
```

The `rename()` must be in the same filesystem (both paths under `~/.agira/<slug>/`) — this is
guaranteed since `.tmp` is written to the same directory. A process killed mid-write leaves the
previous file intact. **Never use `fs::write(path, content)` directly for state files.**

## Exit Code Contract

Use `std::process::exit()` for all exits except success (which returns from main):

```rust
// User error (bad args, invalid state, not found)
eprintln!("error: {}", msg);
std::process::exit(1);

// Internal/IO error
eprintln!("error: {}", err);
std::process::exit(2);
```

Or define an enum and convert in main:

```rust
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(2);
    }
}
```

## stdout vs stderr

- **stdout:** structured output, prompts, completion messages (`task-001 done ✓`)
- **stderr:** error messages, warnings (`eprintln!`)
- **No ANSI codes in stdout** — output must be clean through `cat -v`

## Project Resolution (FM-001)

```rust
fn find_git_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() { return Some(dir); }
        if !dir.pop() { return None; }
    }
}

fn derive_slug(root: &Path) -> String {
    let name = root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    name.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect()
}
```

## Interactive Terminal Detection (FM-002)

```rust
use std::io::IsTerminal;

if !std::io::stdout().is_terminal() {
    eprintln!("error: agira init requires an interactive terminal");
    std::process::exit(1);
}
```

## ID Auto-Assignment (FM-003)

```rust
fn next_task_id(tasks: &[Task]) -> String {
    let n = tasks.len() + 1;
    format!("task-{:03}", n)
}
```

## Module Structure

Suggested file layout (let architect confirm per-task):

```
src/
  main.rs          — CLI entry point, subcommand dispatch
  commands/
    init.rs        — FM-002
    next.rs        — FM-004
    done.rs        — FM-005
    fail.rs        — FM-006
    add.rs         — FM-007
    status.rs      — FM-008
  project.rs       — FM-001: git root resolution, slug derivation, dir creation
  store.rs         — FM-003: tasks.json read/write, state machine enforcement
  config.rs        — FM-009: global config.toml
  types.rs         — shared types: Task, TaskState, Config, etc.
```

## Testing Approach

- Unit tests colocated in the same file (`#[cfg(test)]` module at bottom of each file)
- Integration tests in `tests/` directory — exercise real subcommands with temp dirs
- Use `tempfile` crate for isolated test directories
- Test the acceptance criteria from the PRD directly: real `cargo run --` invocations where possible
