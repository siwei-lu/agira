# Executor Prompt Audit

Audit of the `/goal` prompt used by the `agira-todo-executor` skill to drive the Claude orchestrator.

---

## Critical

### 1. `outstanding` counter has no floor and no recovery path after context compression

`outstanding -= 1` assumes every `<task-notification>` maps to a worker dispatched in the current session. Two failure modes:

- **Stale notification**: a notification from a prior session or a sub-agent arrives → `outstanding` goes negative → stall detection fires prematurely.
- **Context compression**: the counter value is lost mid-run → executor restarts with `outstanding = 0`, sees a non-advancing tuple, and either re-dispatches or blocks a healthy task.

No floor (`max(0, outstanding - 1)`) and no recovery instruction exist.

**Fix (short-term)**: add `outstanding = max(0, outstanding - 1)` floor; add an explicit instruction for what to do when context is summarized (re-derive state from `agira task todo` output rather than trusting the counter).

**Fix (long-term)**: implement routine A task lock — move lock state into `tasks.json` so `agira task todo` is the single source of truth and no in-context counter is needed (see `docs/` for task spec).

---

### 2. `retried` state granularity is undefined

Gate step 4 requires distinguishing "not yet retried" from "already retried once", but the prompt never specifies the data structure or key. If keyed by bare phase name (e.g. `"in_progress"`), retries from task-001's `in_progress` phase will consume the retry budget for task-002's `in_progress` phase — cross-task pollution.

**Fix**: define `retried` explicitly as `Set<(taskID, phase)>` and reference it by that key in the gate logic.

---

### 3. Stop condition does not distinguish "all done" from "all blocked"

When every remaining task is blocked, `agira task todo` returns "no actionable work" — the same output as the all-done case. The executor stops silently with no indication that tasks are stalled.

**Fix**: before stopping, run `agira task list` and emit a warning if any tasks are in `blocked` state.

---

## Moderate

### 4. Idle boundary is defined by a negative list

> do not poll, tail logs, watch output, or check agent status

Enumerating forbidden actions leaves gaps. Claude may still insert an `agira task list` call or other "harmless" command while waiting, which adds noise and token cost.

**Fix**: replace with one positive constraint: `After dispatching, the only permitted action is responding to a <task-notification>.`

---

### 5. Dispatch command is unspecified

"Dispatch exactly one worker in the background" does not say which tool to use, which model, or what prompt to pass. The executor fills this gap by inference, producing inconsistent behavior across sessions.

**Fix**: add an explicit dispatch template, e.g.:
```
Use the Agent tool with the full prompt from `agira task todo` verbatim. Do not summarize or rewrite it.
```

---

### 6. Multiple notifications per dispatched worker are unhandled

The prompt notes that notifications can arrive from any previously dispatched agent but does not address the case where a single top-level worker's internal sub-agents each fire a notification. If two notifications arrive for one dispatch, `outstanding` reaches -1 before the next dispatch, triggering a false stall.

**Fix**: clarify whether sub-agent exits generate harness-level notifications. If they do, add: `Only the exit of the top-level Agent call you dispatched counts as one notification unit. Decrement outstanding exactly once per top-level dispatch, regardless of how many notifications arrive.`

---

## Minor

### 7. Rationale block is at the end and vulnerable to context truncation

The rationale explains why (taskID, phase) tuple comparison is necessary and why `outstanding` is the only reliable in-flight signal. This reasoning is load-bearing — without it, a compressed context will drop the constraints and Claude will revert to naive phase-only comparison.

**Fix**: inline the rationale as a one-line comment next to each rule rather than collecting it at the end.

---

### 8. Role and Workspace Rules contain overlapping prohibitions

`Never implement, edit files` (Role) and `Do not create or switch to git worktrees` (Workspace Rules) both appear in slightly different forms in two sections. Duplicate rules diverge over time and create ambiguity about which version takes precedence.

**Fix**: merge into one `## Constraints` section with a flat list.

---

## Summary

| Severity | Issue | Risk |
|----------|-------|------|
| Critical | `outstanding` has no floor; lost on context compression | False stall / missed dispatch |
| Critical | `retried` granularity undefined | Cross-task retry pollution |
| Critical | Stop does not distinguish done vs all-blocked | Silent failure |
| Moderate | Idle boundary is a negative list | Spurious polling |
| Moderate | Dispatch command unspecified | Inconsistent execution |
| Moderate | Sub-agent notifications may over-decrement counter | False stall |
| Minor | Rationale at end, truncation risk | Constraints silently dropped |
| Minor | Duplicate prohibitions across sections | Rule divergence |
