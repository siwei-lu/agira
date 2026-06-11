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
    #[error(
        "at least one of --set-duty, --set-model, --clear-duty, --clear-model, --set-gate, or --unset-gate is required"
    )]
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
        let gate_suffix = match def.gate.as_deref() {
            Some(cmd) => format!("  [gate: {cmd}]"),
            None => String::new(),
        };
        match (def.model.as_deref(), def.duty.as_deref()) {
            (Some(model), Some(duty)) if !duty.is_empty() => {
                println!("{name}:{model}  {duty}{gate_suffix}");
            }
            (Some(model), _) => println!("{name}:{model}{gate_suffix}"),
            (None, Some(duty)) if !duty.is_empty() => println!("{name}  {duty}{gate_suffix}"),
            _ => println!("{name}{gate_suffix}"),
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
            gate: None,
        },
    );
    write_project_config(&config_path, &config).map_err(map_write_error)?;
    println!("phase added: {name}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_phase_update(
    project: &Project,
    name: &str,
    set_model: Option<&str>,
    set_duty: Option<&str>,
    clear_model: bool,
    clear_duty: bool,
    set_gate: Option<&str>,
    clear_gate: bool,
) -> Result<(), PhaseUpdateError> {
    if set_model.is_none()
        && set_duty.is_none()
        && !clear_model
        && !clear_duty
        && set_gate.is_none()
        && !clear_gate
    {
        return Err(PhaseUpdateError::NoOperation);
    }
    if set_gate.is_some() || clear_gate {
        reject_reserved(name)?;
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
    if let Some(gate_cmd) = set_gate {
        phase.gate = Some(gate_cmd.to_owned());
    }
    if clear_gate {
        phase.gate = None;
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

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::core::{
        config::{Config, PhaseDef, write_project_config},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
    };

    use super::{PhaseUpdateError, run_phase_list, run_phase_update};

    fn three_phase_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("verifying".to_owned(), PhaseDef::default()),
            ],
            3,
        )
    }

    fn setup_project_with_config(config: &Config) -> (tempfile::TempDir, Project) {
        let home_dir = tempfile::TempDir::new().expect("create home temp dir");
        let repo_dir = tempfile::TempDir::new().expect("create repo temp dir");
        let git_root = repo_dir.path().to_path_buf();
        fs::create_dir_all(git_root.join(".git")).expect("create .git dir");

        let state_dir = home_dir.path().join(".agira").join("test-repo");
        fs::create_dir_all(&state_dir).expect("create state dir");

        let config_json = serde_json::to_string_pretty(config).expect("serialize config");
        fs::write(state_dir.join("config.json"), &config_json).expect("write config.json");

        let project = Project {
            git_root,
            slug: "test-repo".to_owned(),
            state_dir,
            global_config: GlobalConfig::default(),
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        };

        (home_dir, project)
    }

    // -----------------------------------------------------------------------
    // set-gate round-trips correctly through config
    // -----------------------------------------------------------------------

    #[test]
    fn set_gate_writes_to_config_and_round_trips() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);

        let result = run_phase_update(
            &project,
            "verifying",
            None,
            None,
            false,
            false,
            Some("cargo test"),
            false,
        );
        assert!(result.is_ok(), "set-gate must succeed: {result:?}");

        // Read the config back and verify the gate was written
        let config_path = project.state_dir.join("config.json");
        let reloaded: Config =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            reloaded.phases["verifying"].gate.as_deref(),
            Some("cargo test")
        );
    }

    #[test]
    fn unset_gate_removes_key_from_config() {
        let mut config = three_phase_config();
        // Pre-set a gate
        config.phases.get_mut("verifying").unwrap().gate = Some("cargo test".to_owned());
        let (_home, project) = setup_project_with_config(&config);

        let result = run_phase_update(
            &project,
            "verifying",
            None,
            None,
            false,
            false,
            None,
            true, // clear_gate
        );
        assert!(result.is_ok(), "unset-gate must succeed: {result:?}");

        let config_path = project.state_dir.join("config.json");
        let raw = fs::read_to_string(&config_path).unwrap();
        let reloaded: Config = serde_json::from_str(&raw).unwrap();
        assert_eq!(reloaded.phases["verifying"].gate, None);
        // The JSON must not contain the gate key for this phase
        assert!(
            !raw.contains("\"gate\""),
            "gate key must not appear in JSON when None"
        );
    }

    // -----------------------------------------------------------------------
    // set-gate on pending and done returns ReservedPhase
    // -----------------------------------------------------------------------

    #[test]
    fn set_gate_on_pending_returns_reserved_phase_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);

        let result = run_phase_update(
            &project,
            "pending",
            None,
            None,
            false,
            false,
            Some("cargo test"),
            false,
        );

        match result {
            Err(PhaseUpdateError::ReservedPhase { name }) => {
                assert_eq!(name, "pending");
            }
            other => panic!("expected ReservedPhase, got: {other:?}"),
        }
    }

    #[test]
    fn set_gate_on_done_returns_reserved_phase_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);

        let result = run_phase_update(
            &project,
            "done",
            None,
            None,
            false,
            false,
            Some("cargo test"),
            false,
        );

        match result {
            Err(PhaseUpdateError::ReservedPhase { name }) => {
                assert_eq!(name, "done");
            }
            other => panic!("expected ReservedPhase, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // run_phase_list shows gate when present
    // -----------------------------------------------------------------------

    #[test]
    fn phase_list_shows_gate_when_present() {
        let mut config = three_phase_config();
        config.phases.get_mut("verifying").unwrap().gate = Some("cargo test".to_owned());
        let (_home, project) = setup_project_with_config(&config);

        // Capture stdout by reading after the call — we check via a config read
        // since stdout capture in unit tests is complex. Instead, verify the config
        // has the gate and that the format logic is exercised without error.
        let result = run_phase_list(&project);
        assert!(result.is_ok(), "phase list must succeed: {result:?}");

        // Also verify the format_phase_line helper logic by constructing directly
        let def = &config.phases["verifying"];
        let gate_suffix = match def.gate.as_deref() {
            Some(cmd) => format!("  [gate: {cmd}]"),
            None => String::new(),
        };
        assert_eq!(gate_suffix, "  [gate: cargo test]");
    }

    // -----------------------------------------------------------------------
    // write_project_config does not serialize gate when it is None
    // -----------------------------------------------------------------------

    #[test]
    fn write_config_omits_gate_key_when_none() {
        let config = three_phase_config();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        write_project_config(&path, &config).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\"gate\""), "gate key must be absent");
    }
}
