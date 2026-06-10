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
