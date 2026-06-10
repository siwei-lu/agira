use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::core::{
    config::{
        Config, ConfigError, INITIAL_PHASE_NAME, TERMINAL_PHASE_NAME, load_project_config,
        normalize_sequence, write_project_config,
    },
    project::Project,
    tasks::TaskStore,
};

#[derive(Debug, Error)]
pub enum WorkflowListError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to load config {path}: {source}")]
    Load {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid config {path}: {reason}")]
    InvalidConfig { path: PathBuf, reason: String },
    #[error("failed to serialize workflow list")]
    JsonOutput(#[source] serde_json::Error),
}

pub fn run_workflow_list(project: &Project, json: bool) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let config = load_project_config(&config_path, &project.global_config).map_err(map_error)?;

    if json {
        let sorted: BTreeMap<&str, &[String]> = config
            .workflows
            .iter()
            .map(|(name, sequence)| (name.as_str(), sequence.as_slice()))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&sorted).map_err(WorkflowListError::JsonOutput)?
        );
    } else {
        print!("{}", format_workflow_list(&config));
    }
    Ok(())
}

pub fn run_workflow_add(
    project: &Project,
    name: &str,
    phases: Vec<String>,
) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let mut config =
        load_project_config(&config_path, &project.global_config).map_err(map_error)?;
    if config.workflows.contains_key(name) {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: format!("workflow '{name}' already exists"),
        });
    }
    let sequence = normalize_sequence(phases);
    validate_refs(&config, &sequence, &config_path)?;
    config.workflows.insert(name.to_owned(), sequence);
    write_project_config(&config_path, &config).map_err(map_error)?;
    println!("workflow added: {name}");
    Ok(())
}

pub fn run_workflow_update(
    project: &Project,
    name: &str,
    add: Option<&str>,
    after: Option<&str>,
    remove: Option<&str>,
) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let mut config =
        load_project_config(&config_path, &project.global_config).map_err(map_error)?;
    if add.is_none() && remove.is_none() {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: "one of --add or --remove is required".to_owned(),
        });
    }
    let mut sequence =
        config
            .workflows
            .get(name)
            .cloned()
            .ok_or_else(|| WorkflowListError::InvalidConfig {
                path: config_path.clone(),
                reason: format!("unknown workflow '{name}'"),
            })?;

    if let Some(phase) = remove {
        if phase == INITIAL_PHASE_NAME || phase == TERMINAL_PHASE_NAME {
            return Err(WorkflowListError::InvalidConfig {
                path: config_path,
                reason: format!("cannot remove reserved phase '{phase}'"),
            });
        }
        let store = TaskStore::new(&project.state_dir, &config).map_err(|error| {
            WorkflowListError::InvalidConfig {
                path: config_path.clone(),
                reason: error.to_string(),
            }
        })?;
        if let Some(task) = store.all_tasks().iter().find(|task| {
            task.workflow == name && task.state == phase && task.state != TERMINAL_PHASE_NAME
        }) {
            return Err(WorkflowListError::InvalidConfig {
                path: config_path,
                reason: format!(
                    "cannot remove phase '{phase}' from workflow '{name}': task {} is in that phase",
                    task.id
                ),
            });
        }
        sequence.retain(|candidate| candidate != phase);
    }

    if let Some(phase) = add {
        if !config.phases.contains_key(phase) {
            return Err(WorkflowListError::InvalidConfig {
                path: config_path,
                reason: format!("unknown phase '{phase}'"),
            });
        }
        if sequence.iter().any(|candidate| candidate == phase) {
            return Err(WorkflowListError::InvalidConfig {
                path: config_path,
                reason: format!("workflow '{name}' already references phase '{phase}'"),
            });
        }
        let insert_after = after.unwrap_or(INITIAL_PHASE_NAME);
        let position = sequence
            .iter()
            .position(|candidate| candidate == insert_after)
            .ok_or_else(|| WorkflowListError::InvalidConfig {
                path: config_path.clone(),
                reason: format!("phase '{insert_after}' not found in workflow '{name}'"),
            })?;
        sequence.insert(position + 1, phase.to_owned());
    }

    config
        .workflows
        .insert(name.to_owned(), normalize_sequence(sequence));
    write_project_config(&config_path, &config).map_err(map_error)?;
    println!("workflow updated: {name}");
    Ok(())
}

pub fn run_workflow_remove(project: &Project, name: &str) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let mut config =
        load_project_config(&config_path, &project.global_config).map_err(map_error)?;
    if name == config.default_workflow {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: "cannot remove the default workflow; set-default first".to_owned(),
        });
    }
    if !config.workflows.contains_key(name) {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: format!("unknown workflow '{name}'"),
        });
    }
    let store = TaskStore::new(&project.state_dir, &config).map_err(|error| {
        WorkflowListError::InvalidConfig {
            path: config_path.clone(),
            reason: error.to_string(),
        }
    })?;
    if let Some(task) = store.all_tasks().iter().find(|task| {
        task.workflow == name && task.state != "failed" && task.state != TERMINAL_PHASE_NAME
    }) {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: format!(
                "cannot remove workflow '{name}': task {} is not terminal",
                task.id
            ),
        });
    }
    config.workflows.remove(name);
    write_project_config(&config_path, &config).map_err(map_error)?;
    println!("workflow removed: {name}");
    Ok(())
}

pub fn run_workflow_set_default(project: &Project, name: &str) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let mut config =
        load_project_config(&config_path, &project.global_config).map_err(map_error)?;
    if !config.workflows.contains_key(name) {
        return Err(WorkflowListError::InvalidConfig {
            path: config_path,
            reason: format!("unknown workflow '{name}'"),
        });
    }
    config.default_workflow = name.to_owned();
    write_project_config(&config_path, &config).map_err(map_error)?;
    println!("default workflow: {name}");
    Ok(())
}

fn format_workflow_list(config: &Config) -> String {
    if config.workflows.is_empty() {
        return "no workflows configured\n".to_owned();
    }

    let mut output = String::new();
    for (name, sequence) in &config.workflows {
        let is_default = name == &config.default_workflow;
        let phases_str = format_phases(config, sequence);

        if is_default {
            output.push_str(&format!("{name} (default)  {phases_str}\n"));
        } else {
            output.push_str(&format!("{name}  {phases_str}\n"));
        }
    }
    output
}

fn format_phases(config: &Config, sequence: &[String]) -> String {
    sequence
        .iter()
        .map(
            |name| match config.phase_def(name).and_then(|p| p.model.as_deref()) {
                Some(model) => format!("{name}:{model}"),
                None => name.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn validate_refs(
    config: &Config,
    sequence: &[String],
    path: &Path,
) -> Result<(), WorkflowListError> {
    for phase in sequence {
        if !config.phases.contains_key(phase) {
            return Err(WorkflowListError::InvalidConfig {
                path: path.to_path_buf(),
                reason: format!("unknown phase '{phase}'"),
            });
        }
    }
    Ok(())
}

fn map_error(error: ConfigError) -> WorkflowListError {
    match error {
        ConfigError::NotFound { path } => WorkflowListError::NotFound { path },
        ConfigError::Read { path, source } => WorkflowListError::Read { path, source },
        ConfigError::Parse { path, source } => WorkflowListError::Load { path, source },
        ConfigError::Invalid { path, reason } => WorkflowListError::InvalidConfig { path, reason },
    }
}
