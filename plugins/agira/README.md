# agira

Skills for the [Agira](https://github.com/siwei-lu/agira) task management workflow.

## Installation

### Claude Code

```
/plugin marketplace add https://github.com/siwei-lu/agira.git
/plugin install agira@agira
```

### Codex

Install via the Codex plugin settings — add this marketplace URL:

```
https://github.com/siwei-lu/agira.git
```

Then install the **agira** plugin.

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

## Agents

The plugin ships three predefined sub-agent role definitions in `plugins/agira/agents/`. Once the plugin is installed they become namespaced subagent types usable from any Claude Code context.

| Agent | Namespaced type | Tool scope |
|---|---|---|
| `enricher.md` | `agira:enricher` | Read, WebSearch, WebFetch, Bash |
| `implementer.md` | `agira:implementer` | Read, Write, Edit, Bash |
| `verifier.md` | `agira:verifier` | Read, Bash |

Each agent covers one standard Agira workflow phase:

- **`agira:enricher`** — researches context and rewrites rough task descriptions into complete, unambiguous specs before implementation begins.
- **`agira:implementer`** — implements the task following TDD, commits the result, and advances with a commit hash as evidence.
- **`agira:verifier`** — runs all checks and acceptance tests, advances on pass, or blocks the task on failure. Read-only; never modifies code.

### Workspace override

Place a same-named file in your project's `.claude/agents/` directory to override the plugin default for that workspace:

```
.claude/
  agents/
    enricher.md      ← overrides agira:enricher for this project
    implementer.md   ← overrides agira:implementer for this project
    verifier.md      ← overrides agira:verifier for this project
```

The workspace file takes precedence over the plugin-bundled definition. Only the files you place override the defaults; any file you omit continues to use the plugin version.

## Requirements

- `agira` CLI installed and on PATH
- An initialized Agira project in the target workspace
