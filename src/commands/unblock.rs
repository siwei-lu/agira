use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum UnblockError {
    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} is not blocked")]
    NotBlocked { id: String },

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

pub fn run_unblock(project: &Project, id: &str) -> Result<(), UnblockError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    config
        .terminal_phase()
        .ok_or_else(|| UnblockError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    unblock_task_flow(&mut store, id)
}

fn unblock_task_flow(store: &mut TaskStore, id: &str) -> Result<(), UnblockError> {
    let (current_state, blocked_at_phase) = {
        let task = store
            .get_task(id)
            .ok_or_else(|| UnblockError::TaskNotFound { id: id.to_owned() })?;
        (task.state.clone(), task.blocked_at_phase.clone())
    };

    if current_state != "blocked" {
        return Err(UnblockError::NotBlocked { id: id.to_owned() });
    }

    if let Err(error) = store.unblock_task(id) {
        return Err(map_store_error(error, id));
    }

    let target_state = blocked_at_phase.unwrap_or_else(|| "unknown".to_owned());
    print_unblock_output(&format!("{id} unblocked: {target_state}"));
    Ok(())
}

fn map_config_error(error: ConfigError) -> UnblockError {
    match error {
        ConfigError::NotFound { path } => UnblockError::ConfigNotFound { path },
        ConfigError::Read { path, source } => UnblockError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => UnblockError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => UnblockError::InvalidConfig { path, reason },
    }
}

fn map_store_error(error: StoreError, id: &str) -> UnblockError {
    match error {
        StoreError::NotFound => UnblockError::TaskNotFound { id: id.to_owned() },
        StoreError::NotBlocked => UnblockError::NotBlocked { id: id.to_owned() },
        other => UnblockError::StoreError(other),
    }
}

fn print_unblock_output(message: &str) {
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig, VerificationConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
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
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            verification: VerificationConfig { commands: vec![] },
            max_retries: 3,
            prd_path: None,
        }
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

    fn capture_output<F>(run: F) -> (Result<(), UnblockError>, String)
    where
        F: FnOnce() -> Result<(), UnblockError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn successful_unblock_restores_phase_and_prints_output() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let (result, output) = capture_output(|| run_unblock(&project, "task-001"));
        result.unwrap();

        assert_eq!(output, "task-001 unblocked: enriching\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "enriching");
        assert!(task.blocked_at_phase.is_none());
        assert!(task.blocked_reason.is_none());
    }

    #[test]
    fn unblocking_non_blocked_task_returns_not_blocked() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();

        let error = run_unblock(&project, "task-001").unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is not blocked");
        match error {
            UnblockError::NotBlocked { id } => assert_eq!(id, "task-001"),
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn unblocking_nonexistent_task_returns_not_found() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_unblock(&project, "task-999").unwrap_err();

        assert_eq!(error.to_string(), "task task-999 not found");
    }

    #[test]
    fn missing_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_unblock(&project, "task-001").unwrap_err();

        assert!(matches!(error, UnblockError::ConfigNotFound { .. }));
    }
}
