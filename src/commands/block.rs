use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum BlockError {
    #[error("--reason is required")]
    MissingReason,

    #[error("--reason must not be empty")]
    EmptyReason,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} is already {state}")]
    AlreadyTerminal { id: String, state: String },

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

pub fn run_block(project: &Project, id: &str, reason: Option<&str>) -> Result<(), BlockError> {
    let reason = validate_reason(reason)?;
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase = config
        .terminal_phase()
        .ok_or_else(|| BlockError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?
        .to_owned();

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    block_task_flow(&mut store, &terminal_phase, id, reason)
}

fn block_task_flow(
    store: &mut TaskStore,
    terminal_phase: &str,
    id: &str,
    reason: &str,
) -> Result<(), BlockError> {
    let current_state = {
        let task = store
            .get_task(id)
            .ok_or_else(|| BlockError::TaskNotFound { id: id.to_owned() })?;
        task.state.clone()
    };

    if current_state == terminal_phase || current_state == "blocked" || current_state == "failed" {
        return Err(BlockError::AlreadyTerminal {
            id: id.to_owned(),
            state: current_state,
        });
    }

    if let Err(error) = store.block_task(id, reason) {
        return Err(map_store_error(error, store, id, terminal_phase));
    }

    print_block_output(&format!("{id} blocked: {reason}"));
    Ok(())
}

fn validate_reason(reason: Option<&str>) -> Result<&str, BlockError> {
    let reason = reason.ok_or(BlockError::MissingReason)?;
    if reason.trim().is_empty() {
        return Err(BlockError::EmptyReason);
    }
    Ok(reason)
}

fn map_config_error(error: ConfigError) -> BlockError {
    match error {
        ConfigError::NotFound { path } => BlockError::ConfigNotFound { path },
        ConfigError::Read { path, source } => BlockError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => BlockError::ConfigLoad { path, source },
    }
}

fn map_store_error(
    error: StoreError,
    store: &TaskStore,
    id: &str,
    _terminal_phase: &str,
) -> BlockError {
    match error {
        StoreError::NotFound => BlockError::TaskNotFound { id: id.to_owned() },
        StoreError::AlreadyBlocked | StoreError::AlreadyTerminal => match store.get_task(id) {
            Some(task) => BlockError::AlreadyTerminal {
                id: id.to_owned(),
                state: task.state.clone(),
            },
            None => BlockError::TaskNotFound { id: id.to_owned() },
        },
        other => BlockError::StoreError(other),
    }
}

fn print_block_output(message: &str) {
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
                    name: "enriching".to_owned(),
                    model: "opus".to_owned(),
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: "sonnet".to_owned(),
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

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
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

    fn capture_output<F>(run: F) -> (Result<(), BlockError>, String)
    where
        F: FnOnce() -> Result<(), BlockError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn missing_reason_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_block(&project, "task-001", None).unwrap_err();
        assert!(matches!(error, BlockError::MissingReason));
        assert_eq!(error.to_string(), "--reason is required");
    }

    #[test]
    fn empty_reason_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        for reason in ["", "  "] {
            let error = run_block(&project, "task-001", Some(reason)).unwrap_err();
            assert!(matches!(error, BlockError::EmptyReason));
            assert_eq!(error.to_string(), "--reason must not be empty");
        }
    }

    #[test]
    fn successful_block_persists_state_and_prints_output() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();

        let (result, output) =
            capture_output(|| run_block(&project, "task-001", Some("waiting on api")));
        result.unwrap();

        assert_eq!(output, "task-001 blocked: waiting on api\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "blocked");
        assert_eq!(task.blocked_at_phase.as_deref(), Some("enriching"));
        assert_eq!(task.blocked_reason.as_deref(), Some("waiting on api"));
    }

    #[test]
    fn blocking_done_task_returns_already_terminal() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_block(&project, "task-001", Some("reason")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already done");
        match error {
            BlockError::AlreadyTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "done");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn blocking_already_blocked_task_returns_already_terminal() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.block_task("task-001", "first").unwrap();

        let error = run_block(&project, "task-001", Some("second")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already blocked");
    }

    #[test]
    fn blocking_failed_task_returns_already_terminal() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.fail_task("task-001", "broken").unwrap();

        let error = run_block(&project, "task-001", Some("reason")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already failed");
    }

    #[test]
    fn blocking_nonexistent_task_returns_not_found() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_block(&project, "task-999", Some("reason")).unwrap_err();

        assert_eq!(error.to_string(), "task task-999 not found");
        match error {
            BlockError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn missing_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_block(&project, "task-001", Some("x")).unwrap_err();

        assert!(matches!(error, BlockError::ConfigNotFound { .. }));
    }
}
