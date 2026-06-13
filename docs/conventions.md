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

## Hooks

Lifecycle hooks are configured in `~/.agira/<slug>/hooks.toml` for a project or
`~/.agira/config.toml` globally. Valid hook events are:

- `*`: every hook dispatch
- `task_added`: a task was created
- `all_tasks_done`: all known tasks are terminal (`done` or `failed`)
- `failed`: a task transitioned into `failed`
- `blocked`: a task transitioned into `blocked`
- Any configured phase name, such as `pending`, `implementing`, `verifying`, or `done`

`blocked` is a special lifecycle event, like `failed`; it is not a configured workflow phase.
It fires on every transition into the blocked state. `AGIRA_ARTIFACT` contains the exact
`--reason` text passed to `agira task block`, so blocked hooks can forward the questions
or requested human input.

Hook commands run through `sh -c`, wait for the child process to exit, and discard
stdout/stderr unless hook debug logging is enabled. Use fast, non-interactive commands.
Enable debug logging with:

```sh
agira config set hook-debug true
```

Debug entries are written to `~/.agira/<slug>/hook-debug.log`.

Every hook receives these environment variables:

- `AGIRA_TASK_ID`: task ID, for example `task-001`
- `AGIRA_TASK_TITLE`: task title
- `AGIRA_TASK_DESCRIPTION`: task description
- `AGIRA_TASK_STATE`: current task state after the lifecycle event
- `AGIRA_TASK_DEPENDENCIES`: comma-separated dependency IDs
- `AGIRA_TASK_RETRY_COUNT`: current retry count
- `AGIRA_TASK_MAX_RETRIES`: configured maximum retries for the task
- `AGIRA_TASK_CREATED_AT`: RFC3339 creation timestamp
- `AGIRA_PROJECT_SLUG`: lowercased git-root basename
- `AGIRA_PROJECT_PATH`: canonical git root path
- `AGIRA_FROM_PHASE`: phase the task is leaving; empty for `task_added`
- `AGIRA_TO_PHASE`: phase or event target; `blocked` for blocked hooks
- `AGIRA_ARTIFACT`: transition artifact text; for `blocked`, the block reason/questions

Quote hook commands defensively. `AGIRA_ARTIFACT` may contain spaces, quotes, or newlines.
Prefer passing values as quoted command arguments or request bodies instead of interpolating
them into a language expression.

macOS local notification:

```toml
[[hooks]]
on = "blocked"
run = "osascript -e 'on run argv' -e 'display notification (item 3 of argv) with title (\"Agira blocked: \" & item 1 of argv) subtitle (item 2 of argv)' -e 'end run' \"$AGIRA_TASK_ID\" \"$AGIRA_TASK_TITLE\" \"$AGIRA_ARTIFACT\""
```

```sh
agira hook add blocked 'osascript -e '\''on run argv'\'' -e '\''display notification (item 3 of argv) with title ("Agira blocked: " & item 1 of argv) subtitle (item 2 of argv)'\'' -e '\''end run'\'' "$AGIRA_TASK_ID" "$AGIRA_TASK_TITLE" "$AGIRA_ARTIFACT"'
```

ntfy.sh push notification:

```toml
[[hooks]]
on = "blocked"
run = "printf '%s\\n\\n%s' \"$AGIRA_TASK_TITLE\" \"$AGIRA_ARTIFACT\" | curl -fsS -X POST -H \"Title: Agira blocked: $AGIRA_TASK_ID\" -H \"Tags: warning\" --data-binary @- https://ntfy.sh/YOUR_TOPIC"
```

```sh
agira hook add blocked 'printf '\''%s\n\n%s'\'' "$AGIRA_TASK_TITLE" "$AGIRA_ARTIFACT" | curl -fsS -X POST -H "Title: Agira blocked: $AGIRA_TASK_ID" -H "Tags: warning" --data-binary @- https://ntfy.sh/YOUR_TOPIC'
```

## Copy & Tone

*(empty — will be filled as patterns are established)*
