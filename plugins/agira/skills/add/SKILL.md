---
name: add
description: >
  Add a task to an Agira project via the CLI. Use whenever the user asks to add, create, or register
  an Agira task. Discover initialized projects, investigate feasibility, confirm with the user, then
  run `agira task add`.
---

# agira-task-add

You are helping the user add a task to an Agira project safely and correctly.

## Steps

1. **Discover the workspace.** Run `agira project list`. If only one project exists, use it. If
   multiple exist, infer from the current working directory or ask — do not guess.

2. **Investigate before writing anything.**

   a. Run `agira task list --json`. Scan only the `title` and `state` fields. If any non-done
      task has the same or very similar title, stop — flag the duplicate and ask the user whether
      to update the existing task instead (`agira task update <id> --description "..."`).

   b. Find relevant source files by grepping keywords from the task title in `src/`:
      ```
      grep -rl "<keyword>" src/
      ```
      Read the files that come back. Understand what already exists, what's in progress, and what
      would actually need to change.

   c. Assess feasibility and risks: Is the requirement clear enough to implement? Does it conflict
      with existing behavior or open tasks? Are there non-obvious side effects or missing context?

   After investigating, surface everything you found in one message — duplicates, conflicts, unclear
   scope, risks. Ask focused questions (2-3 max). Wait for the user to respond before continuing.

3. **Confirm before adding.** Show the user:
   - Workspace path
   - Task title
   - Full description draft
   - `--depends-on` if your investigation revealed a clear dependency

   Do NOT ask for confirmation about the workflow choice — predefined workflows are all
   human-reviewed and a wrong pick is cheap to change. Workflow selection is resolved
   silently in the next step.

4. **Select the best-fitting workflow.** Run `agira workflow list` to discover available
   named workflows. Based on the task's nature, pick the one that fits best and pass
   `--workflow <name>` to `agira task add`.

   - If nothing fits clearly, omit `--workflow` entirely — the task will fall back to
     `config.default_workflow`.
   - `--phases` (a fully custom phase sequence) is reserved for tasks that genuinely
     cannot fit any named workflow. If you use `--phases`, you must include a brief
     written reason for the deviation in the task description.
   - `--workflow` and `--phases` are mutually exclusive — do not combine them.

5. **Run `agira task add`** once the user approves — always use `run_in_background: true`:
   ```
   agira task add "<title>" --description "<details>"
   ```
   Add `--depends-on`, `--phase`, `--workflow`, `--phases`, or `--duties` only when
   explicitly supplied or clearly implied by your investigation. After the background
   command completes, run `agira task list` to confirm the task was created.

## Rules

- Never edit `~/.agira/<slug>/tasks.json` directly — always use the CLI.
- Never add a task already covered by an open task — suggest `agira task update` instead.
- Do not add tasks to a project the user did not intend.
- **NEVER modify source code or project files.** This skill only reads code to understand scope and draft a description. All edits, file changes, or implementation belong in the task, not here.
- **No confirmation for workflow selection.** Selecting any predefined (named) workflow requires NO user confirmation — they are all human-reviewed, so a wrong pick is cheap to change.
- **Use `--phases` sparingly.** It is reserved for tasks that cannot fit any named workflow. Always include a written reason for the deviation in the task description when using it.
