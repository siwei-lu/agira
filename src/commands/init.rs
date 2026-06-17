use std::{
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;
use thiserror::Error;

use crate::core::{
    config::{Config, PhaseDef, normalize_palette_and_sequence},
    project::Project,
};

#[derive(Debug, Error)]
pub enum InitError {
    #[error(
        "agira init requires all flags or none; missing: {}",
        missing.join(" ")
    )]
    MissingFlags { missing: Vec<String> },

    #[error(
        "invalid phases: use comma-separated phase names or phase:model pairs (e.g. enriching,in_progress:codex); phase names and model labels must be non-empty"
    )]
    InvalidPhases,

    #[error("failed to serialize config")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to write {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Default)]
pub struct InitFlags {
    pub stack: Option<String>,
    pub phases: Option<String>,
}

struct ScanResult {
    config: Config,
    commands: Vec<String>,
}

const CANONICAL_GATE_COMMAND: &str =
    "cargo fmt -- --check && cargo test && cargo clippy -- -D warnings";

pub fn run_init(project: &Project, flags: InitFlags) -> Result<(), InitError> {
    let missing = detect_missing_flags(&flags);

    if !missing.is_empty() {
        return Err(InitError::MissingFlags { missing });
    }

    if !has_required_flags(&flags) {
        let defaults = scan_project(&project.git_root, project.global_config.default_max_retries);
        print!(
            "{}",
            bare_invocation_prompt(&defaults.config, &defaults.commands)
        );
        return Ok(());
    }

    let stack = flags.stack.as_deref().unwrap_or_default().trim();
    if stack.is_empty() {
        return Err(InitError::MissingFlags {
            missing: vec!["--stack".to_owned()],
        });
    }

    let config = Config::new_single_workflow(
        stack,
        parse_phases_flag(flags.phases.as_deref().unwrap_or_default())?,
        project.global_config.default_max_retries,
    );

    let config_path = project.state_dir.join("config.json");
    write_config(&config_path, &config)
}

fn scan_project(git_root: &Path, max_retries: u32) -> ScanResult {
    if git_root.join("Cargo.toml").exists() {
        return scan_result_for_stack(
            "rust",
            vec![
                "cargo fmt -- --check".to_owned(),
                "cargo test".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
            ],
            max_retries,
        );
    }

    if git_root.join("package.json").exists() {
        return scan_package_json_project(git_root, max_retries);
    }

    if git_root.join("go.mod").exists() {
        return scan_result_for_stack(
            "go",
            vec![
                "test -z \"$(gofmt -l .)\"".to_owned(),
                "go vet ./...".to_owned(),
                "go test ./...".to_owned(),
            ],
            max_retries,
        );
    }

    if git_root.join("pyproject.toml").exists() {
        return scan_result_for_stack(
            "python",
            vec![
                "python -m ruff format --check .".to_owned(),
                "python -m ruff check .".to_owned(),
                "python -m pytest".to_owned(),
            ],
            max_retries,
        );
    }

    if git_root.join("pom.xml").exists() {
        return scan_result_for_stack("java", vec!["mvn verify".to_owned()], max_retries);
    }

    if git_root.join("build.gradle").exists() || git_root.join("build.gradle.kts").exists() {
        return scan_result_for_stack("java", vec!["./gradlew test".to_owned()], max_retries);
    }

    if git_root.join("pubspec.yaml").exists() {
        return scan_pubspec_project(git_root, max_retries);
    }

    scan_result_for_stack("unknown", Vec::new(), max_retries)
}

fn scan_package_json_project(git_root: &Path, max_retries: u32) -> ScanResult {
    let package_json = read_package_json(&git_root.join("package.json"));
    let package_manager = detect_package_manager(git_root, package_json.as_ref());
    let stack = if is_typescript_project(git_root, package_json.as_ref()) {
        "typescript"
    } else {
        "javascript"
    };
    let commands = package_commands(package_manager, package_json.as_ref());

    scan_result_for_stack(stack, commands, max_retries)
}

fn scan_pubspec_project(git_root: &Path, max_retries: u32) -> ScanResult {
    let contents = fs::read_to_string(git_root.join("pubspec.yaml")).unwrap_or_default();

    if contents.contains("flutter:") || contents.contains("sdk: flutter") {
        scan_result_for_stack(
            "flutter",
            vec![
                "dart format --output=none --set-exit-if-changed .".to_owned(),
                "flutter analyze".to_owned(),
                "flutter test".to_owned(),
            ],
            max_retries,
        )
    } else {
        scan_result_for_stack(
            "dart",
            vec![
                "dart format --output=none --set-exit-if-changed .".to_owned(),
                "dart analyze".to_owned(),
                "dart test".to_owned(),
            ],
            max_retries,
        )
    }
}

