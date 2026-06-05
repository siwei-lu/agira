use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;
use thiserror::Error;

use crate::core::{
    config::{
        Config, INITIAL_PHASE_NAME, PhaseConfig, TERMINAL_PHASE_NAME, VerificationConfig,
        normalize_mandatory_phases,
    },
    project::Project,
};

const ACCEPTANCE_TESTING_VALUES: [&str; 5] = ["cli", "api", "ui", "hybrid", "none"];

#[derive(Debug, Error)]
pub enum InitError {
    #[error(
        "agira init requires all flags or none; missing: {}",
        missing.join(" ")
    )]
    MissingFlags { missing: Vec<String> },

    #[error(
        "invalid phases: use comma-separated phase:model pairs (e.g. enriching:opus,in_progress:sonnet)"
    )]
    InvalidPhases,

    #[error("invalid acceptance-testing: use cli, api, ui, hybrid, or none")]
    InvalidAcceptanceTesting,

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
    pub verification_commands: Option<String>,
    pub acceptance_testing: Option<String>,
    pub prd_path: Option<String>,
}

pub fn run_init(project: &Project, flags: InitFlags) -> Result<(), InitError> {
    let missing = detect_missing_flags(&flags);

    if !missing.is_empty() {
        return Err(InitError::MissingFlags { missing });
    }

    if !has_required_flags(&flags) {
        let _defaults = scan_project(&project.git_root, project.global_config.default_max_retries);
        print!("{}", bare_invocation_prompt());
        return Ok(());
    }

    let stack = flags.stack.as_deref().unwrap_or_default().trim();
    if stack.is_empty() {
        return Err(InitError::MissingFlags {
            missing: vec!["--stack".to_owned()],
        });
    }

    let config = Config {
        stack: stack.to_owned(),
        phases: parse_phases_flag(flags.phases.as_deref().unwrap_or_default())?,
        verification: VerificationConfig {
            commands: parse_verification_commands_flag(
                flags.verification_commands.as_deref().unwrap_or_default(),
            ),
        },
        acceptance_testing: parse_acceptance_testing_flag(
            flags.acceptance_testing.as_deref().unwrap_or_default(),
        )?,
        max_retries: project.global_config.default_max_retries,
        prd_path: parse_prd_path_flag(flags.prd_path.as_deref()),
    };

    let config_path = project.state_dir.join("config.json");
    write_config(&config_path, &config)
}

fn scan_project(git_root: &Path, max_retries: u32) -> Config {
    if git_root.join("Cargo.toml").exists() {
        return config_for_stack(
            "rust",
            vec![
                "cargo fmt -- --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo test".to_owned(),
            ],
            "cli",
            max_retries,
        );
    }

    if git_root.join("package.json").exists() {
        return scan_package_json_project(git_root, max_retries);
    }

    if git_root.join("go.mod").exists() {
        return config_for_stack(
            "go",
            vec![
                "test -z \"$(gofmt -l .)\"".to_owned(),
                "go vet ./...".to_owned(),
                "go test ./...".to_owned(),
            ],
            "api",
            max_retries,
        );
    }

    if git_root.join("pyproject.toml").exists() {
        return config_for_stack(
            "python",
            vec![
                "python -m ruff format --check .".to_owned(),
                "python -m ruff check .".to_owned(),
                "python -m pytest".to_owned(),
            ],
            "api",
            max_retries,
        );
    }

    if git_root.join("pom.xml").exists() {
        return config_for_stack("java", vec!["mvn verify".to_owned()], "api", max_retries);
    }

    if git_root.join("build.gradle").exists() || git_root.join("build.gradle.kts").exists() {
        return config_for_stack(
            "java",
            vec!["./gradlew test".to_owned()],
            "api",
            max_retries,
        );
    }

    if git_root.join("pubspec.yaml").exists() {
        return scan_pubspec_project(git_root, max_retries);
    }

    config_for_stack("unknown", Vec::new(), "none", max_retries)
}

