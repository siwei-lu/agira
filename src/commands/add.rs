use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    hooks::{HookContext, TASK_ADDED_EVENT, dispatch_hooks, hooks_for_event},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum AddError {
    #[error("unknown dependency: {id}")]
    UnknownDependency { id: String },

    #[error("unknown phase: {phase}")]
    UnknownPhase { phase: String },

    #[error("unknown workflow '{name}'; available: {available}")]
    UnknownWorkflow { name: String, available: String },

    #[error("a task with this title already exists: {id} \"{title}\"")]
    DuplicateTitle { id: String, title: String },

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

#[allow(clippy::too_many_arguments)]
pub fn run_add(
    project: &Project,
    title: &str,
    description: Option<&str>,
    depends_on: &[String],
    phase: Option<&str>,
    phases: Option<&str>,
    duties: Option<&[String]>,
    workflow: Option<&str>,
) -> Result<(), AddError> {
    let _ = phases;
    let _ = duties;

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let mut store = TaskStore::new(&project.state_dir, &config)?;
    let title_lowercase = title.to_lowercase();
    let duplicate = store
        .all_tasks()
        .iter()
        .find(|task| {
            Some(task.state.as_str()) != config.terminal_phase()
                && task.title.to_lowercase() == title_lowercase
        })
        .map(|task| (task.id.clone(), task.title.clone()));

    if let Some((id, title)) = duplicate {
        return Err(AddError::DuplicateTitle { id, title });
    }

    let workflow_name = if let Some(wf_name) = workflow {
        if config.sequence(wf_name).is_empty() {
            let names: Vec<String> = config.workflows.keys().cloned().collect();
            return Err(AddError::UnknownWorkflow {
                name: wf_name.to_owned(),
                available: names.join(", "),
            });
        }
        wf_name.to_owned()
    } else {
        config.default_workflow.clone()
    };

    add_task_flow(
        project,
        &mut store,
        title,
        description.unwrap_or(""),
        depends_on.to_vec(),
        phase,
        workflow_name,
    )
}

fn map_config_error(error: ConfigError) -> AddError {
    match error {
        ConfigError::NotFound { path } => AddError::ConfigNotFound { path },
        ConfigError::Read { path, source } => AddError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => AddError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => AddError::InvalidConfig { path, reason },
    }
}

#[allow(clippy::too_many_arguments)]
fn add_task_flow(
    project: &Project,
    store: &mut TaskStore,
    title: &str,
    description: &str,
    depends_on: Vec<String>,
    phase: Option<&str>,
    workflow_name: String,
) -> Result<(), AddError> {
    let task = match store.add_task(title, description, depends_on, phase, workflow_name) {
        Ok(task) => task,
        Err(error) => return Err(map_store_error(error)),
    };

    dispatch_task_added_hooks(project, &task);
    print_add_output(&format!("added {}: {}", task.id, task.title));

    Ok(())
}

fn dispatch_task_added_hooks(project: &Project, task: &crate::core::tasks::Task) {
    let hooks = hooks_for_event(
        &project.global_hooks,
        &project.project_hooks,
        TASK_ADDED_EVENT,
    );
    dispatch_hooks(
        &hooks,
        TASK_ADDED_EVENT,
        &HookContext::new(
            task,
            &project.slug,
            &project.git_root,
            &project.state_dir,
            "",
            &task.state,
            "",
        ),
        project.global_config.hook_debug,
    );
}

fn map_store_error(error: StoreError) -> AddError {
    match error {
        StoreError::DependencyBlocked { blocking_id, .. } => {
            AddError::UnknownDependency { id: blocking_id }
        }
        StoreError::UnknownPhase { phase } => AddError::UnknownPhase { phase },
        other => AddError::StoreError(other),
    }
}

fn print_add_output(message: &str) {
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
