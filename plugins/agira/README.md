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

## Migration from the Legacy Executor Skill

The legacy tmux executor skill has been removed. It is fully superseded by `agira runner`, which is built into the CLI and auto-starts on `task_added`.

If you previously installed a global hook that launched the old executor, clean it up:

1. Remove the old global `task_added` executor hook:
   ```sh
   agira hook remove --global task_added <hook-id>
   ```
2. Kill any leftover executor sessions:
   ```sh
   tmux ls
   tmux kill-session -t <legacy-executor-session>
   ```

Going forward, `agira runner start` manages the orchestration lifecycle.

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
