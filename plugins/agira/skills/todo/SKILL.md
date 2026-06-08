---
name: todo
description: Launch or reuse a tmux-backed Claude executor for an Agira task queue. Use when the user asks to run, start, resume, or ensure an autonomous Agira todo executor for a project, especially requests shaped like "project-slug at /path/to/project" or Agira task-added hook handling. Avoid duplicate executor sessions while making sure stopped or idle executors are restarted.
---

# Agira Todo Executor

## Overview

Ensure an Agira project has one tmux-backed Claude orchestrator dispatching Agira todo work until all queued tasks are finished.

Use the bundled script whenever possible:

```sh
python3 scripts/ensure_executor.py "<project-slug-or-name> at <path-to-project>"
```

## Behavior

1. Parse the project slug/name before the literal separator ` at `.
2. Parse the project path after ` at ` and resolve it to a canonical absolute path.
3. Derive the tmux session name as `agira-executor-<project-slug-or-name>`.
4. Do not double-prefix names that already start with `agira-executor-`.
5. Use the same derived session name for the remote-control session by calling `/remote-control <derived-session>`, for example `/remote-control agira-executor-myproject`.
6. Run every tmux command from the project directory.
7. Check `tmux has-session -t =<derived-session>` and use exact pane targets like `=<derived-session>:` for pane operations so similarly prefixed sessions are not treated as the target session.
8. If the session exists, capture the latest pane output and inspect the current pane command.
9. Determine `workflow_looks_running` from the current pane command plus recent pane output. Do not rely on a visible `/goal active` status line alone; stale goal UI can remain after API errors or an idle prompt.
10. Treat the workflow as running only when the current pane is Claude, the executor goal is present, and the newest meaningful marker in recent output is an active execution marker such as `Pollinating`, `running in the background`, `running command`, `running tool`, waiting for a task completion notification, or an interrupt hint rather than a terminal error or waiting-for-input marker such as `API Error`, `Overloaded`, `rate limit`, or `waiting for input`. Do not treat `1 shell still running` or the Claude prompt/status bar by itself as proof of active work; those markers can remain after an error. If tmux reports a nonstandard current command like a version string, use clear Claude UI markers in the pane as the fallback for identifying a Claude pane.
11. If `workflow_looks_running` is true, do not start or send anything.
12. If the session exists but is idle, completed, errored, or waiting for input, reuse it:
   - if the current command is Claude or the pane content clearly shows the Claude UI, send `/remote-control <derived-session>` as one message, then send the raw executor `/goal` prompt as a second message;
   - if the pane is at a shell prompt, send a shell-quoted `claude --model sonnet` command, wait until the pane is recognizably Claude, then send `/remote-control <derived-session>` followed by the executor `/goal` prompt as two separate messages.
13. If the session does not exist, start a detached tmux session in the project directory running `claude --model sonnet`, wait until the pane is recognizably Claude, then send `/remote-control <derived-session>` followed by the executor `/goal` prompt as two separate messages.

Treat repeated invocations as normal. The operation must be idempotent and must not create duplicate tmux sessions.

## Command Sequence

Submit these as two separate Claude Code messages. Do not combine them into one prompt, because a slash command is recognized at the start of a message and following text is treated as that command's arguments.
For multi-line messages, paste through a tmux buffer and submit with a separate
`Enter`; do not stream the raw prompt with `tmux send-keys`, because embedded
newlines can be interpreted as early submits and leave prompt fragments in the
Claude Code input box.

```text
/remote-control agira-executor-<project-slug-or-name>
```

## Executor Prompt

The executor prompt has a single source of truth at `executor_prompt.md`.
`scripts/ensure_executor.py` reads that file at runtime and sends its contents
as the second Claude Code message after `/remote-control <derived-session>`.
Update `executor_prompt.md` when changing executor behavior; do not duplicate
the full prompt in this skill document or the launcher script.

## Script Usage

Preferred invocation:

```sh
python3 scripts/ensure_executor.py "<slug> at /path/to/project"
```

Alternative explicit invocation:

```sh
python3 scripts/ensure_executor.py --project-name <slug> --project-path /path/to/project
```

Useful checks:

```sh
python3 scripts/ensure_executor.py "<slug> at /path/to/project" --dry-run --json
python3 scripts/ensure_executor.py "<slug> at /path/to/project" --status-only --json
```

The script prints a concise status and exits nonzero for malformed requests, missing paths, missing `tmux`, capture failures, restart failures, or ambiguous non-Claude busy panes.

## Agira Hook

To trigger this executor whenever a task is added, register a global Agira hook that passes the stable Agira slug and canonical project path:

```sh
agira hook add --global task_added 'codex exec --ephemeral "\$agira-todo-executor $AGIRA_PROJECT_SLUG at $AGIRA_PROJECT_PATH"'
```

If the hook already exists, update it instead:

```sh
agira hook update --global task_added 'codex exec --ephemeral "\$agira-todo-executor $AGIRA_PROJECT_SLUG at $AGIRA_PROJECT_PATH"'
```

Current Agira hook events use `task_added`; do not use `task.added`.
Use `agira hook list` to verify the effective hook list; this CLI does not take
`--global` for the list subcommand.

## Constraints

- Do not edit Agira task state files directly, including `~/.agira/.../tasks.json`.
- Do not create or switch to git worktrees.
- Do not run `git worktree add`.
- Do not report executor status that was not observed from the actual commands.
- Prefer the script over hand-written shell commands because the Claude prompt is multiline and contains shell-sensitive text.
