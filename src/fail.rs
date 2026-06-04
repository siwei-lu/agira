use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum FailError {
    #[error("--reason is required")]
    MissingReason,

    #[error("reason must not be empty")]
    EmptyReason,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} is already failed")]
    AlreadyFailed { id: String },

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

pub fn run_fail(project: &Project, id: &str, reason: Option<&str>) -> Result<(), FailError> {
    let reason = validate_reason(reason)?;
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase = config
        .terminal_phase()
        .ok_or_else(|| FailError::InvalidConfig {
            path: config_path.clone(),
            reason: "phases must not be empty".to_owned(),
        })?
        .to_owned();

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    fail_task_flow(&mut store, &terminal_phase, id, reason)
}

fn fail_task_flow(
    store: &mut TaskStore,
    terminal_phase: &str,
    id: &str,
    reason: &str,
) -> Result<(), FailError> {
    let task = store
        .get_task(id)
        .ok_or_else(|| FailError::TaskNotFound { id: id.to_owned() })?;
    let current_state = task.state.clone();
    let retry_count = task.retry_count;
    let max_retries = task.max_retries;

    if current_state == "failed" {
        return Err(FailError::AlreadyFailed { id: id.to_owned() });
    }

    if current_state == terminal_phase {
        return Err(FailError::AlreadyTerminal {
            id: id.to_owned(),
            state: current_state,
        });
    }

    let next_retry_count = retry_count.saturating_add(1);
    if next_retry_count < max_retries {
        let (new_retry_count, max_retries) = match store.retry_task(id, reason) {
            Ok(result) => result,
            Err(error) => return Err(map_store_error(error, store, id)),
        };
        print_fail_output(&format!(
            "{id} retrying ({new_retry_count}/{max_retries}): {reason}"
        ));
    } else {
        let failure_reason = format!("failed (max retries): {reason}");
        if let Err(error) = store.fail_task(id, &failure_reason) {
            return Err(map_store_error(error, store, id));
        }
        print_fail_output(&format!("{id} failed — max retries reached"));
    }

    Ok(())
}

fn validate_reason(reason: Option<&str>) -> Result<&str, FailError> {
    let reason = reason.ok_or(FailError::MissingReason)?;
    if reason.trim().is_empty() {
        return Err(FailError::EmptyReason);
    }

    Ok(reason)
}

fn map_config_error(error: ConfigError) -> FailError {
    match error {
        ConfigError::NotFound { path } => FailError::ConfigNotFound { path },
        ConfigError::Read { path, source } => FailError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => FailError::ConfigLoad { path, source },
    }
}

fn map_store_error(error: StoreError, store: &TaskStore, id: &str) -> FailError {
    match error {
        StoreError::NotFound => FailError::TaskNotFound { id: id.to_owned() },
        StoreError::AlreadyTerminal => match store.get_task(id) {
            Some(task) if task.state == "failed" => FailError::AlreadyFailed { id: id.to_owned() },
            Some(task) => FailError::AlreadyTerminal {
                id: id.to_owned(),
                state: task.state.clone(),
            },
            None => FailError::TaskNotFound { id: id.to_owned() },
        },
        other => FailError::StoreError(other),
    }
}

