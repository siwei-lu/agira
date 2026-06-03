use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use thiserror::Error;

use crate::{
    config::{Config, VerificationConfig},
    project::Project,
};

const DEFAULT_STATE_MACHINE: [&str; 4] = ["enriching", "in_progress", "verifying", "done"];
const ACCEPTANCE_TESTING_VALUES: [&str; 5] = ["cli", "api", "ui", "hybrid", "none"];
const CONVENTIONAL_COMMIT_TYPES: [&str; 10] = [
    "feat", "fix", "chore", "docs", "refactor", "test", "perf", "ci", "build", "revert",
];

#[derive(Debug, Error)]
pub enum InitError {
    #[error("agira init requires an interactive terminal")]
    NonInteractive,

    #[error("input ended before init completed")]
    InputEnded,

    #[error("failed to read stdin")]
    StdinRead(#[source] io::Error),

    #[error("failed to serialize config")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to write {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn run_init(project: &Project) -> Result<(), InitError> {
    if !stdout_is_tty() {
        return Err(InitError::NonInteractive);
    }

    let defaults = scan_project(&project.git_root);
    let config_path = project.state_dir.join("config.json");

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    if !confirm_overwrite(&config_path, &mut reader)? {
        return Ok(());
    }

    let config = interview(&mut reader, defaults)?;
    write_config(&config_path, &config)
}

fn stdout_is_tty() -> bool {
    (unsafe { libc::isatty(libc::STDOUT_FILENO) }) == 1
}

fn scan_project(git_root: &Path) -> Config {
    let _commit_pattern = detect_commit_pattern(git_root);

    if git_root.join("Cargo.toml").exists() {
        return config_for_stack(
            "rust",
            vec![
                "cargo fmt -- --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo test".to_owned(),
            ],
            "cli",
        );
    }

    if git_root.join("package.json").exists() {
        return scan_package_json_project(git_root);
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
        );
    }

    if git_root.join("pubspec.yaml").exists() {
        return scan_pubspec_project(git_root);
    }

    config_for_stack("unknown", Vec::new(), "none")
}

fn scan_package_json_project(git_root: &Path) -> Config {
    let package_json = read_package_json(&git_root.join("package.json"));
    let package_manager = detect_package_manager(git_root, package_json.as_ref());
    let stack = if is_typescript_project(git_root, package_json.as_ref()) {
        "typescript"
    } else {
        "javascript"
    };
    let commands = package_commands(package_manager, package_json.as_ref());
    let acceptance_testing = package_acceptance_testing(package_json.as_ref());

    config_for_stack(stack, commands, &acceptance_testing)
}

fn scan_pubspec_project(git_root: &Path) -> Config {
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
        )
    }
}

fn config_for_stack(stack: &str, commands: Vec<String>, acceptance_testing: &str) -> Config {
    Config {
        stack: stack.to_owned(),
        state_machine: default_state_machine(),
        models: default_models(),
        verification: VerificationConfig { commands },
        acceptance_testing: acceptance_testing.to_owned(),
        max_retries: 3,
        prd_path: None,
    }
}

fn default_state_machine() -> Vec<String> {
    DEFAULT_STATE_MACHINE
        .iter()
        .map(|phase| (*phase).to_owned())
        .collect()
}

fn default_models() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("implementer".to_owned(), "sonnet".to_owned()),
        ("reviewer".to_owned(), "sonnet".to_owned()),
        ("verifier".to_owned(), "haiku".to_owned()),
    ])
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

fn detect_commit_pattern(git_root: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["log", "--no-merges", "-10", "--format=%s"])
        .output();

    let Ok(output) = output else {
        return "unknown".to_owned();
    };

    if !output.status.success() {
        return "unknown".to_owned();
    }

    let subjects = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = subjects
        .lines()
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
        .collect();

    if subjects.is_empty() {
        "unknown".to_owned()
    } else if subjects
        .iter()
        .all(|subject| is_conventional_commit(subject))
    {
        "Conventional Commits".to_owned()
    } else {
        "unknown/mixed".to_owned()
    }
}

