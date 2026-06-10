use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum UnlockError {
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

pub fn run_unlock(project: &Project, id: &str) -> Result<(), UnlockError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    config
        .terminal_phase()
        .ok_or_else(|| UnlockError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;

    store.unlock_task(id).map_err(|e| match e {
        StoreError::NotFound => UnlockError::TaskNotFound { id: id.to_owned() },
        other => UnlockError::StoreError(other),
    })?;

    print_unlock_output(&format!("{id} unlocked"));
    Ok(())
}

fn map_config_error(error: ConfigError) -> UnlockError {
    match error {
        ConfigError::NotFound { path } => UnlockError::ConfigNotFound { path },
        ConfigError::Read { path, source } => UnlockError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => UnlockError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => UnlockError::InvalidConfig { path, reason },
    }
}

fn print_unlock_output(message: &str) {
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
