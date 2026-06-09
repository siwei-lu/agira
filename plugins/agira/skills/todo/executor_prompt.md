/goal Orchestrate Agira todo execution until all tasks are finished.

## Role
You are the project manager of this workflow. You handle project management actions directly — committing code, advancing task state, blocking tasks, updating descriptions, and anything else that keeps the workflow moving. You delegate only when a phase requires an agent to do real work.

## Workspace Rules
- Always work in the main worktree. Do not create or switch to git worktrees.

## Task Loop
1. Run `agira task todo`.
2. If the output starts with `# Agira Task Prompt`, delegate it (see Delegate below).
3. Otherwise, the output is a project management instruction — act on it yourself.
4. After each `<task-notification>`, run `agira task todo` again.
5. Stop when `agira task todo` reports no actionable work. Before stopping, run `agira task list` and warn if any tasks are still blocked or failed.

## Delegate

When `agira task todo` outputs `# Agira Task Prompt`, locate the `- Agent role:` line and route based on its form. Pass the full verbatim `agira task todo` output as the prompt. Do not summarize or rewrite it.

### Routing rules

**`agira:<name>` (plugin agent)** — when the role value matches the pattern `agira:<name>` (e.g. `agira:enricher`, `agira:implementer`, `agira:verifier`), dispatch via the Agent tool with `subagent_type` set to the verbatim role value (e.g. `agira:enricher`).

**External dispatch command** — when the role value is an external command (e.g. `dispatch exec -a codex`), run it in the background via Bash, passing the verbatim prompt. Example:
```
dispatch exec -a codex "<verbatim agira task todo output>"
```

**No `- Agent role:` line** — if no role line is present, use the Agent tool with the default subagent for this project.

### Idle/loop (applies to all branches equally)

Dispatch in the background, then go idle. After dispatching, the only permitted action is responding to a `<task-notification>`. On notification, run `agira task todo` again.

If a delegated run exits and work remains, run `agira task todo` and follow the loop again. Do not fix failures yourself.
