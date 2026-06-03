# Conventions — agira

Taste and style decisions that accumulate over time. The architect reads this before proposing
implementation specs. The planner writes new resolutions here so they are never asked twice.

## Error Handling

- Use `anyhow` for application-level errors (propagation, context chaining)
- Use `thiserror` for library/domain error types that callers match on
- User-facing error messages: lowercase, no trailing period, specific enough to act on
  Example: `error: not inside a git repository` (not `Error: Git repository not found.`)

## CLI Output

- Prompts and structured output → stdout
- Error messages → stderr (via `eprintln!` or logging to stderr)
- No ANSI escape sequences in stdout output (verify with `cat -v`)
- Status messages: `task-001 → verifying`, `task-001 done ✓`, `task-001 failed — max retries reached`

## File I/O

- All writes to JSON files use atomic rename: write `<file>.tmp` in same directory, then `rename()`
- Pretty-print JSON with 2-space indentation
- `~/.agira/<slug>/` is the only place state is written — never inside the target repo

## Exit Codes

- 0: success
- 1: user error (bad arguments, invalid state transition, file not found, invalid config)
- 2: internal/IO error (filesystem failure, unexpected state)

## Slug Derivation

- Git root directory basename, lowercased
- Non-alphanumeric characters → `-`
- On collision: append `-<first-6-chars-of-sha256-of-absolute-path>`

## Interaction Patterns

*(empty — will be filled as patterns are established)*

## Copy & Tone

*(empty — will be filled as patterns are established)*
