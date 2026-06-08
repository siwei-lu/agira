# agira

Skills for the [Agira](https://github.com/siwei-lu/agira) task management workflow.

## Installation

### Claude Code

```
/plugin marketplace add https://github.com/siwei-lu/agira.git
/plugin install agira@agira
```

### Codex

```
/plugin marketplace add https://github.com/siwei-lu/agira.git
/plugin install agira@agira
```

## Skills

### `add`

Safely adds a task to an Agira project via the CLI. Triggers when you ask to create, add, or register an Agira task. It will:

1. Discover the target workspace
2. Check for duplicate tasks and conflicts
3. Confirm the task details with you before running `agira task add`

### `todo`

Launches or reuses a tmux-backed Claude orchestrator that dispatches Agira tasks until all queued work is done. Triggers when you ask to run, start, or resume an Agira executor.

Requires:
- `tmux` on PATH
- `claude` CLI on PATH
- An initialized Agira project (`agira init` run in the target directory)

The bundled `scripts/ensure_executor.py` handles session lifecycle — it is idempotent and safe to call repeatedly.

## Requirements

- `agira` CLI installed and on PATH
- An initialized Agira project in the target workspace
