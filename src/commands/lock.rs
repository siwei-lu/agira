use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum LockError {
    #[error("task {id} not found")]
    TaskNotFound { id: String },

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

pub fn run_lock(project: &Project, id: &str) -> Result<(), LockError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    config
        .terminal_phase()
        .ok_or_else(|| LockError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;

    store.lock_task(id).map_err(|e| match e {
        StoreError::NotFound => LockError::TaskNotFound { id: id.to_owned() },
        other => LockError::StoreError(other),
    })?;

    print_lock_output(&format!("{id} locked"));
    Ok(())
}

fn map_config_error(error: ConfigError) -> LockError {
    match error {
        ConfigError::NotFound { path } => LockError::ConfigNotFound { path },
        ConfigError::Read { path, source } => LockError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => LockError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => LockError::InvalidConfig { path, reason },
    }
}

fn print_lock_output(message: &str) {
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
