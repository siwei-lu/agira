use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("at least one of --title, --description, or --depends-on is required")]
    NoFields,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("unknown dependency: {id}")]
    UnknownDependency { id: String },

    #[error("task {id} is {state} and cannot be updated")]
    CannotUpdate { id: String, state: String },

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

pub struct UpdateInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub acceptance_criteria: Option<String>,
}

pub fn run_update(project: &Project, id: &str, input: UpdateInput) -> Result<(), UpdateError> {
    if input.title.is_none()
        && input.description.is_none()
        && input.depends_on.is_none()
        && input.acceptance_criteria.is_none()
    {
        return Err(UpdateError::NoFields);
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;

    let depends_on_ref = input.depends_on.as_deref();
    // Some(Some("text")) means set; None means no change (we never clear via this path).
    let ac = input.acceptance_criteria.as_deref().map(Some);
    let result = store.update_task_with_ac(
        id,
        input.title.as_deref(),
        input.description.as_deref(),
        depends_on_ref,
        ac,
    );

    match result {
        Ok(()) => {
            print_update_output(&format!("{id} updated"));
            Ok(())
        }
        Err(StoreError::NotFound) => Err(UpdateError::TaskNotFound { id: id.to_owned() }),
        Err(StoreError::DependencyBlocked { blocking_id, .. }) => {
            Err(UpdateError::UnknownDependency { id: blocking_id })
        }
        Err(StoreError::CannotUpdateTerminal { id, state }) => {
            Err(UpdateError::CannotUpdate { id, state })
        }
        Err(error) => Err(error.into()),
    }
}

fn map_config_error(error: ConfigError) -> UpdateError {
    match error {
        ConfigError::NotFound { path } => UpdateError::ConfigNotFound { path },
        ConfigError::Read { path, source } => UpdateError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => UpdateError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => UpdateError::InvalidConfig { path, reason },
    }
}

fn print_update_output(message: &str) {
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
