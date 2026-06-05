use std::path::PathBuf;

use thiserror::Error;

use crate::core::{
    config::{Config, ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum RemoveError {
    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} is {state} and cannot be removed")]
    NotPending { id: String, state: String },

    #[error("task {id} is depended on by {dependent_id}")]
    HasDependents { id: String, dependent_id: String },

    #[error("config file not found: {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("failed to load config {path}: {message}")]
    ConfigLoad { path: PathBuf, message: String },

    #[error("invalid config {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },

    #[error(transparent)]
    StoreError(#[from] StoreError),
}

pub fn run_remove(project: &Project, id: &str) -> Result<(), RemoveError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let pending_phase = pending_phase(&config, config_path)?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    remove_task_flow(&mut store, &pending_phase, id)
}

fn remove_task_flow(
    store: &mut TaskStore,
    pending_phase: &str,
    id: &str,
) -> Result<(), RemoveError> {
    let current_state = {
        let task = store
            .get_task(id)
            .ok_or_else(|| RemoveError::TaskNotFound { id: id.to_owned() })?;
        task.state.clone()
    };

    if current_state != pending_phase {
        return Err(RemoveError::NotPending {
            id: id.to_owned(),
            state: current_state,
        });
    }

    if let Some(dependent) = store
        .all_tasks()
        .iter()
        .find(|t| t.id != id && t.dependencies.iter().any(|dep| dep == id))
    {
        return Err(RemoveError::HasDependents {
            id: id.to_owned(),
            dependent_id: dependent.id.clone(),
        });
    }

    if let Err(error) = store.remove_task(id) {
        return Err(map_store_error(error, store, id));
    }

    print_remove_output(&format!("{id} removed"));
    Ok(())
}

fn pending_phase(config: &Config, path: PathBuf) -> Result<String, RemoveError> {
    config
        .phases
        .first()
        .map(|phase| phase.name.clone())
        .ok_or_else(|| RemoveError::InvalidConfig {
            path,
            message: "phases must not be empty".to_owned(),
        })
}

fn map_config_error(error: ConfigError) -> RemoveError {
    match error {
        ConfigError::NotFound { path } => RemoveError::ConfigNotFound { path },
        ConfigError::Read { path, source } => RemoveError::ConfigLoad {
            path,
            message: source.to_string(),
        },
        ConfigError::Parse { path, source } => RemoveError::ConfigLoad {
            path,
            message: source.to_string(),
        },
        ConfigError::Invalid { path, reason } => RemoveError::InvalidConfig {
            path,
            message: reason,
        },
    }
}

fn map_store_error(error: StoreError, store: &TaskStore, id: &str) -> RemoveError {
    match error {
        StoreError::NotFound => RemoveError::TaskNotFound { id: id.to_owned() },
        StoreError::NotInPendingPhase { .. } => match store.get_task(id) {
            Some(task) => RemoveError::NotPending {
                id: id.to_owned(),
                state: task.state.clone(),
            },
            None => RemoveError::TaskNotFound { id: id.to_owned() },
        },
        StoreError::DependedOnBy { dependent_id, .. } => RemoveError::HasDependents {
            id: id.to_owned(),
            dependent_id,
        },
        other => RemoveError::StoreError(other),
    }
}

fn print_remove_output(message: &str) {
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
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                },
            ],
            default_model: None,
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

    fn capture_output<F>(run: F) -> (Result<(), RemoveError>, String)
    where
        F: FnOnce() -> Result<(), RemoveError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn successful_remove_deletes_pending_task_and_prints_output() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();
        store.add_task("Second", "", None, vec![], None).unwrap();

        let (result, output) = capture_output(|| run_remove(&project, "task-001"));
        result.unwrap();

        assert_eq!(output, "task-001 removed\n");

        let store = test_store(&temp_dir, &config);
        assert!(store.get_task("task-001").is_none());
        assert!(store.get_task("task-002").is_some());
        assert_eq!(store.all_tasks().len(), 1);
    }

    #[test]
    fn removing_nonexistent_task_returns_not_found() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_remove(&project, "task-999").unwrap_err();

        assert_eq!(error.to_string(), "task task-999 not found");
        match error {
            RemoveError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn removing_non_pending_task_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_remove(&project, "task-001").unwrap_err();

        assert_eq!(
            error.to_string(),
            "task task-001 is enriching and cannot be removed"
        );
        match error {
            RemoveError::NotPending { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "enriching");
            }
            other => panic!("unexpected error: {other}"),
        }

        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap().state, "enriching");
    }

    #[test]
    fn missing_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_remove(&project, "task-001").unwrap_err();

        assert!(matches!(error, RemoveError::ConfigNotFound { .. }));
    }

    #[test]
    fn malformed_config_returns_load_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        fs::write(project.state_dir.join("config.json"), "{").unwrap();

        let error = run_remove(&project, "task-001").unwrap_err();

        assert!(matches!(error, RemoveError::ConfigLoad { .. }));
    }

    #[test]
    fn empty_phase_list_returns_invalid_config() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        let mut config = test_config();
        config.phases.clear();

        let error = pending_phase(&config, path).unwrap_err();

        assert!(matches!(error, RemoveError::InvalidConfig { .. }));
    }

    #[test]
    fn removing_task_with_dependents_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Dep", "", None, vec![], None).unwrap();
        store
            .add_task("Dependent", "", None, vec!["task-001".to_owned()], None)
            .unwrap();

        let error = run_remove(&project, "task-001").unwrap_err();

        assert_eq!(
            error.to_string(),
            "task task-001 is depended on by task-002"
        );
        match error {
            RemoveError::HasDependents { id, dependent_id } => {
                assert_eq!(id, "task-001");
                assert_eq!(dependent_id, "task-002");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn removing_blocked_task_returns_not_pending() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let error = run_remove(&project, "task-001").unwrap_err();

        assert!(matches!(error, RemoveError::NotPending { .. }));
    }

    #[test]
    fn removing_done_task_returns_not_pending() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_remove(&project, "task-001").unwrap_err();

        assert!(matches!(error, RemoveError::NotPending { .. }));
    }
}