fn is_conventional_commit(subject: &str) -> bool {
    let Some((prefix, _message)) = subject.split_once(": ") else {
        return false;
    };
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
    let commit_type = prefix
        .split_once('(')
        .map(|(commit_type, scope)| {
            if scope.ends_with(')')
                && scope[..scope.len() - 1].chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
            {
                commit_type
            } else {
                ""
            }
        })
        .unwrap_or(prefix);

    CONVENTIONAL_COMMIT_TYPES.contains(&commit_type)
}

fn confirm_overwrite<R: BufRead>(path: &Path, reader: &mut R) -> Result<bool, InitError> {
    if !path.exists() {
        return Ok(true);
    }

    println!("Config already exists. Overwrite? [y/N]");
    io::stdout().flush().map_err(|source| InitError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let line = read_required_line(reader)?;
    Ok(line.starts_with('y') || line.starts_with('Y'))
}

fn interview<R: BufRead>(reader: &mut R, mut config: Config) -> Result<Config, InitError> {
    loop {
        println!("Detected phase list: {}", config.state_machine.join(", "));
        print!("Confirm phases with Enter or y, or enter comma-separated overrides: ");
        flush_stdout_for_prompt()?;
        let line = read_required_line(reader)?;

        match parse_state_machine_override(&line, &config.state_machine) {
            Ok(state_machine) => {
                config.state_machine = state_machine;
                break;
            }
            Err(message) => println!("{message}"),
        }
    }

    loop {
        println!("Default models: {}", format_models(&config.models));
        print!("Confirm models with Enter or y, or enter role=model pairs: ");
        flush_stdout_for_prompt()?;
        let line = read_required_line(reader)?;

        match parse_models_override(&line, &config.models) {
            Ok(models) => {
                config.models = models;
                break;
            }
            Err(message) => println!("{message}"),
        }
    }

    println!(
        "Verification commands: {}",
        format_commands(&config.verification.commands)
    );
    print!("Confirm commands with Enter or y, enter semicolon-separated commands, or none: ");
    flush_stdout_for_prompt()?;
    let line = read_required_line(reader)?;
    config.verification.commands =
        parse_verification_commands(&line, &config.verification.commands);

    loop {
        println!("Default acceptance_testing: {}", config.acceptance_testing);
        print!("Confirm with Enter or y, or enter cli/api/ui/hybrid/none: ");
        flush_stdout_for_prompt()?;
        let line = read_required_line(reader)?;

        match parse_acceptance_testing_override(&line, &config.acceptance_testing) {
            Ok(acceptance_testing) => {
                config.acceptance_testing = acceptance_testing;
                break;
            }
            Err(message) => println!("{message}"),
        }
    }

    print!("Default PRD path (blank to skip):");
    flush_stdout_for_prompt()?;
    let line = read_required_line(reader)?;
    let prd_path = line.trim();
    config.prd_path = if prd_path.is_empty() {
        None
    } else {
        Some(prd_path.to_owned())
    };

    Ok(config)
}

fn flush_stdout_for_prompt() -> Result<(), InitError> {
    io::stdout().flush().map_err(|source| InitError::Io {
        path: PathBuf::from("<stdout>"),
        source,
    })
}

fn read_required_line<R: BufRead>(reader: &mut R) -> Result<String, InitError> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line).map_err(InitError::StdinRead)?;

    if bytes_read == 0 {
        return Err(InitError::InputEnded);
    }

    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn should_keep_default(input: &str) -> bool {
    matches!(input.trim(), "" | "y" | "Y")
}

fn parse_state_machine_override(
    input: &str,
    default: &[String],
) -> Result<Vec<String>, &'static str> {
    if should_keep_default(input) {
        return Ok(default.to_vec());
    }

    let phases: Vec<String> = input
        .split(',')
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if phases.is_empty()
        || phases
            .iter()
            .any(|phase| phase.chars().any(char::is_whitespace))
    {
        Err("invalid phase list; use comma-separated phase names without whitespace")
    } else {
        Ok(phases)
    }
}