fn scan_package_json_project(git_root: &Path, max_retries: u32) -> Config {
    let package_json = read_package_json(&git_root.join("package.json"));
    let package_manager = detect_package_manager(git_root, package_json.as_ref());
    let stack = if is_typescript_project(git_root, package_json.as_ref()) {
        "typescript"
    } else {
        "javascript"
    };
    let commands = package_commands(package_manager, package_json.as_ref());
    let acceptance_testing = package_acceptance_testing(package_json.as_ref());

    config_for_stack(stack, commands, &acceptance_testing, max_retries)
}

fn scan_pubspec_project(git_root: &Path, max_retries: u32) -> Config {
    let contents = fs::read_to_string(git_root.join("pubspec.yaml")).unwrap_or_default();

    if contents.contains("flutter:") || contents.contains("sdk: flutter") {
        config_for_stack(
            "flutter",
            vec![
                "dart format --output=none --set-exit-if-changed .".to_owned(),
                "flutter analyze".to_owned(),
                "flutter test".to_owned(),
            ],
            "ui",
            max_retries,
        )
    } else {
        config_for_stack(
            "dart",
            vec![
                "dart format --output=none --set-exit-if-changed .".to_owned(),
                "dart analyze".to_owned(),
                "dart test".to_owned(),
            ],
            "cli",
            max_retries,
        )
    }
}

fn config_for_stack(
    stack: &str,
    commands: Vec<String>,
    acceptance_testing: &str,
    max_retries: u32,
) -> Config {
    Config {
        stack: stack.to_owned(),
        phases: default_phases(),
        verification: VerificationConfig { commands },
        acceptance_testing: acceptance_testing.to_owned(),
        max_retries,
        prd_path: None,
    }
}

fn default_phases() -> Vec<PhaseConfig> {
    vec![
        PhaseConfig {
            name: INITIAL_PHASE_NAME.to_owned(),
            model: "sonnet".to_owned(),
        },
        PhaseConfig {
            name: "enriching".to_owned(),
            model: "opus".to_owned(),
        },
        PhaseConfig {
            name: "in_progress".to_owned(),
            model: "sonnet".to_owned(),
        },
        PhaseConfig {
            name: "verifying".to_owned(),
            model: "haiku".to_owned(),
        },
        PhaseConfig {
            name: TERMINAL_PHASE_NAME.to_owned(),
            model: "haiku".to_owned(),
        },
    ]
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

fn package_acceptance_testing(package_json: Option<&Value>) -> String {
    let frontend = has_any_package_dependency(
        package_json,
        &["next", "vite", "react", "vue", "svelte", "angular"],
    );
    let api =
        has_any_package_dependency(package_json, &["express", "fastify", "koa", "hapi", "nest"]);

    match (frontend, api) {
        (true, true) => "hybrid",
        (true, false) => "ui",
        (false, true) => "api",
        (false, false) => "cli",
    }
    .to_owned()
}

fn has_any_package_dependency(package_json: Option<&Value>, markers: &[&str]) -> bool {
    package_json
        .into_iter()
        .filter_map(Value::as_object)
        .flat_map(|root| {
            [
                "dependencies",
                "devDependencies",
                "peerDependencies",
                "optionalDependencies",
            ]
            .into_iter()
            .filter_map(|section| root.get(section))
            .filter_map(Value::as_object)
        })
        .flat_map(|dependencies| dependencies.keys())
        .any(|name| dependency_matches(name, markers))
}

fn dependency_matches(name: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| {
        name == *marker
            || (*marker == "angular" && name.starts_with("@angular/"))
            || (*marker == "hapi" && name.contains("hapi"))
            || (*marker == "nest" && name.starts_with("@nestjs/"))
    })
}

fn bare_invocation_prompt() -> &'static str {
    r#"# agira init — agent setup required

`agira init` was called without flags. Your job: scan this repo, prove the project can
start locally, reason about what you find, recommend configuration, interview the user,
then call `agira init` with all flags filled in.

## Step 1 — Scan the repo

Read and record findings from each of the following before asking the user anything:

