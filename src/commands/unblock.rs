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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
