use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum LockError {
    #[error("task {id} not found")]
    TaskNotFound { id: String },

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
    StoreError(#[from] StoreError),
}

pub fn run_lock(project: &Project, id: &str) -> Result<(), LockError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    config
        .terminal_phase()
        .ok_or_else(|| LockError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;

    store.lock_task(id).map_err(|e| match e {
        StoreError::NotFound => LockError::TaskNotFound { id: id.to_owned() },
        other => LockError::StoreError(other),
    })?;

    print_lock_output(&format!("{id} locked"));
    Ok(())
}

fn map_config_error(error: ConfigError) -> LockError {
    match error {
        ConfigError::NotFound { path } => LockError::ConfigNotFound { path },
        ConfigError::Read { path, source } => LockError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => LockError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => LockError::InvalidConfig { path, reason },
    }
}

fn print_lock_output(message: &str) {
    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(message);
            output.push('\n');
        }
    });

    println!("{message}");
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

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
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
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

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        }
    }

    fn write_config(project: &Project, config: &Config) {
        let contents = serde_json::to_vec_pretty(config).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    fn test_project_with_config() -> (TempDir, Project, Config) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        write_config(&project, &config);
        (temp_dir, project, config)
    }

    fn capture_output<F>(run: F) -> (Result<(), LockError>, String)
    where
        F: FnOnce() -> Result<(), LockError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn successful_lock_persists_and_prints_output() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();

        let (result, output) = capture_output(|| run_lock(&project, "task-001"));
        result.unwrap();

        assert_eq!(output, "task-001 locked\n");

        let store = test_store(&temp_dir, &config);
        assert!(store.get_task("task-001").unwrap().locked_at.is_some());
    }

    #[test]
    fn locking_unknown_task_returns_not_found() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_lock(&project, "task-999").unwrap_err();

        assert_eq!(error.to_string(), "task task-999 not found");
        assert!(matches!(error, LockError::TaskNotFound { .. }));
    }

    #[test]
    fn missing_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_lock(&project, "task-001").unwrap_err();

        assert!(matches!(error, LockError::ConfigNotFound { .. }));
    }
}
