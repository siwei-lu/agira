# agira

A CLI tool for AI-assisted task management. Tracks tasks through a state machine (`pending → enriching → in_progress → verifying → done`) and orchestrates autonomous Claude workers to execute them.

## Install the CLI

Download the latest release from [GitHub Releases](https://github.com/siwei-lu/agira/releases) or build from source:

```sh
cargo build --release
cp target/release/agira /usr/local/bin/agira
```

## Skills marketplace

The agira repo ships a plugin for both **Claude Code** and **Codex** that adds two skills:

- **`add`** — safely creates an Agira task via the CLI (duplicate check, feasibility review, confirmation before writing)
- **`todo`** — launches or reuses a tmux-backed Claude orchestrator that dispatches tasks until the queue is empty

### Claude Code

Add the marketplace and install the plugin:

```
/plugin marketplace add https://github.com/siwei-lu/agira.git
/plugin install agira@agira
```

### Codex

Add the marketplace and install the plugin:

```
/plugin marketplace add https://github.com/siwei-lu/agira.git
/plugin install agira@agira
```

## Quick start

```sh
agira init                          # initialise project (run once in repo root)
agira task add "My first task" --description "..."
agira task list
agira task todo                     # print the current task prompt
agira task todo --artifact "..."    # advance the task with evidence
```

## CLI reference

```
agira task list                     # show task table
agira task list --json              # raw JSON
agira task todo                     # print prompt for current task
agira task todo --artifact ...      # advance current task with evidence
agira task add "title" --description "..."
agira task update <id> --title "new title"
agira task fail <id> --reason "..."
agira phase get
agira phase update --add <phase:model> --after <existing>
agira -v                            # version
```

## Development

```sh
cargo build          # debug build
cargo test           # run all tests
cargo fmt            # format
cargo clippy -- -D warnings
```

Runtime state is written to `~/.agira/<project-slug>/` — never inside the repo.
