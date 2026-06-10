use std::{io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{
        ConfigError, INITIAL_PHASE_NAME, PhaseDef, TERMINAL_PHASE_NAME, load_project_config,
        write_project_config,
    },
    project::Project,
};

#[derive(Debug, Error)]
pub enum PhaseGetError {
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
}

#[derive(Debug, Error)]
pub enum PhaseUpdateError {
    #[error("at least one of --set-duty, --set-model, --clear-duty, or --clear-model is required")]
    NoOperation,
    #[error("phase not found: {name}")]
    PhaseNotFound { name: String },
    #[error("phase already exists: {name}")]
    DuplicatePhase { name: String },
    #[error("reserved phase name: {name}")]
    ReservedPhase { name: String },
    #[error("cannot remove phase '{name}': workflows still reference it: {}", workflows.join(", "))]
    PhaseReferenced {
        name: String,
        workflows: Vec<String>,
    },
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
}

pub fn run_phase_list(project: &Project) -> Result<(), PhaseGetError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_get_config_error)?;

    for (name, def) in &config.phases {
        match (def.model.as_deref(), def.duty.as_deref()) {
            (Some(model), Some(duty)) if !duty.is_empty() => {
                println!("{name}:{model}  {duty}");
            }
            (Some(model), _) => println!("{name}:{model}"),
            (None, Some(duty)) if !duty.is_empty() => println!("{name}  {duty}"),
            _ => println!("{name}"),
        }
    }
    Ok(())
}

pub fn run_phase_add(
    project: &Project,
    name: &str,
    model: Option<&str>,
    duty: Option<&str>,
) -> Result<(), PhaseUpdateError> {
    reject_reserved(name)?;
    let config_path = project.state_dir.join("config.json");
    let mut config = load_project_config(&config_path, &project.global_config)
        .map_err(map_update_config_error)?;
    if config.phases.contains_key(name) {
        return Err(PhaseUpdateError::DuplicatePhase {
            name: name.to_owned(),
        });
    }
    config.phases.insert(
        name.to_owned(),
        PhaseDef {
            model: model.map(str::to_owned),
            duty: duty.map(str::to_owned),
        },
    );
    write_project_config(&config_path, &config).map_err(map_write_error)?;
    println!("phase added: {name}");
    Ok(())
}

pub fn run_phase_update(
    project: &Project,
    name: &str,
    set_model: Option<&str>,
    set_duty: Option<&str>,
    clear_model: bool,
    clear_duty: bool,
) -> Result<(), PhaseUpdateError> {
    if set_model.is_none() && set_duty.is_none() && !clear_model && !clear_duty {
        return Err(PhaseUpdateError::NoOperation);
    }
    let config_path = project.state_dir.join("config.json");
    let mut config = load_project_config(&config_path, &project.global_config)
        .map_err(map_update_config_error)?;
    let phase = config
        .phases
        .get_mut(name)
        .ok_or_else(|| PhaseUpdateError::PhaseNotFound {
            name: name.to_owned(),
        })?;
    if let Some(model) = set_model {
        phase.model = Some(model.to_owned());
    }
    if let Some(duty) = set_duty {
        phase.duty = Some(duty.to_owned());
    }
    if clear_model {
        phase.model = None;
    }
    if clear_duty {
        phase.duty = None;
    }
    write_project_config(&config_path, &config).map_err(map_write_error)?;
    println!("phase updated: {name}");
    Ok(())
}

pub fn run_phase_remove(project: &Project, name: &str) -> Result<(), PhaseUpdateError> {
    reject_reserved(name)?;
    let config_path = project.state_dir.join("config.json");
    let mut config = load_project_config(&config_path, &project.global_config)
        .map_err(map_update_config_error)?;
    if !config.phases.contains_key(name) {
        return Err(PhaseUpdateError::PhaseNotFound {
            name: name.to_owned(),
        });
    }
    let workflows: Vec<String> = config
        .workflows
        .iter()
        .filter(|(_, sequence)| sequence.iter().any(|phase| phase == name))
        .map(|(workflow, _)| workflow.clone())
        .collect();
    if !workflows.is_empty() {
        return Err(PhaseUpdateError::PhaseReferenced {
            name: name.to_owned(),
            workflows,
        });
    }
    config.phases.remove(name);
    write_project_config(&config_path, &config).map_err(map_write_error)?;
    println!("phase removed: {name}");
    Ok(())
}

fn reject_reserved(name: &str) -> Result<(), PhaseUpdateError> {
    if name == INITIAL_PHASE_NAME || name == TERMINAL_PHASE_NAME {
        Err(PhaseUpdateError::ReservedPhase {
            name: name.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn map_get_config_error(error: ConfigError) -> PhaseGetError {
    match error {
        ConfigError::NotFound { path } => PhaseGetError::NotFound { path },
        ConfigError::Read { path, source } => PhaseGetError::Read { path, source },
        ConfigError::Parse { path, source } => PhaseGetError::Load { path, source },
        ConfigError::Invalid { path, reason } => PhaseGetError::InvalidConfig { path, reason },
    }
}

fn map_update_config_error(error: ConfigError) -> PhaseUpdateError {
    match error {
        ConfigError::NotFound { path } => PhaseUpdateError::ConfigNotFound { path },
        ConfigError::Read { path, source } => PhaseUpdateError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => PhaseUpdateError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => PhaseUpdateError::InvalidConfig { path, reason },
    }
}

fn map_write_error(error: ConfigError) -> PhaseUpdateError {
    map_update_config_error(error)
}