fn scan_result_for_stack(stack: &str, commands: Vec<String>, max_retries: u32) -> ScanResult {
    ScanResult {
        config: config_for_stack(stack, max_retries),
        commands,
    }
}

fn config_for_stack(stack: &str, max_retries: u32) -> Config {
    Config::new_single_workflow(stack, default_phases(), max_retries)
}

fn default_phases() -> Vec<(String, PhaseDef)> {
    vec![
        (
            "in_progress".to_owned(),
            PhaseDef {
                model: Some("sonnet".to_owned()),
                duty: None,
                gate: None,
            },
        ),
        (
            "accepting".to_owned(),
            PhaseDef {
                model: Some("sonnet".to_owned()),
                duty: None,
                gate: None,
            },
        ),
    ]
}

fn parse_phases_flag(input: &str) -> Result<Vec<(String, PhaseDef)>, InitError> {
    let phases: Result<Vec<(String, PhaseDef)>, InitError> = input
        .split(',')
        .map(str::trim)
        .map(|pair| {
            let pair = pair.trim();
            if pair.is_empty() {
                return Err(InitError::InvalidPhases);
            }
            match pair.split_once(':') {
                Some((name, model)) => {
                    let name = name.trim();
                    let model = model.trim();
                    if name.is_empty() || model.is_empty() {
                        Err(InitError::InvalidPhases)
                    } else {
                        Ok((
                            name.to_owned(),
                            PhaseDef {
                                model: Some(model.to_owned()),
                                duty: None,
                                gate: None,
                            },
                        ))
                    }
                }
                None => Ok((
                    pair.to_owned(),
                    PhaseDef {
                        model: None,
                        duty: None,
                        gate: None,
                    },
                )),
            }
        })
        .collect();

    let phases = phases?;
    if phases.is_empty() {
        return Err(InitError::InvalidPhases);
    }
    let (palette, sequence) = normalize_palette_and_sequence(phases, Vec::new());
    Ok(sequence
        .into_iter()
        .filter_map(|name| palette.get(&name).cloned().map(|def| (name, def)))
        .filter(|(name, _)| name != "pending" && name != "done")
        .collect())
}

