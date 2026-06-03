use std::{io, path::PathBuf};

use chrono::Utc;
use thiserror::Error;

use crate::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum DoneError {
    #[error("--artifact is required")]
    MissingArtifact,

    #[error("artifact must not be empty")]
    EmptyArtifact,

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

pub fn run_done(project: &Project, id: &str, artifact: Option<&str>) -> Result<(), DoneError> {
    let artifact = artifact.ok_or(DoneError::MissingArtifact)?;
    if artifact.trim().is_empty() {
        return Err(DoneError::EmptyArtifact);
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase =
        config
            .state_machine
            .last()
            .cloned()
            .ok_or_else(|| DoneError::InvalidConfig {
                path: config_path.clone(),
                reason: "state_machine must not be empty".to_owned(),
            })?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    let task = store
        .get_task(id)
        .ok_or_else(|| DoneError::TaskNotFound { id: id.to_owned() })?;
    let current_phase = task.state.clone();
    let dependencies = task.dependencies.clone();

    if current_phase == terminal_phase || current_phase == "failed" {
        return Err(DoneError::AlreadyTerminal {
            id: id.to_owned(),
            state: current_phase,
        });
    }

    let current_index = config
        .state_machine
        .iter()
        .position(|phase| phase == &current_phase)
        .ok_or_else(|| StoreError::InvalidTransition {
            from: current_phase.clone(),
            to: String::new(),
        })?;
    let target_index = current_index + 1;

    if target_index > 0 {
        for dependency_id in &dependencies {
            match store.get_task(dependency_id) {
                Some(dependency) if dependency.state == terminal_phase => {}
                _ => {
                    return Err(StoreError::DependencyBlocked {
                        task_id: id.to_owned(),
                        blocking_id: dependency_id.clone(),
                    }
                    .into());
                }
            }
        }
    }

    let completed_at = Utc::now().to_rfc3339();
    store.record_phase_artifact(id, artifact, completed_at)?;
    store.next_phase(id)?;

    let resulting_state = store.get_task(id).unwrap().state.clone();
    if resulting_state == terminal_phase {
        print_done_output(&format!("{id} done ✓"));
    } else {
        print_done_output(&format!("{id} → {resulting_state}"));
    }

    Ok(())
}

fn map_config_error(error: ConfigError) -> DoneError {
    match error {
        ConfigError::NotFound { path } => DoneError::ConfigNotFound { path },
        ConfigError::Read { path, source } => DoneError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => DoneError::ConfigLoad { path, source },
    }
}

fn print_done_output(message: &str) {
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
    use std::{collections::BTreeMap, fs};

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{Config, VerificationConfig},
        global_config::GlobalConfig,
        tasks::{StoreError, TaskStore},
    };

    fn test_config() -> Config {
        Config {
            state_machine: vec![
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "done".to_owned(),
            ],
            max_retries: 3,
            models: BTreeMap::new(),
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            stack: "rust".to_owned(),
            default_model: "sonnet".to_owned(),
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

    fn capture_output<F>(run: F) -> (Result<(), DoneError>, String)
    where
        F: FnOnce() -> Result<(), DoneError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });

        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());

        (result, output)
    }

    #[test]
    fn missing_artifact_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_done(&project, "task-001", None).unwrap_err();

        assert!(matches!(error, DoneError::MissingArtifact));
    }

    #[test]
    fn empty_artifact_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        for artifact in ["", "  "] {
            let error = run_done(&project, "task-001", Some(artifact)).unwrap_err();

            assert!(matches!(error, DoneError::EmptyArtifact));
        }
    }

    #[test]
    fn nonfinalterminal_advance_records_artifact_and_prints_arrow() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();

        let (result, output) =
            capture_output(|| run_done(&project, "task-001", Some("tests pass")));
        result.unwrap();

        assert_eq!(output, "task-001 → in_progress\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "in_progress");

        let phase = task.phases.get("enriching").unwrap();
        assert_eq!(phase.artifact, "tests pass");
        assert!(DateTime::parse_from_rfc3339(&phase.completed_at).is_ok());
    }

    #[test]
    fn terminal_advance_prints_done_checkmark() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) =
            capture_output(|| run_done(&project, "task-001", Some("final artifact")));
        result.unwrap();

        assert_eq!(output, "task-001 done ✓\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "done");

        let phase = task.phases.get("in_progress").unwrap();
        assert_eq!(phase.artifact, "final artifact");
    }

    #[test]
    fn task_not_found_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_done(&project, "task-999", Some("artifact")).unwrap_err();

        match error {
            DoneError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn already_terminal_done_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_done(&project, "task-001", Some("too late")).unwrap_err();

        match error {
            DoneError::AlreadyTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "done");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn already_failed_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.fail_task("task-001", "failed").unwrap();

        let error = run_done(&project, "task-001", Some("too late")).unwrap_err();

        match error {
            DoneError::AlreadyTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "failed");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn dependency_blocked_does_not_record_artifact() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Dependency", "", None, vec![]).unwrap();
        store
            .add_task("Dependent", "", None, vec!["task-001".to_owned()])
            .unwrap();

        let error = run_done(&project, "task-002", Some("blocked artifact")).unwrap_err();

        assert!(matches!(
            error,
            DoneError::StoreError(StoreError::DependencyBlocked { .. })
        ));

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-002").unwrap();
        assert!(task.phases.is_empty());
    }
}