1. **Stack markers** — check the repo root for: `Cargo.toml`, `package.json`, `pom.xml`,
   `build.gradle`, `build.gradle.kts`, `go.mod`, `pyproject.toml`, `pubspec.yaml`.
   If `package.json` exists, read its content: detect TypeScript (tsconfig.json or `typescript`
   in devDependencies), package manager (`packageManager` field or lockfiles: `bun.lockb`,
   `pnpm-lock.yaml`, `yarn.lock`), frontend frameworks (next, vite, react, vue, svelte, angular),
   backend frameworks (express, fastify, koa, hapi, nest).

2. **Build / test / lint commands** — read `package.json` scripts section, any `Makefile`,
   CI configs (`.github/workflows/`, `.gitlab-ci.yml`). Record the exact commands.

3. **Run / start instructions** — find and read the project's local run instructions:
   README files, docs, `package.json` scripts (`dev`, `start`, `serve`), `Makefile`,
   `Cargo.toml`, `pyproject.toml`, Docker Compose files, Procfiles, or framework config.
   Record the exact start command, expected port or URL, and any documented env setup.

4. **Project structure** — list top-level directories and key source files to understand
   whether this is a CLI, library, API, full-stack app, or something else.

5. **Existing AI config** — read `CLAUDE.md`, `.claude/settings.json` if present.

6. **Commit conventions** — run `git log --no-merges -10 --format="%s"` and note the pattern
   (Conventional Commits, Angular, freeform, etc.).

7. **PRD** — check `docs/prd.md`. If found and it contains `## Functional Modules` with FM-IDs,
   record its path. Otherwise note it as absent.

## Step 2 — Prove the project starts (REQUIRED)

After scanning and before recommending flags, you MUST make a real local start/run attempt.
This step is required, not optional. Do not proceed to `agira init` until the project starts successfully, unless you have a concrete blocker that requires user input.

1. Choose the most appropriate run command from the instructions you found.
2. Install missing dependencies using the repo's declared package manager or build tool.
3. Resolve environment issues when possible: read `.env.example` or docs, set documented
   non-secret defaults, choose a free supported port, and fix missing generated files.
   Do not invent secrets; ask the user if a real credential is required.
4. Actually start the dev server, API, CLI, or app. For long-running servers, start them
   only long enough to verify readiness, then stop the process unless the user asks to
   keep it running.
5. Confirm success with concrete evidence: listening URL, health endpoint, rendered page,
   CLI output, or equivalent smoke check.

Record the exact start command, required env setup, port or URL, and verification evidence.
These findings must feed into `--verification-commands`, `--acceptance-testing`, and
`CLAUDE.md`.

## Step 3 — Reason and recommend

After scanning, reason about each flag. For each one, propose 2–3 concrete options ranked by
fit, with a brief justification. **Do not just show a value and ask "confirm or override?"** —
the user expects analysis, not confirmation dialogs.

**`--stack`**
Name the detected language and primary framework. If the repo is a monorepo or has multiple
stacks, ask which part agira is being set up for before continuing.

**`--phases`**
Reason about project complexity to propose the middle of the state machine. Each phase carries its model.
`pending` is always inserted first and `done` is always inserted last; include them only if you want to override their default models.
- Design / enrichment phases → `opus` (reasoning-heavy, architecture decisions)
- Implementation phases → `sonnet` (code generation, good cost/quality balance)
- Verification / linting phases → `haiku` (fast, cheap, mechanical checks)

Examples:
- PRD-driven project with review loop: `enriching:opus,in_progress:sonnet,reviewing:sonnet,verifying:haiku`
- CLI tool or library: `in_progress:sonnet,verifying:haiku`
- Prototype: `in_progress:sonnet`

Present 2 options with a clear trade-off. Format: `phase:model,phase:model,...`

**`--verification-commands`**
From your scan and required start proof, propose exact commands. Prefer commands found in
CI or Makefile over anything you infer. Include the confirmed project-start smoke command
or wrapper command, plus any setup command required before acceptance testing can run.
Use semicolons only as command separators; do not put raw semicolons inside an individual
command. If a reliable start check requires multiple shell operations, prefer an existing
single project script or Makefile target and list that.
Format: `cmd1;cmd2;cmd3`

