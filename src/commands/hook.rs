use std::{fs, io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{Config, ConfigError, load_project_config},
    hooks::{HookConfig, HookConfigError, HookEntry, save_hooks},
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

    #[error("no project hook configured for {event}")]
    HookNotFound { event: String },

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

    #[error(transparent)]
    Hooks(#[from] HookConfigError),

    #[error("failed to delete {path}")]
    Delete {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn run_hook_list(project: &Project) -> Result<(), HookError> {
    print!("{}", format_hook_list(project));

    Ok(())
}

pub fn run_hook_add(
    project: &Project,
    event: &str,
    command_parts: &[String],
) -> Result<(), HookError> {
    validate_event(project, event)?;
    let command = command_parts.join(" ");
    if command.trim().is_empty() {
        return Err(HookError::EmptyCommand);
    }

    let mut hooks = project.project_hooks.clone();
    hooks.hooks.push(HookEntry {
        on: event.to_owned(),
        run: command.clone(),
    });
    save_hooks(&project_hooks_path(project), &hooks)?;

    println!("added hook {event}: {command}");

    Ok(())
}

pub fn run_hook_remove(project: &Project, event: &str) -> Result<(), HookError> {
    validate_event(project, event)?;

    let mut hooks = project.project_hooks.clone();
    let before = hooks.hooks.len();
    hooks.hooks.retain(|hook| hook.on != event);
    let removed = before - hooks.hooks.len();

    if removed == 0 {
        return Err(HookError::HookNotFound {
            event: event.to_owned(),
        });
    }

    let hooks_path = project_hooks_path(project);
    if hooks.hooks.is_empty() {
        delete_hooks_file(&hooks_path)?;
    } else {
        save_hooks(&hooks_path, &hooks)?;
    }

    println!("removed {removed} hook(s) for {event}");

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
    ["*", "failed"]
        .into_iter()
        .map(str::to_owned)
        .chain(config.phases.iter().map(|phase| phase.name.clone()))
        .collect()
}

fn map_config_error(error: ConfigError) -> HookError {
    match error {
        ConfigError::NotFound { path } => HookError::ConfigNotFound { path },
        ConfigError::Read { path, source } => HookError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => HookError::ConfigLoad { path, source },
    }
}

fn project_hooks_path(project: &Project) -> PathBuf {
    project.state_dir.join("hooks.toml")
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
        config::{Config, PhaseConfig, VerificationConfig, load_project_config},
        global_config::GlobalConfig,
        hooks::{HookConfig, HookEntry, load_hooks},
        project::Project,
    };

    use super::*;

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
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
                    name: "done".to_owned(),
                    model: "haiku".to_owned(),
                },
            ],
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
        }
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
    fn add_accepts_star_failed_and_configured_phase_names() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        run_hook_add(&project, "*", &["echo all".to_owned()]).unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(&project, "failed", &["echo failed".to_owned()]).unwrap();
        let project = Project {
            project_hooks: load_hooks(&project.state_dir.join("hooks.toml")).unwrap(),
            ..project
        };
        run_hook_add(&project, "done", &["echo done".to_owned()]).unwrap();

        let hooks = load_hooks(&project.state_dir.join("hooks.toml")).unwrap();
        assert_eq!(
            hooks.hooks,
            vec![
                HookEntry {
                    on: "*".to_owned(),
                    run: "echo all".to_owned(),
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

        let error = run_hook_add(&project, "review", &["echo review".to_owned()]).unwrap_err();
        assert!(matches!(error, HookError::UnknownEvent { event, .. } if event == "review"));

        let error = run_hook_add(&project, "done", &["   ".to_owned()]).unwrap_err();
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

        run_hook_add(&project, "done", &["echo project".to_owned()]).unwrap();

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

        run_hook_remove(&project, "done").unwrap();

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

        let error = run_hook_remove(&project, "done").unwrap_err();

        assert!(matches!(error, HookError::HookNotFound { event } if event == "done"));
    }

    #[test]
    fn remove_rejects_unknown_events() {
        let (_temp_dir, project) = setup(HookConfig::default(), HookConfig::default());

        let error = run_hook_remove(&project, "review").unwrap_err();

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

        run_hook_remove(&project, "done").unwrap();

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
                "failed".to_owned(),
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "verifying".to_owned(),
                "done".to_owned(),
            ]
        );
    }
}
