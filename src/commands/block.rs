use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    hooks::{HookContext, dispatch_hooks, hooks_for_phase},
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
    block_task_flow(project, &mut store, &terminal_phase, id, reason)
}

fn block_task_flow(
    project: &Project,
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

    let task = store.get_task(id).unwrap();
    let from_phase = task.blocked_at_phase.as_deref().unwrap_or("").to_owned();
    dispatch_task_hooks(project, task, &from_phase, "blocked", reason);

    print_block_output(&format!("{id} blocked: {reason}"));
    Ok(())
}

fn dispatch_task_hooks(
    project: &Project,
    task: &crate::core::tasks::Task,
    from_phase: &str,
    to_phase: &str,
    artifact: &str,
) {
    let hooks = hooks_for_phase(&project.global_hooks, &project.project_hooks, to_phase);
    dispatch_hooks(
        &hooks,
        to_phase,
        &HookContext::new(
            task,
            &project.slug,
            &project.git_root,
            &project.state_dir,
            from_phase,
            to_phase,
            artifact,
        ),
        project.global_config.hook_debug,
    );
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
        ConfigError::Invalid { path, reason } => BlockError::InvalidConfig { path, reason },
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, thread, time::Duration};

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig},
        global_config::GlobalConfig,
        hooks::{HookConfig, HookEntry},
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

    fn test_project_with_hooks(temp_dir: &TempDir, hooks: Vec<HookEntry>) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig { hooks },
        }
    }

    fn read_file_eventually(path: &Path) -> String {
        for _ in 0..50 {
            match fs::read_to_string(path) {
                Ok(contents) if !contents.is_empty() => return contents,
                _ => {}
            }

            thread::sleep(Duration::from_millis(10));
        }

        panic!("hook output was not written to {}", path.display());
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
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();

        let (result, output) =
            capture_output(|| run_block(&project, "task-001", Some("waiting on api")));
        result.unwrap();

        assert_eq!(output, "task-001 blocked: waiting on api\n");

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "blocked");
        assert_eq!(task.blocked_at_phase.as_deref(), Some("pending"));
        assert_eq!(task.blocked_reason.as_deref(), Some("waiting on api"));
    }

    #[test]
    fn blocking_done_task_returns_already_terminal() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
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
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();
        store.block_task("task-001", "first").unwrap();

        let error = run_block(&project, "task-001", Some("second")).unwrap_err();

        assert_eq!(error.to_string(), "task task-001 is already blocked");
    }

    #[test]
    fn blocking_failed_task_returns_already_terminal() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();
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

    #[test]
    fn successful_block_fires_blocked_hook() {
        let temp_dir = TempDir::new().unwrap();
        let marker_path = temp_dir.path().join("blocked-marker.txt");
        let hooks = vec![HookEntry {
            on: "blocked".to_owned(),
            run: format!(
                "printf '%s' \"$AGIRA_FROM_PHASE/$AGIRA_TO_PHASE/$AGIRA_ARTIFACT\" > {}",
                marker_path.display()
            ),
        }];
        let project = test_project_with_hooks(&temp_dir, hooks);
        let config = test_config();
        write_config(&project, &config);
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("First", "", vec![], None, None, None)
            .unwrap();

        let (result, _output) =
            capture_output(|| run_block(&project, "task-001", Some("waiting on api")));
        result.unwrap();

        let contents = read_file_eventually(&marker_path);
        assert_eq!(contents, "pending/blocked/waiting on api");
    }
}