**`--acceptance-testing`**
Reason from what you successfully started, not markers alone:
- Frontend markers (next, vite, react, vue, svelte, angular) → `ui`
- Backend markers (express, fastify, koa, nest, Spring, FastAPI, etc.) → `api`
- Both present → `hybrid`
- CLI binary or library → `cli`
- Cannot determine → ask explicitly; do not guess
Valid values: `cli`, `api`, `ui`, `hybrid`, `none`

**`--prd-path`**
If you found `docs/prd.md` with FM-IDs, propose it. Otherwise leave blank unless the user
mentions a requirements document.

## Step 4 — Interview the user

Present your findings, required start proof, and recommendations, one flag at a time. For each:
1. State what the scan found (one line)
2. State your recommendation and why (one or two lines)
3. Offer 2–3 concrete alternatives when the choice isn't obvious
4. Ask for a decision — not an open-ended "what would you like?"

If the scan clearly determined a value with no ambiguity, confirm it in a single line and
move on. Only dwell on decisions where multiple options are genuinely reasonable.

## Step 5 — Run agira init

Once all values are confirmed, call:

```sh
agira init \
  --stack <stack> \
  --phases <phase:model,...> \
  --verification-commands <cmd1;cmd2;...> \
  --acceptance-testing <cli|api|ui|hybrid|none> \
  [--prd-path <path>]
```

## Step 6 — Write CLAUDE.md

After running `agira init`, write a **complete** `CLAUDE.md` in the repo root that captures
all project context gathered in Steps 1–4. The goal is a file any AI agent can read to start
working in this repo immediately — not a bounded annotation.

**If CLAUDE.md does not exist:** create it from scratch.

**If CLAUDE.md exists:** read its current contents first, then write a new version that folds
in what was there (preserving any useful project-specific guidance) and adds everything below.

The CLAUDE.md must cover all of these at minimum:

- **Stack** — language, primary framework, key libraries
- **Project structure** — what the top-level directories contain; where the main source tree lives
- **Build, test, and lint commands** — exact commands, ready to copy-paste
- **Local run/start** — exact start command, required env setup, port or URL, and proof it worked
- **Commit conventions** — pattern from `git log`; omit if no consistent pattern was found
- **PRD** — relative path if a requirements document was confirmed; omit otherwise
- **Development workflow** — any conventions captured in existing config (current CLAUDE.md,
  `.claude/settings.json`, CI files, Makefile, etc.)

Write the whole file, not just an appended block.
"#
}

fn detect_missing_flags(flags: &InitFlags) -> Vec<String> {
    let required = [
        ("--stack", flags.stack.as_ref()),
        ("--phases", flags.phases.as_ref()),
        (
            "--verification-commands",
            flags.verification_commands.as_ref(),
        ),
        ("--acceptance-testing", flags.acceptance_testing.as_ref()),
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
    flags.stack.is_some()
        && flags.phases.is_some()
        && flags.verification_commands.is_some()
        && flags.acceptance_testing.is_some()
}

fn parse_phases_flag(input: &str) -> Result<Vec<PhaseConfig>, InitError> {
    let phases: Result<Vec<PhaseConfig>, InitError> = input
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
                    if name.is_empty()
                        || model.is_empty()
                        || name.chars().any(char::is_whitespace)
                        || model.chars().any(char::is_whitespace)
                    {
                        Err(InitError::InvalidPhases)
                    } else {
                        Ok(PhaseConfig {
                            name: name.to_owned(),
                            model: model.to_owned(),
                        })
                    }
                }
                None => Err(InitError::InvalidPhases),
            }
        })
        .collect();

    let phases = phases?;
    if phases.is_empty() {
        return Err(InitError::InvalidPhases);
    }
    Ok(normalize_mandatory_phases(phases))
}

fn parse_verification_commands_flag(input: &str) -> Vec<String> {
    let trimmed = input.trim();

    if trimmed == "none" {
        Vec::new()
    } else {
        trimmed
            .split(';')
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }
}

fn parse_acceptance_testing_flag(input: &str) -> Result<String, InitError> {
    let value = input.trim();

    if ACCEPTANCE_TESTING_VALUES.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(InitError::InvalidAcceptanceTesting)
    }
}