fn print_fail_output(message: &str) {
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

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::*;
    use crate::{
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

    fn capture_output<F>(run: F) -> (Result<(), FailError>, String)
    where
        F: FnOnce() -> Result<(), FailError>,
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

        let error = run_fail(&project, "task-001", None).unwrap_err();

        assert!(matches!(error, FailError::MissingReason));
        assert_eq!(error.to_string(), "--reason is required");
    }

    #[test]
    fn empty_reason_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        for reason in ["", "  "] {
            let error = run_fail(&project, "task-001", Some(reason)).unwrap_err();

            assert!(matches!(error, FailError::EmptyReason));
            assert_eq!(error.to_string(), "reason must not be empty");
        }
    }

    #[test]
    fn retry_decrements_below_max() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) =
            capture_output(|| run_fail(&project, "task-001", Some("compilation error")));
        result.unwrap();

        assert_eq!(output, "task-001 retrying (1/3): compilation error\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "enriching");
        assert_eq!(task.retry_count, 1);
        assert_eq!(task.max_retries, 3);

        let last = task.history.last().unwrap();
        assert_eq!(last.from.as_deref(), Some("in_progress"));
        assert_eq!(last.to, "enriching");
        assert_eq!(last.reason, "retry 1/3: compilation error");
        assert!(DateTime::parse_from_rfc3339(&last.timestamp).is_ok());
    }

    #[test]
    fn retry_when_next_count_is_still_below_max() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store
            .retry_task("task-001", "first transient failure")
            .unwrap();

        let (result, output) =
            capture_output(|| run_fail(&project, "task-001", Some("second transient failure")));
        result.unwrap();

        assert_eq!(
            output,
            "task-001 retrying (2/3): second transient failure\n"
        );

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "enriching");
        assert_eq!(task.retry_count, 2);
        assert_eq!(
            task.history.last().unwrap().reason,
            "retry 2/3: second transient failure"
        );
    }

    #[test]
    fn terminal_fail_when_at_max_minus_one() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();

        capture_output(|| run_fail(&project, "task-001", Some("first failure")))
            .0
            .unwrap();
        capture_output(|| run_fail(&project, "task-001", Some("second failure")))
            .0
            .unwrap();

        let (result, output) =
            capture_output(|| run_fail(&project, "task-001", Some("still broken")));
        result.unwrap();

        assert_eq!(output, "task-001 failed — max retries reached\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "failed");
        assert_eq!(task.retry_count, 2);

        let last = task.history.last().unwrap();
        assert_eq!(last.to, "failed");
        assert_eq!(last.reason, "failed (max retries): still broken");
        assert!(DateTime::parse_from_rfc3339(&last.timestamp).is_ok());
    }

    #[test]
    fn terminal_fail_when_at_max() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.retry_task("task-001", "one").unwrap();
        store.retry_task("task-001", "two").unwrap();
        store.retry_task("task-001", "three").unwrap();

        let (result, output) =
            capture_output(|| run_fail(&project, "task-001", Some("still broken")));
        result.unwrap();

        assert_eq!(output, "task-001 failed — max retries reached\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "failed");
        assert_eq!(task.retry_count, 3);
        assert_eq!(
            task.history.last().unwrap().reason,
            "failed (max retries): still broken"
        );
    }

    #[test]
    fn already_failed_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.fail_task("task-001", "failed").unwrap();
        let before = store.get_task("task-001").unwrap().clone();

        let error = run_fail(&project, "task-001", Some("too late")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already failed");
        match error {
            FailError::AlreadyFailed { id } => assert_eq!(id, "task-001"),
            other => panic!("unexpected error: {other}"),
        }

        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap(), &before);
    }

    #[test]
    fn already_terminal_done_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        let before = store.get_task("task-001").unwrap().clone();

        let error = run_fail(&project, "task-001", Some("too late")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already done");
        match error {
            FailError::AlreadyTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "done");
            }
            other => panic!("unexpected error: {other}"),
        }

        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap(), &before);
    }

    #[test]
    fn task_not_found_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_fail(&project, "task-999", Some("x")).unwrap_err();

        assert_eq!(error.to_string(), "task task-999 not found");
        match error {
            FailError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_fail(&project, "task-001", Some("x")).unwrap_err();

        assert!(matches!(error, FailError::ConfigNotFound { .. }));
    }

    #[test]
    fn malformed_config_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        fs::write(project.state_dir.join("config.json"), "{").unwrap();

        let error = run_fail(&project, "task-001", Some("x")).unwrap_err();

        assert!(matches!(error, FailError::ConfigLoad { .. }));
    }

    #[test]
    fn empty_phases_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let mut config = test_config();
        config.phases.clear();
        write_config(&project, &config);

        let error = run_fail(&project, "task-001", Some("x")).unwrap_err();

        assert!(matches!(error, FailError::InvalidConfig { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "invalid config {}: phases must not be empty",
                project.state_dir.join("config.json").display()
            )
        );
    }
}
