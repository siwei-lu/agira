use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    hooks::{ALL_TASKS_DONE_EVENT, HookContext, dispatch_hooks, hooks_for_event, hooks_for_phase},
    project::Project,
    tasks::{StoreError, TaskStore, all_tasks_done},
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
    fail_task_flow(project, &mut store, &terminal_phase, id, reason)
}

fn fail_task_flow(
    project: &Project,
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
        let task = store.get_task(id).unwrap();
        let to_phase = task.state.clone();
        dispatch_task_hooks(project, task, &current_state, &to_phase, "");
        print_fail_output(&format!(
            "{id} retrying ({new_retry_count}/{max_retries}): {reason}"
        ));
    } else {
        let failure_reason = format!("failed (max retries): {reason}");
        if let Err(error) = store.fail_task(id, &failure_reason) {
            return Err(map_store_error(error, store, id));
        }
        let task = store.get_task(id).unwrap();
        let hook_ctx = dispatch_task_hooks(project, task, &current_state, "failed", "");
        if all_tasks_done(store.all_tasks()) {
            let all_done_hooks = hooks_for_event(
                &project.global_hooks,
                &project.project_hooks,
                ALL_TASKS_DONE_EVENT,
            );
            dispatch_hooks(
                &all_done_hooks,
                ALL_TASKS_DONE_EVENT,
                &hook_ctx,
                project.global_config.hook_debug,
            );
        }
        print_fail_output(&format!("{id} failed — max retries reached"));
    }

    Ok(())
}

fn dispatch_task_hooks(
    project: &Project,
    task: &crate::core::tasks::Task,
    from_phase: &str,
    to_phase: &str,
    artifact: &str,
) -> HookContext {
    let hooks = hooks_for_phase(&project.global_hooks, &project.project_hooks, to_phase);
    let hook_ctx = HookContext::new(
        task,
        &project.slug,
        &project.git_root,
        &project.state_dir,
        from_phase,
        to_phase,
        artifact,
    );
    dispatch_hooks(
        &hooks,
        to_phase,
        &hook_ctx,
        project.global_config.hook_debug,
    );
    hook_ctx
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
        ConfigError::Invalid { path, reason } => FailError::InvalidConfig { path, reason },
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
