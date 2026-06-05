use std::{
    fs, io,
    path::{Path, PathBuf},
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

pub fn collect_hooks(
    agira_root: &Path,
    project_slug: &str,
    to_phase: &str,
) -> Result<Vec<HookEntry>, HookConfigError> {
    let global_hooks = load_hooks(&agira_root.join("config.toml"))?;
    let project_hooks = load_hooks(&agira_root.join(project_slug).join("hooks.toml"))?;

    Ok(matching_hooks(&global_hooks, to_phase)
        .chain(matching_hooks(&project_hooks, to_phase))
        .cloned()
        .collect())
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