fn write_config(path: &Path, config: &Config) -> Result<(), InitError> {
    let bytes = serde_json::to_vec_pretty(config).map_err(InitError::Serialize)?;
    let temporary_path = path.with_extension("json.tmp");

    fs::write(&temporary_path, bytes).map_err(|source| InitError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    fs::rename(&temporary_path, path).map_err(|source| InitError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

fn read_package_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
}

#[derive(Clone, Copy)]
enum PackageManager {
    Bun,
    Pnpm,
    Yarn,
    Npm,
}

fn detect_package_manager(git_root: &Path, package_json: Option<&Value>) -> PackageManager {
    if let Some(package_manager) = package_json
        .and_then(|value| value.get("packageManager"))
        .and_then(Value::as_str)
        .and_then(package_manager_from_field)
    {
        return package_manager;
    }

    if git_root.join("bun.lockb").exists() {
        PackageManager::Bun
    } else if git_root.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if git_root.join("yarn.lock").exists() {
        PackageManager::Yarn
    } else {
        PackageManager::Npm
    }
}

fn package_manager_from_field(value: &str) -> Option<PackageManager> {
    let name = value.split('@').next().unwrap_or(value);

    match name {
        "bun" => Some(PackageManager::Bun),
        "pnpm" => Some(PackageManager::Pnpm),
        "yarn" => Some(PackageManager::Yarn),
        "npm" => Some(PackageManager::Npm),
        _ => None,
    }
}

fn is_typescript_project(git_root: &Path, package_json: Option<&Value>) -> bool {
    git_root.join("tsconfig.json").exists()
        || package_json
            .and_then(|value| value.get("devDependencies"))
            .and_then(Value::as_object)
            .is_some_and(|dependencies| dependencies.contains_key("typescript"))
}

fn package_commands(package_manager: PackageManager, package_json: Option<&Value>) -> Vec<String> {
    let scripts = package_json
        .and_then(|value| value.get("scripts"))
        .and_then(Value::as_object);

    let commands: Vec<String> = ["test", "lint", "format"]
        .iter()
        .filter(|script| scripts.is_some_and(|scripts| scripts.contains_key(**script)))
        .map(|script| package_script_command(package_manager, script))
        .collect();

    if commands.is_empty() {
        default_package_commands(package_manager)
    } else {
        commands
    }
}

fn package_script_command(package_manager: PackageManager, script: &str) -> String {
    match package_manager {
        PackageManager::Bun => format!("bun run {script}"),
        PackageManager::Pnpm => format!("pnpm {script}"),
        PackageManager::Yarn => format!("yarn {script}"),
        PackageManager::Npm if script == "test" => "npm test".to_owned(),
        PackageManager::Npm => format!("npm run {script}"),
    }
}

fn default_package_commands(package_manager: PackageManager) -> Vec<String> {
    match package_manager {
        PackageManager::Bun => vec![
            "bun test".to_owned(),
            "bunx eslint .".to_owned(),
            "bunx prettier --check .".to_owned(),
        ],
        PackageManager::Pnpm => vec![
            "pnpm test".to_owned(),
            "pnpm lint".to_owned(),
            "pnpm format".to_owned(),
        ],
        PackageManager::Yarn => vec![
            "yarn test".to_owned(),
            "yarn lint".to_owned(),
            "yarn format".to_owned(),
        ],
        PackageManager::Npm => vec![
            "npm test".to_owned(),
            "npm run lint".to_owned(),
            "npm run format".to_owned(),
        ],
    }
}

fn bare_invocation_prompt(defaults: &Config, commands: &[String]) -> String {
    let mut prompt = String::from(
        r#"# agira init — agent setup required

`agira init` was called without flags. Your job: scan this repo, prove the project can
start locally, reason about what you find, recommend configuration, interview the user,
then call `agira init` with all flags filled in.

"#,
    );
    prompt.push_str(&auto_detected_defaults_block(defaults, commands));
    prompt.push_str(r#"## Step 1 — Scan the repo

Read and record findings from each of the following before asking the user anything:

1. **Stack markers** — check the repo root for: `Cargo.toml`, `package.json`, `pom.xml`,
   `build.gradle`, `build.gradle.kts`, `go.mod`, `pyproject.toml`, `pubspec.yaml`.
   If `package.json` exists, read its content: detect TypeScript (tsconfig.json or `typescript`
   in devDependencies), package manager (`packageManager` field or lockfiles: `bun.lockb`,
   `pnpm-lock.yaml`, `yarn.lock`), frontend frameworks (next, vite, react, vue, svelte, angular),
   backend frameworks (express, fastify, koa, hapi, nest).

2. **Build / test / lint commands** — read `package.json` scripts section, any `Makefile`,
   CI configs (`.github/workflows/`, `.gitlab-ci.yml`). Record the exact commands.

3. **Run / start instructions** — SCAN ONLY here; you execute these in Step 2, not yet.
   Check README files, docs, `package.json` scripts (`dev`, `start`, `serve`), `Makefile`,
   `Cargo.toml`, `pyproject.toml`, Docker Compose files, Procfiles, or framework config.
   Record documented start commands, expected ports or URLs, and any env setup.

4. **Project structure** — list top-level directories and key source files to understand
   whether this is a CLI, library, API, full-stack app, or something else.

5. **Existing AI config** — read `CLAUDE.md`, `.claude/settings.json` if present.

6. **Commit conventions** — run `git log --no-merges -10 --format="%s"` and note the pattern
   (Conventional Commits, Angular, freeform, etc.).

7. **Observable runtime behavior** — check for end-to-end or acceptance test infrastructure:
   `playwright.config.*`, `cypress.config.*`, `jest.e2e.*`, `e2e/`, `cypress/`, `playwright/`,
   `tests/e2e/`, `tests/acceptance/`, `docker-compose*.yml` used for tests.
   Also flag the presence of a UI framework (next, react, vue, svelte, angular) or HTTP server
   (express, fastify, koa, hapi, nest, or any framework that binds a port).
   Record whether the project has an observable runtime behavior layer beyond unit tests
   (UI rendering, API endpoints, CLI output, e2e suite). This finding feeds directly into
   the `accepting` phase decision in Step 3.

## Step 2 — Prove the project starts (REQUIRED)

After scanning and before recommending flags, you MUST make a real local start/run attempt.
This step is required, not optional. This is where you EXECUTE the start, using what Step 1.3 found.
Do not proceed to `agira init` until the project starts successfully, unless you have a concrete blocker that requires user input.

1. Choose the most appropriate run command from the instructions you found.
2. Install missing dependencies using the repo's declared package manager or build tool.
3. Resolve environment issues when possible: read `.env.example` or docs, set documented
   non-secret defaults, choose a free supported port, and fix missing generated files.
   Do not invent secrets; ask the user if a real credential is required.
4. Branch by project type:
   - CLI/library → build, then run a smoke invocation (e.g. `--help` or a sample command) and capture output.
   - server/UI/API → start the process, hit the health endpoint or render a page, capture the listening URL/response, then stop the process.
5. Confirm success with concrete evidence: listening URL, health endpoint, rendered page,
   CLI output, or equivalent smoke check.

Record the start command, required env setup, port or URL, and verification evidence.
These findings must feed into the implementing-phase gate and `CLAUDE.md`.

## Step 3 — Reason and recommend

After scanning and Step 2 proof, reason about each flag. Propose concrete options ranked by
fit, with a brief justification.

**`--stack`**
Name the detected language and primary framework. If the repo is a monorepo or has multiple
stacks, ask which part agira is being set up for before continuing.

**`--phases`**
**`pending` and `done` are built-in phases that are automatically present: agira inserts `pending` first and `done` last. Do not include them in the --phases flag value; configure only the workflow phases between them.**

Reason about project complexity to propose only the middle of the state machine. Each phase may carry a freeform agent/model label.
Model labels are optional. When present, they are arbitrary non-empty text (whitespace allowed), such as `opus`, `sonnet`, `haiku`, `codex`, `dispatch -a codex`, or a project-specific executor label.
A bare phase name is valid when the phase should be model-less in config.
- Design / enrichment phases → `opus` or another reasoning-heavy agent label
- Implementation phases → `sonnet`, `codex`, or another code execution label
- Independent review / acceptance phases → `sonnet`, `codex`, or another code-aware label

Phase selection rules:
- Each middle phase is one dedicated subagent invocation.
- Add a phase only when that step genuinely needs its own focused context.
- Prefer fewer phases because each handoff has cost.

**Deterministic checks (lint / format / type-check / unit tests) are enforced as a GATE on
the implementing phase (`in_progress`), NOT as a separate agent phase.** A gate runs
automatically before the phase is allowed to record its artifact and advance; it is
unfakeable and needs no agent. Place it on the code-producing phase so the actor who can
fix a failure is the one blocked.

`accepting` is the agent phase for behavioral verification — it starts the app, exercises it
from outside (browser, HTTP client, CLI invocation, e2e suite), and confirms that observable
behavior matches the spec. It requires a running process and cannot be replaced by a gate.

**Rule:** add `accepting` whenever the project has observable runtime behavior — UI, API
endpoints, CLI output, or an e2e suite. Omit `accepting` only for pure libraries or utilities
where unit tests ARE the complete acceptance criterion and there is no runtime to start.

Present the full resulting state machine to the user, including built-ins, in arrow form:
`pending -> [chosen phases] -> done`

Lead with the two primary options below (1 and 2) and their trade-off; offer the
variants only where they apply. Format: `phase[:model],phase[:model],...`

- **Option 1 (recommended, lean):** `in_progress:sonnet,accepting:sonnet` — implementing agent
  guarded by a deterministic gate, then a behavioral acceptance agent. Suitable for most projects.
- **Option 2 (alternative, spec-heavy):** `enriching:opus,in_progress:sonnet,accepting:sonnet`
  — adds an upfront enriching phase to turn a rough task into a complete spec before any code
  is written. Use when tasks arrive underspecified and a clarification step saves rework.
- **Pure-library variant (no runtime):** `in_progress:codex` with a gate; skip `accepting`
  because unit tests are the complete acceptance criterion and there is no observable runtime.
- **Prototype with explicit model:** `in_progress:sonnet`
- **Prototype using a configured default model:** `in_progress`

**Phase duties**
Each middle phase should carry a `duty` paragraph that states what the subagent does and what evidence or artifact it must produce. Duties are not set in `agira init`; after init, set them with:

```sh
agira phase update --set-duty <phase> "<text>"
```

For an enriching phase, a good default duty is: "Rewrite the task description as a complete spec with sections: ## Goal, ## Acceptance Criteria, ## Constraints. Persist with agira task update <id> --description \"...\", then advance."

For an accepting phase, choose the template that matches what Step 1.7 found:
- **e2e suite present**: "Run the e2e suite against a locally running instance. All scenarios must pass. Capture the test report and advance."
- **frontend UI**: "Start the app, open the feature in a browser, exercise the key user flows, capture screenshots proving the observable behavior matches the spec. Advance when all flows pass."
- **API/HTTP server**: "Start the server, send representative HTTP requests to the new endpoint(s), verify response status codes and payloads match the spec. Capture the request/response pairs and advance."
- **CLI binary**: "Build the binary and run it with representative inputs. Capture stdout/stderr and exit codes and confirm they match the spec. Advance when all cases pass."

Do not set duties on `pending` or `done`; they are mandatory phases and reject duties.

**Implementing-phase gate commands**
From your scan and required start proof, propose exact deterministic commands to use as the
gate on `in_progress`. Prefer commands found in CI or Makefile over anything you infer.
If a reliable check requires multiple shell operations, prefer an existing single project
script or Makefile target. Join multiple commands with `&&` so the gate fails fast.
Canonical example for Rust projects: `"#,
    );
    prompt.push_str(CANONICAL_GATE_COMMAND);
    prompt.push_str(
        r#"`

## Step 4 — Interview the user

Present ALL findings and recommendations in ONE message. Ask the user only where a decision is genuinely ambiguous (multiple reasonable options); confirm unambiguous values inline without a question.

Include the scan findings that matter, the required start proof, the recommended flag values,
the full state machine in `pending -> ... -> done` form, and 2–3 concrete alternatives only
where the choice is not obvious. Ask for decisions with specific options, not an open-ended
"what would you like?"

Ask how they verify a feature is truly complete for this project: screenshots for frontend,
API request/response for backend, stdout/stderr for CLI, or other proof of correctness.
Ask what evidence each phase should deliver, then draft a suitable `duty` paragraph per
middle phase for the user to confirm.

If Step 1.7 found observable runtime behavior (UI, API endpoints, CLI output, or an e2e suite),
propose `accepting` as the **default recommendation** — not as an implicit option. Explain
clearly why: the project has a runtime that can be exercised from outside, and a gate alone
cannot confirm observable behavior. The user may override, but the default must include it.

## Step 5 — Run agira init

Once all values are confirmed, call:

```sh
agira init \
  --stack <stack> \
  --phases <phase[:model],...>
```

## Step 6 — Set phase duties and the implementing gate

After running `agira init`, run one command per meaningful middle phase to set its duty:

```sh
agira phase update --set-duty <phase> "<draft duty>"
```

Use only duties the user confirmed. Do not run this for `pending` or `done`.

If the workflow includes an `enriching` phase (Option 2 / spec-heavy alternative), set its
duty using the canonical enriching template: "Rewrite the task description as a complete
spec with sections: ## Goal, ## Acceptance Criteria, ## Constraints. Persist with
`agira task update <id> --description \"...\"`, then advance."

**Set the deterministic gate on the implementing phase.** This is the most important step:
it enforces lint, format, type-check, and unit tests automatically before the implementing
agent can record any artifact. Run:

```sh
agira phase update in_progress --set-gate "<deterministic check commands joined by &&>"
```

Derive the actual gate commands from your scan findings (CI, Makefile, project scripts).
Use the same commands you identified as the implementing-phase gate commands in Step 3.

## Step 7 — Write CLAUDE.md

After running `agira init`, update `CLAUDE.md` in the repo root so future AI agents can work
in this repo with minimal rediscovery.

Preserve all human-authored sections verbatim. Use scan findings ONLY to fill MISSING sections; never overwrite or rewrite existing content. If a required section already exists, leave it as-is.
Keep CLAUDE.md concise — one line per point; no prose paragraphs where a bullet suffices.

**If CLAUDE.md does not exist:** create it from scratch.

**If CLAUDE.md exists:** read its current contents first, then add only missing required
sections or missing bullets without rewriting existing content.

The CLAUDE.md must cover all of these at minimum:

- **Stack** — language, primary framework, key libraries
- **Project structure** — what the top-level directories contain; where the main source tree lives
- **Build, test, and lint commands** — exact commands, ready to copy-paste
- **Local run/start** — exact start command, required env setup, port or URL, and proof it worked
- **Commit conventions** — pattern from `git log`; omit if no consistent pattern was found
- **Development workflow** — any conventions captured in existing config (current CLAUDE.md,
  `.claude/settings.json`, CI files, Makefile, etc.)
"#,
    );
    prompt
}

fn auto_detected_defaults_block(defaults: &Config, commands: &[String]) -> String {
    let mut block = String::new();

    if defaults.stack != "unknown" {
        let commands = if commands.is_empty() {
            "none found, derive from CI/scripts".to_owned()
        } else {
            commands.join(" && ")
        };

        write!(
            &mut block,
            r#"## agira's auto-detected defaults

**agira auto-detected these defaults; verify them, don't blindly trust them. Treat them as a starting point for Step 3 recommendations.**

- `stack={stack}`
- suggested implementing-phase gate commands: `{commands}`

"#,
            stack = defaults.stack,
            commands = commands
        )
        .expect("writing to String cannot fail");
    } else {
        block.push_str(
            r#"## agira's auto-detected defaults

**agira could not auto-detect reliable defaults for this repo. Investigate from the markers below and derive verification commands from CI/scripts.**

"#,
        );
    }

    block
}

fn detect_missing_flags(flags: &InitFlags) -> Vec<String> {
    let required = [
        ("--stack", flags.stack.as_ref()),
        ("--phases", flags.phases.as_ref()),
    ];

    if required.iter().all(|(_, value)| value.is_none())
        || required.iter().all(|(_, value)| value.is_some())
    {
        Vec::new()
    } else {
        required
            .iter()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| (*name).to_owned())
            .collect()
    }
}

fn has_required_flags(flags: &InitFlags) -> bool {
    flags.stack.is_some() && flags.phases.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance criteria for task-146: init prompt teaching model
    #[test]
    fn init_prompt_sets_gate_on_implementing_phase() {
        let defaults = scan_result_for_stack(
            "rust",
            vec![
                "cargo fmt -- --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo test".to_owned(),
            ],
            2,
        );
        let prompt = bare_invocation_prompt(&defaults.config, &defaults.commands);
        // (a) must instruct setting a gate on the implementing / in_progress phase
        assert!(
            prompt.contains("in_progress --set-gate"),
            "prompt must contain 'in_progress --set-gate', got:\n{prompt}"
        );
    }

    #[test]
    fn init_prompt_presents_enriching_as_alternative_not_recommended_default() {
        let defaults = scan_result_for_stack("rust", vec![], 2);
        let prompt = bare_invocation_prompt(&defaults.config, &defaults.commands);
        // (b) enriching must appear as an alternative option, not the default
        // The prompt should describe it as "Option 2" or "alternative"
        let lower = prompt.to_lowercase();
        assert!(
            lower.contains("option 2") || lower.contains("alternative"),
            "prompt must present enriching as an alternative (Option 2), got:\n{prompt}"
        );
        // enriching:opus must appear in an example that is NOT the first/recommended option
        assert!(
            prompt.contains("enriching"),
            "prompt must mention 'enriching', got:\n{prompt}"
        );
    }

    #[test]
    fn init_prompt_does_not_recommend_standalone_verifying_agent_phase() {
        let defaults = scan_result_for_stack("rust", vec![], 2);
        let prompt = bare_invocation_prompt(&defaults.config, &defaults.commands);
        // (c) must NOT recommend a standalone verifying agent phase
        // The recommended/Option 1 example must not contain "verifying"
        // We look for the Option 1 example line and ensure it doesn't include verifying
        assert!(
            !prompt.contains("verifying:haiku"),
            "prompt must not recommend 'verifying:haiku' phase, got:\n{prompt}"
        );
        // The phrase "verifying vs accepting" block should not appear
        assert!(
            !prompt.contains("verifying` vs `accepting"),
            "prompt must not contain old 'verifying vs accepting' block, got:\n{prompt}"
        );
    }

    #[test]
    fn default_phases_seed_only_in_progress_and_accepting() {
        let phases = default_phases();

        assert_eq!(
            phases,
            vec![
                (
                    "in_progress".to_owned(),
                    PhaseDef {
                        model: Some("sonnet".to_owned()),
                        duty: None,
                        gate: None,
                    },
                ),
                (
                    "accepting".to_owned(),
                    PhaseDef {
                        model: Some("sonnet".to_owned()),
                        duty: None,
                        gate: None,
                    },
                ),
            ]
        );

        let names = phases
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"enriching"));
        assert!(!names.contains(&"verifying"));

        let (_, sequence) = normalize_palette_and_sequence(phases, Vec::new());
        assert_eq!(
            sequence,
            vec![
                "pending".to_owned(),
                "in_progress".to_owned(),
                "accepting".to_owned(),
                "done".to_owned(),
            ]
        );
    }
}
