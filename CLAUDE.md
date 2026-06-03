# agira

Rust CLI tool that orchestrates AI-assisted software development workflows.

## Commit Convention

Conventional Commits format. Examples:
```
feat(fm-001): implement project resolution
fix(fm-003): atomic write crash safety
chore: update dependencies
test(fm-004): add agira-next output assertions
refactor(fm-002): extract interview logic
```

Pattern: `<type>(<scope>)?: <description>`
Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `perf`, `ci`, `build`, `revert`
Scope: optional, typically the FM module (`fm-001`) or subsystem name.

## Branch Strategy

All work committed directly to `main`. No feature branches.

## Key Conventions

- All error output → stderr; all prompts and structured output → stdout
- Exit codes: 0=success, 1=user error, 2=internal/IO error
- Every JSON write uses atomic rename: write to `<file>.tmp`, then `rename()` into place
- State stored in `~/.agira/<slug>/` — never inside the target repo
- `tasks.json` and `config.json` are pretty-printed with 2-space indent

## Orchestration

This repo uses the todo skill for AI-assisted development. See `.claude/skills/todo/SKILL.md`.
Task state is tracked in `.orchestrator/tasks.json` (gitignored).
