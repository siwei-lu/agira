use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    hooks::{HookContext, dispatch_hooks, hooks_for_phase},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum CloseError {
    #[error("--reason is required")]
    MissingReason,

    #[error("--reason must not be empty")]
    EmptyReason,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} is not in a failed state")]
    NotFailed { id: String },

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

pub fn run_close(project: &Project, id: &str, reason: Option<&str>) -> Result<(), CloseError> {
    let reason = validate_reason(reason)?;
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase = config
        .terminal_phase()
        .ok_or_else(|| CloseError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?
        .to_owned();

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    close_task_flow(project, &mut store, &terminal_phase, id, reason)
}

fn close_task_flow(
    project: &Project,
    store: &mut TaskStore,
    terminal_phase: &str,
    id: &str,
    reason: &str,
) -> Result<(), CloseError> {
    let current_state = {
        let task = store
            .get_task(id)
            .ok_or_else(|| CloseError::TaskNotFound { id: id.to_owned() })?;
        task.state.clone()
    };

    if current_state != "failed" {
        return Err(CloseError::NotFailed { id: id.to_owned() });
    }

    if let Err(error) = store.close_task(id, reason, terminal_phase) {
        return Err(map_store_error(error, id));
    }

    let task = store.get_task(id).unwrap();
    dispatch_task_hooks(project, task, "failed", terminal_phase, reason);

    print_close_output(&format!("{id} closed: {reason}"));
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

fn validate_reason(reason: Option<&str>) -> Result<&str, CloseError> {
    let reason = reason.ok_or(CloseError::MissingReason)?;
    if reason.trim().is_empty() {
        return Err(CloseError::EmptyReason);
    }
    Ok(reason)
}

fn map_config_error(error: ConfigError) -> CloseError {
    match error {
        ConfigError::NotFound { path } => CloseError::ConfigNotFound { path },
        ConfigError::Read { path, source } => CloseError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => CloseError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => CloseError::InvalidConfig { path, reason },
    }
}

fn map_store_error(error: StoreError, id: &str) -> CloseError {
    match error {
        StoreError::NotFound => CloseError::TaskNotFound { id: id.to_owned() },
        StoreError::NotFailed { .. } => CloseError::NotFailed { id: id.to_owned() },
        other => CloseError::StoreError(other),
    }
}

fn print_close_output(message: &str) {
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
mod close_tests {
    use std::fs;

    use crate::core::{
        config::{Config, PhaseDef},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
        tasks::TaskStore,
    };

    use super::close_task_flow;

    fn make_project_store() -> (tempfile::TempDir, Project, TaskStore) {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let state_dir = dir.path().join(".agira");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            1,
        );
        let global_config = GlobalConfig::default();
        let project = Project {
            git_root: dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: state_dir.clone(),
            global_config,
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        };
        let store = TaskStore::new(&state_dir, &config).expect("create store");
        (dir, project, store)
    }

    // ── happy path ──────────────────────────────────────────────────────────────

    #[test]
    fn close_failed_task_transitions_to_done() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task(
                "close me",
                "description",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");

        // Put task into failed state.
        store
            .fail_task(&task.id, "some failure")
            .expect("fail task");

        close_task_flow(
            &project,
            &mut store,
            "done",
            &task.id,
            "manual verification passed",
        )
        .expect("close task flow");

        let task = store.get_task(&task.id).expect("get task");
        assert_eq!(task.state, "done");
    }

    #[test]
    fn close_records_history_entry() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task(
                "history check",
                "description",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");

        store
            .fail_task(&task.id, "some failure")
            .expect("fail task");

        close_task_flow(&project, &mut store, "done", &task.id, "manually verified")
            .expect("close task flow");

        let task = store.get_task(&task.id).expect("get task");
        let last_entry = task.history.last().expect("history entry");
        assert_eq!(last_entry.from.as_deref(), Some("failed"));
        assert_eq!(last_entry.to, "done");
        assert_eq!(last_entry.reason, "closed: manually verified");
    }

    // ── error: non-failed tasks ──────────────────────────────────────────────────

    #[test]
    fn close_pending_task_returns_not_failed() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task("pending task", "", Vec::new(), None, "default".to_owned())
            .expect("add task");

        let result = close_task_flow(&project, &mut store, "done", &task.id, "reason");
        assert!(
            matches!(result, Err(super::CloseError::NotFailed { .. })),
            "expected NotFailed, got: {result:?}"
        );
    }

    #[test]
    fn close_active_phase_task_returns_not_failed() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task(
                "in progress task",
                "",
                Vec::new(),
                Some("implementing"),
                "default".to_owned(),
            )
            .expect("add task");

        let result = close_task_flow(&project, &mut store, "done", &task.id, "reason");
        assert!(
            matches!(result, Err(super::CloseError::NotFailed { .. })),
            "expected NotFailed, got: {result:?}"
        );
    }

    #[test]
    fn close_done_task_returns_not_failed() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task(
                "done task",
                "",
                Vec::new(),
                Some("done"),
                "default".to_owned(),
            )
            .expect("add task");

        let result = close_task_flow(&project, &mut store, "done", &task.id, "reason");
        assert!(
            matches!(result, Err(super::CloseError::NotFailed { .. })),
            "expected NotFailed, got: {result:?}"
        );
    }

    #[test]
    fn close_blocked_task_returns_not_failed() {
        let (_dir, project, mut store) = make_project_store();
        let task = store
            .add_task(
                "blocked task",
                "",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");

        store
            .block_task(&task.id, "blocked reason")
            .expect("block task");

        let result = close_task_flow(&project, &mut store, "done", &task.id, "reason");
        assert!(
            matches!(result, Err(super::CloseError::NotFailed { .. })),
            "expected NotFailed, got: {result:?}"
        );
    }

    // ── error: unknown task ─────────────────────────────────────────────────────

    #[test]
    fn close_unknown_task_returns_not_found() {
        let (_dir, project, mut store) = make_project_store();

        let result = close_task_flow(&project, &mut store, "done", "task-999", "reason");
        assert!(
            matches!(result, Err(super::CloseError::TaskNotFound { .. })),
            "expected TaskNotFound, got: {result:?}"
        );
    }

    // ── validate_reason ─────────────────────────────────────────────────────────

    #[test]
    fn validate_reason_none_returns_missing_reason() {
        let result = super::validate_reason(None);
        assert!(matches!(result, Err(super::CloseError::MissingReason)));
    }

    #[test]
    fn validate_reason_empty_returns_empty_reason() {
        let result = super::validate_reason(Some("   "));
        assert!(matches!(result, Err(super::CloseError::EmptyReason)));
    }

    #[test]
    fn validate_reason_valid_returns_str() {
        let result = super::validate_reason(Some("manual verification"));
        assert_eq!(result.unwrap(), "manual verification");
    }
}