fn parse_models_override(
    input: &str,
    default: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, &'static str> {
    if should_keep_default(input) {
        return Ok(default.clone());
    }

    let mut models = BTreeMap::new();

    for pair in input
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let Some((role, model)) = pair.split_once('=') else {
            return Err("invalid models; use comma-separated role=model pairs");
        };
        let role = role.trim();
        let model = model.trim();

        if role.is_empty() || model.is_empty() {
            return Err("invalid models; role and model must be non-empty");
        }

        models.insert(role.to_owned(), model.to_owned());
    }

    if models.is_empty() {
        Err("invalid models; use comma-separated role=model pairs")
    } else {
        Ok(models)
    }
}

fn parse_verification_commands(input: &str, default: &[String]) -> Vec<String> {
    let trimmed = input.trim();

    if should_keep_default(trimmed) {
        default.to_vec()
    } else if trimmed == "none" {
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

fn parse_acceptance_testing_override(input: &str, default: &str) -> Result<String, &'static str> {
    if should_keep_default(input) {
        return Ok(default.to_owned());
    }

    let value = input.trim();
    if ACCEPTANCE_TESTING_VALUES.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err("invalid acceptance_testing; use cli, api, ui, hybrid, or none")
    }
}

fn format_models(models: &BTreeMap<String, String>) -> String {
    models
        .iter()
        .map(|(role, model)| format!("{role}={model}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_commands(commands: &[String]) -> String {
    if commands.is_empty() {
        "none".to_owned()
    } else {
        commands.join("; ")
    }
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
    use std::{fs, io::Cursor};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn slug_defaults_for_rust() {
        let git_root = TempDir::new().unwrap();
        fs::write(git_root.path().join("Cargo.toml"), "").unwrap();

        let config = scan_project(git_root.path());

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
    }

    #[test]
    fn slug_defaults_for_unknown() {
        let git_root = TempDir::new().unwrap();

        let config = scan_project(git_root.path());

        assert_eq!(config.stack, "unknown");
        assert!(config.verification.commands.is_empty());
        assert_eq!(config.acceptance_testing, "none");
    }

    #[test]
    fn parse_state_machine_override() {
        let default = default_state_machine();

        assert_eq!(
            super::parse_state_machine_override("a,b,c", &default).unwrap(),
            ["a", "b", "c"]
        );
        assert_eq!(
            super::parse_state_machine_override("", &default).unwrap(),
            default
        );
    }

    #[test]
    fn parse_models_override() {
        let default = default_models();

        assert_eq!(
            super::parse_models_override("implementer=opus,reviewer=sonnet", &default).unwrap(),
            BTreeMap::from([
                ("implementer".to_owned(), "opus".to_owned()),
                ("reviewer".to_owned(), "sonnet".to_owned()),
            ])
        );
        assert_eq!(super::parse_models_override("", &default).unwrap(), default);
    }

    #[test]
    fn parse_verification_commands() {
        let default = vec!["default".to_owned()];

        assert_eq!(
            super::parse_verification_commands("cmd1;cmd2", &default),
            ["cmd1", "cmd2"]
        );
        assert!(super::parse_verification_commands("none", &default).is_empty());
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
        );

        write_config(&path, &config).unwrap();

        let contents = fs::read_to_string(path).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();

        assert!(value.get("stack").is_some());
        assert!(value.get("state_machine").is_some());
        assert!(value.get("models").is_some());
        assert!(value.get("verification").is_some());
        assert!(value.get("acceptance_testing").is_some());
        assert!(value.get("prd_path").is_none());
    }

    #[test]
    fn overwrite_declined_does_not_change_config() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        let original = "{\"stack\":\"existing\"}\n";
        fs::write(&path, original).unwrap();
        let mut reader = Cursor::new("n\n");

        assert!(!confirm_overwrite(&path, &mut reader).unwrap());

        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
