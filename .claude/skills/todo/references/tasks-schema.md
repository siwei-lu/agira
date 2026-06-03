# tasks.json Schema

This is the canonical schema for `.orchestrator/tasks.json`. All agents and hooks must parse this
structure consistently.

## Top-Level Structure

```json
{
  "requirements": {
    "original_input": "The raw input the user provided (ticket number, PRD text, plain text)",
    "input_type": "jira | github | prd | plain_text",
    "clarification_log": [
      { "round": 1, "questions": ["..."], "answers": ["..."] }
    ],
    "final_requirements": "The interpreted requirements after clarification",
    "branch": "feat/PROJ-123-add-dark-mode"
  },
  "created_at": "2026-04-02T10:00:00Z",
  "housekeeper_cycle_count": 0,
  "max_housekeeper_cycles": 3,
  "max_retries": 3,
  "tasks": [
    { /* task object — see below */ }
  ]
}
```

## Task Object

```json
{
  "id": "task-001",
  "title": "Implement user registration endpoint",
  "description": "Create POST /register endpoint with email uniqueness check and JWT response",
  "state": "pending",
  "assignee": "implementer",
  "model": "sonnet",
  "source": "user",
  "prd_module_id": "FM-001",
  "dependencies": [],
  "retry_count": 0,
  "enrichment": {
    "prd_source": "docs/prd.md:FM-001",
    "confidence": "high",
    "prd_refs": ["..."],
    "acceptance_criteria": ["..."],
    "awaiting_confirmation": false,
    "implementation_certainty": "high",
    "impact_scope": 2,
    "needs_checkpoint": false,
    "checkpoint": null,
    "acceptance_steps": [ /* see Acceptance Step Objects section */ ]
  },
  "architecture": { /* see Architecture Object section — null until architecting completes */ },
  "verification_results": [],
  "history": [
    { "from_state": null, "to_state": "pending", "timestamp": "2026-04-02T10:00:00Z", "reason": "Task created" }
  ]
}
```

## Field Definitions

### Task Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | yes | Unique identifier (e.g., `task-001`) |
| `title` | string | yes | Short human-readable title |
| `description` | string | yes | Detailed description of what needs to be done |
| `state` | string | yes | Current state: `pending`, `enriching`, `architecting`, `in_progress`, `verifying`, `done`, `failed` |
| `assignee` | string | yes | Sub-agent role |
| `model` | string | yes | Claude model: `opus`, `sonnet`, `haiku` |
| `source` | string | yes | `"user"` or `"housekeeper"` |
| `prd_module_id` | string | no | FM-ID from PRD (e.g., `"FM-001"`) |
| `dependencies` | string[] | no | Task IDs that must be `done` before this one starts |
| `retry_count` | number | yes | How many times retried |
| `enrichment` | object | yes | PRD context and acceptance criteria |
| `architecture` | object\|null | Implementation spec from architect. `null` until architecting completes. |
| `verification_results` | array | yes | Results from each verification attempt |
| `history` | array | yes | State transition log with timestamps |

### Enrichment Object

| Field | Type | Description |
|-------|------|-------------|
| `prd_source` | string | `"prd.md:FM-001"` (standard schema match) |
| `confidence` | string | `"high"` or `"low"` |
| `prd_refs` | string[] | Verbatim PRD fragments — never paraphrased |
| `acceptance_criteria` | string[] | Verifiable conditions for `done` |
| `awaiting_confirmation` | boolean | `true` when paused for human input |
| `implementation_certainty` | string | `"high"` or `"low"` |
| `impact_scope` | number | Estimated files/modules affected |
| `needs_checkpoint` | boolean | `true` when approach needs human approval |
| `checkpoint` | object\|null | Implementation brief. `null` if `needs_checkpoint: false`. |
| `acceptance_steps` | array | CLI acceptance test steps. **Must be non-empty** (acceptance_testing config exists). |

### Acceptance Step Objects (CLI strategy)

```json
{
  "type": "cli",
  "description": "agira next with no tasks prints the no-tasks message",
  "command": "cargo run -- next",
  "assert": [
    { "condition": "exit_code", "value": 0 },
    { "condition": "stdout_contains", "value": "No tasks found" }
  ],
  "evidence": "terminal_output"
}
```

**Enricher rules:**
1. Every `acceptance_criteria` string → at least one `cli` step
2. Command uses `cargo run --` prefix (binary not yet installed)
3. Each step needs at least one `assert` and `evidence: "terminal_output"`
4. Empty steps = enrichment failure

### Architecture Object

| Field | Type | Description |
|-------|------|-------------|
| `components` | string[] | Files/modules to create or modify |
| `decisions` | array | Every decision: `{question, answer, source}` |
| `dom_structure` | null | N/A for CLI |
| `state_management` | string\|null | How state flows |
| `data_flow` | string\|null | Command → parse → state change → output |
| `notes` | string\|null | Extra context for implementer |

**Decision `source` values:** `"conventions.md"`, `"memory"`, `"user"`, `"architect"` (low-stakes only), `"unresolved"`

### Checkpoint Object

| Field | Type | Description |
|-------|------|-------------|
| `approach` | string | One sentence: technical direction |
| `touch_files` | string[] | Files/modules to be modified |
| `risk` | string\|null | What's uncertain, or `"none"` |
| `status` | string | `"pending"` → `"approved"` or `"revised"` |
| `user_response` | string\|null | Human's direction if revised |

**Trigger:** `needs_checkpoint = true` when `implementation_certainty === "low"` OR `impact_scope >= 3`.

### Verification Result Object

```json
{
  "attempt": 1,
  "timestamp": "2026-04-02T10:05:00Z",
  "overall": true,
  "checks": {
    "fmt": { "passed": true, "output": "" },
    "clippy": { "passed": true, "output": "" },
    "test": { "passed": true, "output": "test result: ok. 5 passed; 0 failed" }
  },
  "code_review": {
    "passed": true,
    "review_score": 3,
    "threshold": 15,
    "finding_counts": { "critical": 0, "high": 0, "medium": 1, "low": 0 },
    "findings": []
  },
  "acceptance": {
    "passed": true,
    "steps_total": 3,
    "steps_passed": 3,
    "results": []
  }
}
```

### History Entry

`{from_state, to_state, timestamp, reason}`. When `to_state === "done"`, include `commit_hash`.

## Invariants

1. `done` task must have ≥1 verification result with `overall: true`
2. `failed` task must have `retry_count >= max_retries` (3)
3. Cannot enter `in_progress` if `enrichment.awaiting_confirmation === true`
4. Cannot enter `in_progress` if `enrichment.checkpoint?.status === "pending"`
5. `in_progress` task must have `enrichment` field present
6. `in_progress` with `needs_checkpoint: true` must have `checkpoint.status === "approved"` or `"revised"`
7. `confidence: high` must not be set when `prd_refs.length === 0`
8. `housekeeper_cycle_count` must never exceed `max_housekeeper_cycles` (3)
9. Tasks with `source: "housekeeper"` created only after planner reviews housekeeper findings
10. State transitions recorded in `history` — no silent changes
11. Enricher modifies only `enrichment`, `state`, and `history` fields
12. `done` transition requires a git commit. Planner executes it — not the implementer.
13. Cannot enter `in_progress` with `architecture: null` or any decision `source: "unresolved"`
