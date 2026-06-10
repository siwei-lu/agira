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
        .sequence(&config.default_workflow)
        .first()
        .cloned()
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