fn parse_prd_path_flag(input: Option<&str>) -> Option<String> {
    input
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
}

fn write_config(path: &Path, config: &Config) -> Result<(), InitError> {
    let bytes = serde_json::to_vec_pretty(config).map_err(InitError::Serialize)?;
    let temporary_path = path.with_extension("json.tmp");

    fs::write(&temporary_path, bytes).map_err(|source| InitError::Io {
        path: temporary_path.clone(),
        source,
    })?;
    fs::rename(&temporary_path, path).map_err(|source| InitError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    println!("config written to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn slug_defaults_for_rust() {
        let git_root = TempDir::new().unwrap();
        fs::write(git_root.path().join("Cargo.toml"), "").unwrap();

        let config = scan_project(git_root.path(), 5);

        assert_eq!(config.stack, "rust");
        assert_eq!(
            config.verification.commands,
            [
                "cargo fmt -- --check",
                "cargo clippy -- -D warnings",
                "cargo test"
            ]
        );
        assert_eq!(config.acceptance_testing, "cli");
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.phases[0].name, "pending");
        assert_eq!(config.phases[0].model, "sonnet");
        assert_eq!(config.phases[1].name, "enriching");
        assert_eq!(config.phases[1].model, "opus");
    }

    #[test]
    fn slug_defaults_for_unknown() {
        let git_root = TempDir::new().unwrap();

        let config = scan_project(git_root.path(), 4);

        assert_eq!(config.stack, "unknown");
        assert!(config.verification.commands.is_empty());
        assert_eq!(config.acceptance_testing, "none");
        assert_eq!(config.max_retries, 4);
    }

    #[test]
    fn slug_defaults_for_java_maven() {
        let git_root = TempDir::new().unwrap();
        fs::write(git_root.path().join("pom.xml"), "").unwrap();

        let config = scan_project(git_root.path(), 3);

        assert_eq!(config.stack, "java");
        assert_eq!(config.verification.commands, ["mvn verify"]);
        assert_eq!(config.acceptance_testing, "api");
    }

    #[test]
    fn slug_defaults_for_java_gradle() {
        let git_root = TempDir::new().unwrap();
        fs::write(git_root.path().join("build.gradle"), "").unwrap();

        let config = scan_project(git_root.path(), 3);

        assert_eq!(config.stack, "java");
        assert_eq!(config.verification.commands, ["./gradlew test"]);
        assert_eq!(config.acceptance_testing, "api");
    }

    #[test]
    fn slug_defaults_for_java_gradle_kts() {
        let git_root = TempDir::new().unwrap();
        fs::write(git_root.path().join("build.gradle.kts"), "").unwrap();

        let config = scan_project(git_root.path(), 3);

        assert_eq!(config.stack, "java");
        assert_eq!(config.verification.commands, ["./gradlew test"]);
        assert_eq!(config.acceptance_testing, "api");
    }

    #[test]
    fn parse_phases_flag() {
        let phases =
            super::parse_phases_flag("enriching:opus,in_progress:sonnet,done:haiku").unwrap();
        assert_eq!(phases.len(), 4);
        assert_eq!(phases[0].name, "pending");
        assert_eq!(phases[0].model, "sonnet");
        assert_eq!(phases[1].name, "enriching");
        assert_eq!(phases[1].model, "opus");
        assert_eq!(phases[2].name, "in_progress");
        assert_eq!(phases[2].model, "sonnet");
        assert_eq!(phases[3].name, "done");
        assert_eq!(phases[3].model, "haiku");

        let single = super::parse_phases_flag("done:haiku").unwrap();
        assert_eq!(single.len(), 2);
        assert_eq!(single[0].name, "pending");
        assert_eq!(single[0].model, "sonnet");
        assert_eq!(single[1].name, "done");
        assert_eq!(single[1].model, "haiku");

        let without_mandatory =
            super::parse_phases_flag("enriching:opus,in_progress:sonnet").unwrap();
        assert_eq!(without_mandatory.len(), 4);
        assert_eq!(without_mandatory[0].name, "pending");
        assert_eq!(without_mandatory[1].name, "enriching");
        assert_eq!(without_mandatory[2].name, "in_progress");
        assert_eq!(without_mandatory[3].name, "done");

        assert!(matches!(
            super::parse_phases_flag("in progress:sonnet"),
            Err(InitError::InvalidPhases)
        ));
        assert!(matches!(
            super::parse_phases_flag(""),
            Err(InitError::InvalidPhases)
        ));
        assert!(matches!(
            super::parse_phases_flag("enriching"),
            Err(InitError::InvalidPhases)
        ));
        assert!(matches!(
            super::parse_phases_flag(":sonnet"),
            Err(InitError::InvalidPhases)
        ));
        assert!(matches!(
            super::parse_phases_flag("enriching:"),
            Err(InitError::InvalidPhases)
        ));
    }

    #[test]
    fn parse_verification_commands_flag() {
        assert!(super::parse_verification_commands_flag("none").is_empty());
        assert_eq!(
            super::parse_verification_commands_flag(" cargo test ; cargo fmt ; ; "),
            ["cargo test", "cargo fmt"]
        );
        assert_eq!(
            super::parse_verification_commands_flag("cargo test"),
            ["cargo test"]
        );
    }

    #[test]
    fn parse_acceptance_testing_flag() {
        for value in ["cli", "api", "ui", "hybrid", "none"] {
            assert_eq!(super::parse_acceptance_testing_flag(value).unwrap(), value);
        }
        assert!(matches!(
            super::parse_acceptance_testing_flag("browser"),
            Err(InitError::InvalidAcceptanceTesting)
        ));
    }

    #[test]
    fn detect_missing_flags() {
        let all_present = InitFlags {
            stack: Some("rust".to_owned()),
            phases: Some("done:haiku".to_owned()),
            verification_commands: Some("cargo test".to_owned()),
            acceptance_testing: Some("cli".to_owned()),
            prd_path: None,
        };
        assert!(super::detect_missing_flags(&all_present).is_empty());

        let partial = InitFlags {
            stack: Some("rust".to_owned()),
            phases: Some("done:haiku".to_owned()),
            verification_commands: None,
            acceptance_testing: None,
            prd_path: None,
        };
        assert_eq!(
            super::detect_missing_flags(&partial),
            ["--verification-commands", "--acceptance-testing"]
        );

        assert!(super::detect_missing_flags(&InitFlags::default()).is_empty());
    }

    #[test]
    fn bare_invocation_prompt() {
        let prompt = super::bare_invocation_prompt();

        assert!(prompt.contains("```sh\nagira init \\\n"));
        assert!(prompt.contains("CLAUDE.md"));
        assert!(prompt.contains("Write the whole file, not just an appended block."));
        assert!(!prompt.contains("agira-context"));
        assert!(prompt.contains("phase:model"));
        assert!(!prompt.contains("--models"));
        assert!(prompt.contains("## Step 2 — Prove the project starts (REQUIRED)"));
        assert!(prompt.contains("This step is required, not optional."));
        assert!(
            prompt.contains("Do not proceed to `agira init` until the project starts successfully")
        );
        assert!(prompt.contains("Run / start instructions"));
        assert!(prompt.contains("confirmed project-start smoke command"));
        assert!(prompt.contains("Reason from what you successfully started, not markers alone"));
        assert!(prompt.contains("Local run/start"));
    }

    #[test]
    fn write_config_produces_valid_json() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        let config = config_for_stack(
            "rust",
            vec![
                "cargo fmt -- --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo test".to_owned(),
            ],
            "cli",
            5,
        );

        write_config(&path, &config).unwrap();

        let contents = fs::read_to_string(path).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();

        assert!(value.get("stack").is_some());
        assert!(value.get("phases").is_some());
        assert!(value.get("state_machine").is_none());
        assert!(value.get("models").is_none());
        assert!(value.get("verification").is_some());
        assert!(value.get("acceptance_testing").is_some());
        assert_eq!(value.get("max_retries").and_then(Value::as_u64), Some(5));
        assert!(value.get("default_model").is_none());
        assert!(value.get("prd_path").is_none());
    }
}
