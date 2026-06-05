use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    pub on: String,
    pub run: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

pub struct HookContext<'a> {
    pub task_id: &'a str,
    pub task_title: &'a str,
    pub project_slug: &'a str,
    pub from_phase: &'a str,
    pub to_phase: &'a str,
    pub artifact: &'a str,
}

#[derive(Debug, Error)]
pub enum HookConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid hooks config at {path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("failed to serialize hooks config for {path}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn load_hooks(path: &Path) -> Result<HookConfig, HookConfigError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(HookConfig::default());
        }
        Err(source) => {
            return Err(HookConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    toml::from_str::<HookConfig>(&contents).map_err(|error| HookConfigError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn save_hooks(path: &Path, config: &HookConfig) -> Result<(), HookConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| HookConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let contents = toml::to_string_pretty(config).map_err(|source| HookConfigError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    let temporary_path = path.with_extension("toml.tmp");

    fs::write(&temporary_path, contents)
        .and_then(|_| fs::rename(&temporary_path, path))
        .map_err(|source| HookConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
fn collect_hooks(
    agira_root: &Path,
    project_slug: &str,
    to_phase: &str,
) -> Result<Vec<HookEntry>, HookConfigError> {
    let global_hooks = load_hooks(&agira_root.join("config.toml"))?;
    let project_hooks = load_hooks(&agira_root.join(project_slug).join("hooks.toml"))?;

    Ok(hooks_for_phase(&global_hooks, &project_hooks, to_phase))
}

pub fn hooks_for_phase(
    global_hooks: &HookConfig,
    project_hooks: &HookConfig,
    to_phase: &str,
) -> Vec<HookEntry> {
    matching_hooks(global_hooks, to_phase)
        .chain(matching_hooks(project_hooks, to_phase))
        .cloned()
        .collect()
}

pub fn dispatch_hooks(hooks: &[HookEntry], ctx: &HookContext<'_>) {
    for hook in hooks {
        if let Err(error) = Command::new("sh")
            .arg("-c")
            .arg(&hook.run)
            .env("AGIRA_TASK_ID", ctx.task_id)
            .env("AGIRA_TASK_TITLE", ctx.task_title)
            .env("AGIRA_PROJECT_SLUG", ctx.project_slug)
            .env("AGIRA_FROM_PHASE", ctx.from_phase)
            .env("AGIRA_TO_PHASE", ctx.to_phase)
            .env("AGIRA_ARTIFACT", ctx.artifact)
            .spawn()
        {
            eprintln!("warning: hook spawn failed: {error}");
        }
    }
}

fn matching_hooks<'a>(
    config: &'a HookConfig,
    to_phase: &'a str,
) -> impl Iterator<Item = &'a HookEntry> {
    config
        .hooks
        .iter()
        .filter(move |hook| hook.on == "*" || hook.on == to_phase)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn absent_file_returns_empty_config() {
        let temp_dir = TempDir::new().unwrap();

        let config = load_hooks(&temp_dir.path().join("hooks.toml")).unwrap();

        assert!(config.hooks.is_empty());
    }

    #[test]
    fn valid_toml_parses_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(
            &path,
            r#"
[[hooks]]
on = "done"
run = "echo done"

[[hooks]]
on = "*"
run = "echo changed"
"#,
        )
        .unwrap();

        let config = load_hooks(&path).unwrap();

        assert_eq!(
            config,
            HookConfig {
                hooks: vec![
                    HookEntry {
                        on: "done".to_owned(),
                        run: "echo done".to_owned(),
                    },
                    HookEntry {
                        on: "*".to_owned(),
                        run: "echo changed".to_owned(),
                    },
                ],
            }
        );
    }

    #[test]
    fn save_hooks_writes_toml_that_load_hooks_reads_back_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        let config = HookConfig {
            hooks: vec![
                HookEntry {
                    on: "done".to_owned(),
                    run: "printf ok".to_owned(),
                },
                HookEntry {
                    on: "*".to_owned(),
                    run: "echo all".to_owned(),
                },
            ],
        };

        save_hooks(&path, &config).unwrap();
        let loaded = load_hooks(&path).unwrap();

        assert_eq!(loaded, config);
    }

    #[test]
    fn save_hooks_creates_parent_directories_as_needed() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nested").join("hooks.toml");
        let config = HookConfig {
            hooks: vec![HookEntry {
                on: "failed".to_owned(),
                run: "echo fail".to_owned(),
            }],
        };

        save_hooks(&path, &config).unwrap();

        assert_eq!(load_hooks(&path).unwrap(), config);
    }

    #[test]
    fn malformed_toml_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(&path, "[[hooks]]\non = [").unwrap();

        let error = load_hooks(&path).unwrap_err();

        match error {
            HookConfigError::Parse {
                path: error_path, ..
            } => assert_eq!(error_path, path),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn collect_hooks_matches_wildcard_for_any_phase() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "*"
run = "echo global"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "verifying").unwrap();

        assert_eq!(
            hooks,
            vec![HookEntry {
                on: "*".to_owned(),
                run: "echo global".to_owned(),
            }]
        );
    }

    #[test]
    fn collect_hooks_filters_specific_phase_in_global_first_order() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo global done"

[[hooks]]
on = "failed"
run = "echo global failed"
"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo project done"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "done").unwrap();

        assert_eq!(
            hooks,
            vec![
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo global done".to_owned(),
                },
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo project done".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn collect_hooks_excludes_non_matching_hooks() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo done"
"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
on = "failed"
run = "echo failed"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "in_progress").unwrap();

        assert!(hooks.is_empty());
    }

    #[test]
    fn collect_hooks_absent_project_file_returns_empty_list() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            "default_max_retries = 3\n",
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "done").unwrap();

        assert!(hooks.is_empty());
    }
}
