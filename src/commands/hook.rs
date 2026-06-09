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
        .chain(config.phases().iter().map(|phase| phase.name.clone()))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::core::{
        config::{Config, PhaseConfig, load_project_config},
        global_config::GlobalConfig,
        hooks::{HookConfig, HookEntry, load_hooks},
        project::Project,
    };

    use super::*;

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "verifying".to_owned(),
                    model: Some("haiku".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            None,
            3,
        )
    }

    fn write_config(project: &Project) {
        let contents = serde_json::to_vec_pretty(&test_config()).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn test_project(
        temp_dir: &TempDir,
        global_hooks: HookConfig,
        project_hooks: HookConfig,
    ) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks,
            project_hooks,
        }
    }

    fn setup(global_hooks: HookConfig, project_hooks: HookConfig) -> (TempDir, Project) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir, global_hooks, project_hooks.clone());
        write_config(&project);
        if !project_hooks.hooks.is_empty() {
            crate::core::hooks::save_hooks(&project.state_dir.join("hooks.toml"), &project_hooks)
                .unwrap();
        }
        (temp_dir, project)
    }

    #[test]
    fn list_formats_global_then_project_hooks_with_event_and_command() {
        let (_temp_dir, project) = setup(
            HookConfig {
                hooks: vec![HookEntry {
                    on: "done".to_owned(),
                    run: "echo global".to_owned(),
                }],
            },
            HookConfig {
                hooks: vec![HookEntry {
                    on: "failed".to_owned(),
                    run: "echo project".to_owned(),
                }],
            },
        );

        let output = format_hook_list(&project);

        assert_eq!(
            output,
            "source  event  command\nglobal  done  echo global\nproject  failed  echo project\n"
        );
    }

    #[test]
    fn list_prints_no_hooks_configured_when_both_configs_are_empty() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        assert_eq!(format_hook_list(&project), "no hooks configured\n");
    }

    #[test]
    fn command_from_parts_preserves_single_string_and_quotes_spaced_arguments() {
        assert_eq!(command_from_parts(&["echo all".to_owned()]), "echo all");
        assert_eq!(
            command_from_parts(&[
                "hermes".to_owned(),
                "chat".to_owned(),
                "--quiet".to_owned(),
                "-q".to_owned(),
                "/todo $AGIRA_PROJECT_PATH".to_owned(),
            ]),
            "hermes chat --quiet -q \"/todo $AGIRA_PROJECT_PATH\""
        );
    }

    #[test]
    fn add_accepts_star_task_added_all_tasks_done_failed_and_configured_phase_names() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        run_hook_add(&project, "*", &["echo all".to_owned()], false).unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(
            &project,
            "task_added",
            &["echo task_added".to_owned()],
            false,
        )
        .unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(
            &project,
            "all_tasks_done",
            &["echo all_tasks_done".to_owned()],
            false,
        )
        .unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(&project, "failed", &["echo failed".to_owned()], false).unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(&project, "done", &["echo done".to_owned()], false).unwrap();

        let hooks = load_hooks(&project.state_dir.join("hooks.toml")).unwrap();
        assert_eq!(
            hooks.hooks,
            vec![
                HookEntry {
                    on: "*".to_owned(),
                    run: "echo all".to_owned(),
                },
                HookEntry {
                    on: "task_added".to_owned(),
                    run: "echo task_added".to_owned(),
                },
                HookEntry {
                    on: "all_tasks_done".to_owned(),
                    run: "echo all_tasks_done".to_owned(),
                },
                HookEntry {
                    on: "failed".to_owned(),
                    run: "echo failed".to_owned(),
                },
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo done".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn add_rejects_unknown_event_not_in_phases_and_rejects_empty_command() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        let error =
            run_hook_add(&project, "review", &["echo review".to_owned()], false).unwrap_err();
        assert!(matches!(error, HookError::UnknownEvent { event, .. } if event == "review"));

        let error = run_hook_add(&project, "done", &["   ".to_owned()], false).unwrap_err();
        assert!(matches!(error, HookError::EmptyCommand));
    }

    #[test]
    fn add_appends_to_existing_project_hooks_without_altering_global_hooks() {
        let global_hooks = HookConfig {
            hooks: vec![HookEntry {
                on: "done".to_owned(),
                run: "echo global".to_owned(),
            }],
        };
        let project_hooks = HookConfig {
            hooks: vec![HookEntry {
                on: "failed".to_owned(),
                run: "echo existing".to_owned(),
            }],
        };
        let (_temp_dir, project) = setup(global_hooks.clone(), project_hooks);

        run_hook_add(&project, "done", &["echo project".to_owned()], false).unwrap();

        let hooks = load_hooks(&project.state_dir.join("hooks.toml")).unwrap();
        assert_eq!(
            hooks.hooks,
            vec![
                HookEntry {
                    on: "failed".to_owned(),
                    run: "echo existing".to_owned(),
                },
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo project".to_owned(),
                },
            ]
        );
        assert_eq!(project.global_hooks, global_hooks);
    }

    #[test]
    fn remove_removes_all_project_hooks_for_event_and_leaves_other_events_intact() {
        let project_hooks = HookConfig {
            hooks: vec![
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo first".to_owned(),
                },
                HookEntry {
                    on: "failed".to_owned(),
                    run: "echo failed".to_owned(),
                },
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo second".to_owned(),
                },
            ],
        };
        let (_temp_dir, project) = setup(HookConfig::default(), project_hooks);

        run_hook_remove(&project, "done", false).unwrap();

        let hooks = load_hooks(&project.state_dir.join("hooks.toml")).unwrap();
        assert_eq!(
            hooks.hooks,
            vec![HookEntry {
                on: "failed".to_owned(),
                run: "echo failed".to_owned(),
            }]
        );
    }

    #[test]
    fn remove_rejects_valid_events_with_no_matching_project_hook() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        let error = run_hook_remove(&project, "done", false).unwrap_err();

        assert!(matches!(error, HookError::HookNotFound { event, .. } if event == "done"));
    }

    #[test]
    fn remove_rejects_unknown_events() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        let error = run_hook_remove(&project, "review", false).unwrap_err();

        assert!(matches!(error, HookError::UnknownEvent { event, .. } if event == "review"));
    }

    #[test]
    fn remove_deletes_hooks_file_when_no_project_hooks_remain() {
        let project_hooks = HookConfig {
            hooks: vec![HookEntry {
                on: "done".to_owned(),
                run: "echo done".to_owned(),
            }],
        };
        let (_temp_dir, project) = setup(HookConfig::default(), project_hooks);

        run_hook_remove(&project, "done", false).unwrap();

        assert!(!project.state_dir.join("hooks.toml").exists());
    }

    #[test]
    fn valid_events_include_current_project_config_phases() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());
        let config = load_project_config(
            &project.state_dir.join("config.json"),
            &project.global_config,
        )
        .unwrap();

        assert_eq!(
            valid_hook_events(&config),
            vec![
                "*".to_owned(),
                "task_added".to_owned(),
                "all_tasks_done".to_owned(),
                "failed".to_owned(),
                "pending".to_owned(),
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "verifying".to_owned(),
                "done".to_owned(),
            ]
        );
    }
}
