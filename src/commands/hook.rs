use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::core::{
    config::{Config, ConfigError, load_project_config},
    hooks::{
        ALL_TASKS_DONE_EVENT, HookConfig, HookConfigError, HookEntry, TASK_ADDED_EVENT, save_hooks,
        save_hooks_preserving_toml,
    },
    project::Project,
};

#[derive(Debug, Error)]
pub enum HookError {
    #[error("invalid hook event: event cannot be empty or contain whitespace")]
    InvalidEventName,

    #[error("unknown hook event: {event}; valid events: {}", valid_events.join(", "))]
    UnknownEvent {
        event: String,
        valid_events: Vec<String>,
    },

    #[error("hook command cannot be empty")]
    EmptyCommand,

    #[error("no {scope} hook configured for {event}")]
    HookNotFound { scope: HookScope, event: String },

    #[error("config file not found: {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("failed to read {path}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to load config {path}: {source}")]
    ConfigLoad {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid config {path}: {reason}")]
    InvalidConfig { path: PathBuf, reason: String },

    #[error(transparent)]
    Hooks(#[from] HookConfigError),

    #[error("failed to delete {path}")]
    Delete {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
    Project,
    Global,
}

impl HookScope {
    fn from_global(global: bool) -> Self {
        if global { Self::Global } else { Self::Project }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

impl fmt::Display for HookScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

pub fn run_hook_list(project: &Project) -> Result<(), HookError> {
    print!("{}", format_hook_list(project));

    Ok(())
}

pub fn run_hook_add(
    project: &Project,
    event: &str,
    command_parts: &[String],
    global: bool,
) -> Result<(), HookError> {
    validate_event(project, event)?;
    let command = command_from_parts(command_parts);
    if command.trim().is_empty() {
        return Err(HookError::EmptyCommand);
    }

    let scope = HookScope::from_global(global);
    let mut hooks = scoped_hooks(project, scope).clone();
    hooks.hooks.push(HookEntry {
        on: event.to_owned(),
        run: command.clone(),
    });
    save_scoped_hooks(&scoped_hooks_path(project, scope), &hooks, scope)?;

    if scope == HookScope::Project {
        println!("added hook {event}: {command}");
    } else {
        println!("added {scope} hook {event}: {command}");
    }

    Ok(())
}

pub fn run_hook_remove(project: &Project, event: &str, global: bool) -> Result<(), HookError> {
    validate_event(project, event)?;

    let scope = HookScope::from_global(global);
    let mut hooks = scoped_hooks(project, scope).clone();
    let before = hooks.hooks.len();
    hooks.hooks.retain(|hook| hook.on != event);
    let removed = before - hooks.hooks.len();

    if removed == 0 {
        return Err(HookError::HookNotFound {
            scope,
            event: event.to_owned(),
        });
    }

    let hooks_path = scoped_hooks_path(project, scope);
    if hooks.hooks.is_empty() && scope == HookScope::Project {
        delete_hooks_file(&hooks_path)?;
    } else {
        save_scoped_hooks(&hooks_path, &hooks, scope)?;
    }

    if scope == HookScope::Project {
        println!("removed {removed} hook(s) for {event}");
    } else {
        println!("removed {removed} {scope} hook(s) for {event}");
    }

    Ok(())
}

pub fn run_hook_update(
    project: &Project,
    event: &str,
    command_parts: &[String],
    global: bool,
) -> Result<(), HookError> {
    validate_event(project, event)?;
    let command = command_from_parts(command_parts);
    if command.trim().is_empty() {
        return Err(HookError::EmptyCommand);
    }

    let scope = HookScope::from_global(global);
    let mut hooks = scoped_hooks(project, scope).clone();
    let mut updated = 0;
    for hook in hooks.hooks.iter_mut().filter(|hook| hook.on == event) {
        hook.run = command.clone();
        updated += 1;
    }

    if updated == 0 {
        return Err(HookError::HookNotFound {
            scope,
            event: event.to_owned(),
        });
    }

    save_scoped_hooks(&scoped_hooks_path(project, scope), &hooks, scope)?;

    if scope == HookScope::Project {
        println!("updated {updated} hook(s) for {event}: {command}");
    } else {
        println!("updated {updated} {scope} hook(s) for {event}: {command}");
    }

    Ok(())
}

fn format_hook_list(project: &Project) -> String {
    if project.global_hooks.hooks.is_empty() && project.project_hooks.hooks.is_empty() {
        return "no hooks configured\n".to_owned();
    }

    let mut output = String::from("source  event  command\n");
    append_hook_rows(&mut output, "global", &project.global_hooks);
    append_hook_rows(&mut output, "project", &project.project_hooks);
    output
}

fn append_hook_rows(output: &mut String, source: &str, config: &HookConfig) {
    for hook in &config.hooks {
        output.push_str(&format!("{source}  {}  {}\n", hook.on, hook.run));
    }
}

fn command_from_parts(command_parts: &[String]) -> String {
    if command_parts.len() == 1 {
        return command_parts[0].clone();
    }

    command_parts
        .iter()
        .map(|part| shell_word(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(part: &str) -> String {
    if !part.is_empty() && !part.chars().any(char::is_whitespace) {
        return part.to_owned();
    }

    let mut quoted = String::with_capacity(part.len() + 2);
    quoted.push('"');
    for character in part.chars() {
        match character {
            '"' | '\\' | '`' => {
                quoted.push('\\');
                quoted.push(character);
            }
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

fn validate_event(project: &Project, event: &str) -> Result<(), HookError> {
    if event.is_empty() || event.chars().any(char::is_whitespace) {
        return Err(HookError::InvalidEventName);
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let valid_events = valid_hook_events(&config);

    if valid_events.iter().any(|valid| valid == event) {
        Ok(())
    } else {
        Err(HookError::UnknownEvent {
            event: event.to_owned(),
            valid_events,
        })
    }
}

fn valid_hook_events(config: &Config) -> Vec<String> {
    ["*", TASK_ADDED_EVENT, ALL_TASKS_DONE_EVENT, "failed"]
        .into_iter()
        .map(str::to_owned)
        .chain(config.phases.keys().cloned())
        .collect()
}

fn map_config_error(error: ConfigError) -> HookError {
    match error {
        ConfigError::NotFound { path } => HookError::ConfigNotFound { path },
        ConfigError::Read { path, source } => HookError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => HookError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => HookError::InvalidConfig { path, reason },
    }
}

fn project_hooks_path(project: &Project) -> PathBuf {
    project.state_dir.join("hooks.toml")
}

fn global_hooks_path(project: &Project) -> PathBuf {
    project
        .state_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.state_dir.clone())
        .join("config.toml")
}

fn scoped_hooks(project: &Project, scope: HookScope) -> &HookConfig {
    match scope {
        HookScope::Project => &project.project_hooks,
        HookScope::Global => &project.global_hooks,
    }
}

fn scoped_hooks_path(project: &Project, scope: HookScope) -> PathBuf {
    match scope {
        HookScope::Project => project_hooks_path(project),
        HookScope::Global => global_hooks_path(project),
    }
}

fn save_scoped_hooks(path: &Path, hooks: &HookConfig, scope: HookScope) -> Result<(), HookError> {
    match scope {
        HookScope::Project => save_hooks(path, hooks)?,
        HookScope::Global => save_hooks_preserving_toml(path, hooks)?,
    }

    Ok(())
}

fn delete_hooks_file(path: &PathBuf) -> Result<(), HookError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(HookError::Delete {
            path: path.to_path_buf(),
            source,
        }),
    }
}
