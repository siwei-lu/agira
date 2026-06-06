use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::core::{
    config::{
        ConfigError, INITIAL_PHASE_NAME, PhaseConfig, TERMINAL_PHASE_NAME, load_project_config,
        validate_terminal_phase,
    },
    project::Project,
    tasks::{StoreError, TaskStore},
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
        "at least one of --add, --remove, --set-model, --clear-model, --set-duty, or --clear-duty is required"
    )]
    NoOperation,

    #[error("--after and --before cannot be used together")]
    ConflictingPositionFlags,

    #[error(
        "invalid --add format: use phase or phase:model (e.g. review or review:codex); phase and model labels must be non-empty and contain no whitespace"
    )]
    InvalidAddFormat,

    #[error(
        "invalid model label: {model}; model labels must be non-empty and contain no whitespace"
    )]
    InvalidModelLabel { model: String },

    #[error("phase not found: {name}")]
    PhaseNotFound { name: String },

    #[error("phase already exists: {name}")]
    DuplicatePhase { name: String },

    #[error("cannot remove mandatory phase '{name}'")]
    MandatoryPhase { name: String },

    #[error(
        "cannot set model on mandatory phase '{name}': pending and done are transition phases with no model"
    )]
    MandatoryPhaseNoModel { name: String },

    #[error(
        "cannot set duty on mandatory phase '{name}': pending and done are transition phases with no duty"
    )]
    MandatoryPhaseDuty { name: String },

    #[error("cannot insert before mandatory initial phase '{name}'")]
    CannotInsertBeforeInitial { name: String },

    #[error("cannot insert after mandatory terminal phase '{name}'")]
    CannotInsertAfterTerminal { name: String },

    #[error("cannot remove phase '{name}': tasks currently in that phase: {}", task_ids.join(", "))]
    PhaseBusy { name: String, task_ids: Vec<String> },

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

    #[error("failed to write {path}")]
    ConfigWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    StoreError(#[from] StoreError),
}

pub fn run_phase_get(project: &Project) -> Result<(), PhaseGetError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_get_config_error)?;

    let display: Vec<String> = config
        .phases
        .iter()
        .map(|p| match p.model.as_deref() {
            Some(m) => format!("{}:{}", p.name, m),
            None => p.name.clone(),
        })
        .collect();
    println!("phases: {}", display.join(" \u{2192} "));

    Ok(())
}

#[cfg(test)]
fn run_phase_update(
    project: &Project,
    add: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    remove: Option<&str>,
    set_model: Option<&[String]>,
) -> Result<(), PhaseUpdateError> {
    run_phase_update_inner(
        project, add, after, before, remove, set_model, None, None, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_phase_update_with_clear_model(
    project: &Project,
    add: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    remove: Option<&str>,
    set_model: Option<&[String]>,
    clear_model: Option<&str>,
    set_duty: Option<&[String]>,
    clear_duty: Option<&str>,
) -> Result<(), PhaseUpdateError> {
    run_phase_update_inner(
        project,
        add,
        after,
        before,
        remove,
        set_model,
        clear_model,
        set_duty,
        clear_duty,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_phase_update_inner(
    project: &Project,
    add: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    remove: Option<&str>,
    set_model: Option<&[String]>,
    clear_model: Option<&str>,
    set_duty: Option<&[String]>,
    clear_duty: Option<&str>,
) -> Result<(), PhaseUpdateError> {
    if add.is_none()
        && remove.is_none()
        && set_model.is_none()
        && clear_model.is_none()
        && set_duty.is_none()
        && clear_duty.is_none()
    {
        return Err(PhaseUpdateError::NoOperation);
    }

    if after.is_some() && before.is_some() {
        return Err(PhaseUpdateError::ConflictingPositionFlags);
    }

    let new_phase = if let Some(add_arg) = add {
        let (name, model) = parse_phase_model_arg(add_arg)?;
        Some(PhaseConfig {
            name: name.to_owned(),
            model: model.map(str::to_owned),
            duty: None,
        })
    } else {
        None
    };

    let config_path = project.state_dir.join("config.json");
    let config = load_project_config(&config_path, &project.global_config)
        .map_err(map_update_config_error)?;

    if let Some(after) = after {
        if !config.phases.iter().any(|p| p.name == after) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: after.to_owned(),
            });
        }
    }

    if let Some(before) = before {
        if !config.phases.iter().any(|p| p.name == before) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: before.to_owned(),
            });
        }
    }

    if let Some(ref p) = new_phase {
        if config.phases.iter().any(|existing| existing.name == p.name) {
            return Err(PhaseUpdateError::DuplicatePhase {
                name: p.name.clone(),
            });
        }
    }

    if let Some(remove) = remove {
        if is_mandatory_phase(remove) {
            return Err(PhaseUpdateError::MandatoryPhase {
                name: remove.to_owned(),
            });
        }

        let store = TaskStore::new(&project.state_dir, &config)?;
        let blocking_ids: Vec<String> = store
            .all_tasks()
            .iter()
            .filter(|task| task.state == remove)
            .map(|task| task.id.clone())
            .collect();
        if !blocking_ids.is_empty() {
            return Err(PhaseUpdateError::PhaseBusy {
                name: remove.to_owned(),
                task_ids: blocking_ids,
            });
        }
    }

    if new_phase.is_some() {
        if before == Some(INITIAL_PHASE_NAME) {
            return Err(PhaseUpdateError::CannotInsertBeforeInitial {
                name: INITIAL_PHASE_NAME.to_owned(),
            });
        }

        if after == Some(TERMINAL_PHASE_NAME) || (after.is_none() && before.is_none()) {
            return Err(PhaseUpdateError::CannotInsertAfterTerminal {
                name: TERMINAL_PHASE_NAME.to_owned(),
            });
        }
    }

    if let Some(sm) = set_model {
        let phase_name = &sm[0];
        let model_name = &sm[1];
        if !is_valid_phase_label(model_name) {
            return Err(PhaseUpdateError::InvalidModelLabel {
                model: model_name.clone(),
            });
        }
        if is_mandatory_phase(phase_name) {
            return Err(PhaseUpdateError::MandatoryPhaseNoModel {
                name: phase_name.clone(),
            });
        }
        if !config.phases.iter().any(|p| &p.name == phase_name) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: phase_name.clone(),
            });
        }
    }

    if let Some(phase_name) = clear_model {
        if is_mandatory_phase(phase_name) {
            return Err(PhaseUpdateError::MandatoryPhaseNoModel {
                name: phase_name.to_owned(),
            });
        }
        if !config.phases.iter().any(|p| p.name == phase_name) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: phase_name.to_owned(),
            });
        }
    }

    if let Some(sd) = set_duty {
        let phase_name = &sd[0];
        if is_mandatory_phase(phase_name) {
            return Err(PhaseUpdateError::MandatoryPhaseDuty {
                name: phase_name.clone(),
            });
        }
        if !config.phases.iter().any(|p| &p.name == phase_name) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: phase_name.clone(),
            });
        }
    }

    if let Some(phase_name) = clear_duty {
        if is_mandatory_phase(phase_name) {
            return Err(PhaseUpdateError::MandatoryPhaseDuty {
                name: phase_name.to_owned(),
            });
        }
        if !config.phases.iter().any(|p| p.name == phase_name) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: phase_name.to_owned(),
            });
        }
    }

    let mut new_phases = config.phases.clone();

    if let Some(remove) = remove {
        new_phases.retain(|p| p.name != remove);
    }

    if let Some(phase) = new_phase {
        if let Some(after) = after {
            let pos = new_phases.iter().position(|p| p.name == after).unwrap();
            new_phases.insert(pos + 1, phase);
        } else if let Some(before) = before {
            let pos = new_phases.iter().position(|p| p.name == before).unwrap();
            new_phases.insert(pos, phase);
        } else {
            new_phases.push(phase);
        }
    }

    if let Some(sm) = set_model {
        let (phase_name, model_name) = (&sm[0], &sm[1]);
        for p in &mut new_phases {
            if &p.name == phase_name {
                p.model = Some(model_name.clone());
                break;
            }
        }
    }

    if let Some(phase_name) = clear_model {
        for p in &mut new_phases {
            if p.name == phase_name {
                p.model = None;
                break;
            }
        }
    }

    if let Some(sd) = set_duty {
        let (phase_name, duty) = (&sd[0], &sd[1]);
        for p in &mut new_phases {
            if &p.name == phase_name {
                p.duty = Some(duty.clone());
                break;
            }
        }
    }

    if let Some(phase_name) = clear_duty {
        for p in &mut new_phases {
            if p.name == phase_name {
                p.duty = None;
                break;
            }
        }
    }

    validate_updated_phases(&config_path, &new_phases)?;
    patch_phases(&config_path, &new_phases)?;

    let display: Vec<String> = new_phases
        .iter()
        .map(|p| match p.model.as_deref() {
            Some(m) => format!("{}:{}", p.name, m),
            None => p.name.clone(),
        })
        .collect();
    println!("phases: {}", display.join(" \u{2192} "));

    Ok(())
}

fn parse_phase_model_arg(input: &str) -> Result<(&str, Option<&str>), PhaseUpdateError> {
    if let Some((name, model)) = input.split_once(':') {
        // Colon present: both parts must be non-empty and whitespace-free.
        if is_valid_phase_label(name) && is_valid_phase_label(model) {
            Ok((name, Some(model)))
        } else {
            Err(PhaseUpdateError::InvalidAddFormat)
        }
    } else {
        // No colon: bare phase name with no model.
        if is_valid_phase_label(input) {
            Ok((input, None))
        } else {
            Err(PhaseUpdateError::InvalidAddFormat)
        }
    }
}

fn is_valid_phase_label(label: &str) -> bool {
    !label.is_empty() && !label.chars().any(char::is_whitespace)
}

fn is_mandatory_phase(name: &str) -> bool {
    name == INITIAL_PHASE_NAME || name == TERMINAL_PHASE_NAME
}

fn validate_updated_phases(
    config_path: &Path,
    phases: &[PhaseConfig],
) -> Result<(), PhaseUpdateError> {
    if phases.is_empty() {
        return Err(PhaseUpdateError::InvalidConfig {
            path: config_path.to_path_buf(),
            reason: "phases must not be empty".to_owned(),
        });
    }

    validate_terminal_phase(phases).map_err(|reason| PhaseUpdateError::InvalidConfig {
        path: config_path.to_path_buf(),
        reason,
    })
}

fn patch_phases(config_path: &Path, new_phases: &[PhaseConfig]) -> Result<(), PhaseUpdateError> {
    let contents =
        fs::read_to_string(config_path).map_err(|source| PhaseUpdateError::ConfigRead {
            path: config_path.to_path_buf(),
            source,
        })?;

    let mut value: serde_json::Value =
        serde_json::from_str(&contents).map_err(|source| PhaseUpdateError::ConfigLoad {
            path: config_path.to_path_buf(),
            source,
        })?;

    value["phases"] = serde_json::json!(new_phases);
    // Remove old keys if present (migration)
    if let Some(obj) = value.as_object_mut() {
        obj.remove("state_machine");
        obj.remove("models");
    }

    let bytes =
        serde_json::to_vec_pretty(&value).map_err(|source| PhaseUpdateError::ConfigLoad {
            path: config_path.to_path_buf(),
            source,
        })?;

    let tmp_path = config_path.with_extension("json.tmp");
    fs::write(&tmp_path, &bytes).map_err(|source| PhaseUpdateError::ConfigWrite {
        path: config_path.to_path_buf(),
        source,
    })?;
    fs::rename(&tmp_path, config_path).map_err(|source| PhaseUpdateError::ConfigWrite {
        path: config_path.to_path_buf(),
        source,
    })?;

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            max_retries: 3,
        }
    }

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        }
    }

    fn write_config(project: &Project, config: &Config) {
        let contents = serde_json::to_vec_pretty(config).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    fn setup() -> (TempDir, Project, Config) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        write_config(&project, &config);
        (temp_dir, project, config)
    }

    fn loaded_phases(project: &Project) -> Vec<PhaseConfig> {
        let config_path = project.state_dir.join("config.json");
        let config = load_project_config(&config_path, &GlobalConfig::default()).unwrap();
        config.phases
    }

    #[test]
    fn get_returns_ok_with_valid_config() {
        let (_temp_dir, project, _config) = setup();
        run_phase_get(&project).unwrap();
    }

    #[test]
    fn get_errors_when_config_missing() {
        let temp_dir = TempDir::new().unwrap();
        let project = Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        };
        let err = run_phase_get(&project).unwrap_err();
        assert!(matches!(err, PhaseGetError::NotFound { .. }));
    }

    #[test]
    fn insert_after_existing_phase() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(
            &project,
            Some("review:sonnet"),
            Some("enriching"),
            None,
            None,
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases[0].name, "pending");
        assert_eq!(phases[1].name, "enriching");
        assert_eq!(phases[2].name, "review");
        assert_eq!(phases[2].model, Some("sonnet".to_owned()));
        assert_eq!(phases[3].name, "in_progress");
        assert_eq!(phases[4].name, "done");
    }

    #[test]
    fn insert_before_existing_phase() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(
            &project,
            Some("review:sonnet"),
            None,
            Some("done"),
            None,
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases[3].name, "review");
        assert_eq!(phases[3].model, Some("sonnet".to_owned()));
        assert_eq!(phases[4].name, "done");
    }

    #[test]
    fn insert_without_position_cannot_move_done_from_end() {
        let (_temp_dir, project, _config) = setup();
        let error =
            run_phase_update(&project, Some("deployed:haiku"), None, None, None, None).unwrap_err();
        match error {
            PhaseUpdateError::CannotInsertAfterTerminal { name } => {
                assert_eq!(name, "done");
            }
            other => panic!("expected CannotInsertAfterTerminal, got: {other}"),
        }

        let phases = loaded_phases(&project);
        assert_eq!(phases.last().unwrap().name, "done");
        assert!(!phases.iter().any(|p| p.name == "deployed"));
    }

    #[test]
    fn insert_after_done_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(
            &project,
            Some("deployed:haiku"),
            Some("done"),
            None,
            None,
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PhaseUpdateError::CannotInsertAfterTerminal { name } if name == "done"
        ));
    }

    #[test]
    fn insert_before_pending_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(
            &project,
            Some("triage:haiku"),
            None,
            Some("pending"),
            None,
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PhaseUpdateError::CannotInsertBeforeInitial { name } if name == "pending"
        ));
    }

    #[test]
    fn invalid_add_format_returns_error() {
        let (_temp_dir, project, _config) = setup();
        // Bare phase name ("review") is now valid; only truly malformed inputs are rejected.
        for bad_input in [":sonnet", "review:", "review sonnet", "review:son net"] {
            let error =
                run_phase_update(&project, Some(bad_input), None, None, None, None).unwrap_err();
            assert!(
                matches!(error, PhaseUpdateError::InvalidAddFormat),
                "expected InvalidAddFormat for input '{bad_input}'"
            );
        }
    }

    #[test]
    fn add_model_less_phase_before_done() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(&project, Some("triage"), None, Some("done"), None, None).unwrap();
        let phases = loaded_phases(&project);
        let triage = phases.iter().find(|p| p.name == "triage").unwrap();
        assert_eq!(triage.model, None);
    }

    #[test]
    fn add_model_less_phase_after_existing_phase() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(
            &project,
            Some("triage"),
            Some("enriching"),
            None,
            None,
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        let triage = phases.iter().find(|p| p.name == "triage").unwrap();
        assert_eq!(triage.model, None);
        // Ensure ordering: pending -> enriching -> triage -> in_progress -> done
        let names: Vec<&str> = phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["pending", "enriching", "triage", "in_progress", "done"]
        );
    }

    #[test]
    fn add_accepts_freeform_model_labels() {
        for model in ["codex", "dispatch-codex", "my_agent"] {
            let (_temp_dir, project, _config) = setup();
            let add_arg = format!("review:{model}");
            run_phase_update(&project, Some(&add_arg), None, Some("done"), None, None).unwrap();
            let phases = loaded_phases(&project);
            let review = phases.iter().find(|p| p.name == "review").unwrap();
            assert_eq!(review.model, Some(model.to_owned()));
        }
    }

    #[test]
    fn remove_existing_phase() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(&project, None, None, None, Some("in_progress"), None).unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].name, "pending");
        assert_eq!(phases[1].name, "enriching");
        assert_eq!(phases[2].name, "done");
    }

    #[test]
    fn remove_done_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(&project, None, None, None, Some("done"), None).unwrap_err();
        assert!(matches!(
            error,
            PhaseUpdateError::MandatoryPhase { name } if name == "done"
        ));

        let phases = loaded_phases(&project);
        assert_eq!(phases.last().unwrap().name, "done");
    }

    #[test]
    fn remove_pending_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error =
            run_phase_update(&project, None, None, None, Some("pending"), None).unwrap_err();
        assert!(matches!(
            error,
            PhaseUpdateError::MandatoryPhase { name } if name == "pending"
        ));

        let phases = loaded_phases(&project);
        assert_eq!(phases.first().unwrap().name, "pending");
    }

    #[test]
    fn duplicate_name_rejection() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(&project, Some("enriching:sonnet"), None, None, None, None)
            .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::DuplicatePhase { name } if name == "enriching"));
    }

    #[test]
    fn blocked_removal_lists_task_ids() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Blocked task", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        let error =
            run_phase_update(&project, None, None, None, Some("enriching"), None).unwrap_err();
        match error {
            PhaseUpdateError::PhaseBusy { name, task_ids } => {
                assert_eq!(name, "enriching");
                assert_eq!(task_ids, vec!["task-001"]);
            }
            other => panic!("expected PhaseBusy, got: {other}"),
        }
    }

    #[test]
    fn no_operation_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(&project, None, None, None, None, None).unwrap_err();
        assert!(matches!(error, PhaseUpdateError::NoOperation));
    }

    #[test]
    fn after_and_before_together_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(
            &project,
            Some("new:sonnet"),
            Some("done"),
            Some("in_progress"),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::ConflictingPositionFlags));
    }

    #[test]
    fn after_nonexistent_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(
            &project,
            Some("new:sonnet"),
            Some("nonexistent"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn before_nonexistent_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update(
            &project,
            Some("new:sonnet"),
            None,
            Some("nonexistent"),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn add_and_remove_together() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(
            &project,
            Some("review:sonnet"),
            None,
            Some("done"),
            Some("in_progress"),
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.len(), 4);
        assert!(!phases.iter().any(|p| p.name == "in_progress"));
        assert_eq!(phases[2].name, "review");
        assert_eq!(phases[3].name, "done");
    }

    #[test]
    fn set_model_changes_existing_phase_model() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["enriching".to_owned(), "haiku".to_owned()];
        run_phase_update(&project, None, None, None, None, Some(&args)).unwrap();
        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(enriching.model, Some("haiku".to_owned()));
    }

    #[test]
    fn clear_model_unsets_existing_phase_model() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update_with_clear_model(
            &project,
            None,
            None,
            None,
            None,
            None,
            Some("enriching"),
            None,
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(enriching.model, None);
    }

    #[test]
    fn set_duty_changes_existing_phase_duty() {
        let (_temp_dir, project, _config) = setup();
        let args = vec![
            "enriching".to_owned(),
            "Clarify requirements before implementation".to_owned(),
        ];
        run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&args),
            None,
        )
        .unwrap();

        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(
            enriching.duty,
            Some("Clarify requirements before implementation".to_owned())
        );
    }

    #[test]
    fn clear_duty_unsets_existing_phase_duty() {
        let (_temp_dir, project, mut config) = setup();
        config.phases[1].duty = Some("Clarify requirements".to_owned());
        write_config(&project, &config);

        run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("enriching"),
        )
        .unwrap();

        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(enriching.duty, None);
    }

    #[test]
    fn set_and_clear_duty_on_pending_return_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["pending".to_owned(), "Route new work".to_owned()];
        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&args),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(error, PhaseUpdateError::MandatoryPhaseDuty { name } if name == "pending")
        );

        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("pending"),
        )
        .unwrap_err();
        assert!(
            matches!(error, PhaseUpdateError::MandatoryPhaseDuty { name } if name == "pending")
        );
    }

    #[test]
    fn set_and_clear_duty_on_done_return_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["done".to_owned(), "Archive finished work".to_owned()];
        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&args),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PhaseUpdateError::MandatoryPhaseDuty { name } if name == "done"
        ));

        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("done"),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PhaseUpdateError::MandatoryPhaseDuty { name } if name == "done"
        ));
    }

    #[test]
    fn set_and_clear_duty_unknown_phase_return_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["nonexistent".to_owned(), "Review implementation".to_owned()];
        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&args),
            None,
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));

        let error = run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("nonexistent"),
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn set_duty_and_set_model_together() {
        let (_temp_dir, project, _config) = setup();
        let model_args = vec!["in_progress".to_owned(), "opus".to_owned()];
        let duty_args = vec![
            "in_progress".to_owned(),
            "Implement the accepted plan".to_owned(),
        ];
        run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            Some(&model_args),
            None,
            Some(&duty_args),
            None,
        )
        .unwrap();

        let phases = loaded_phases(&project);
        let in_progress = phases.iter().find(|p| p.name == "in_progress").unwrap();
        assert_eq!(in_progress.model, Some("opus".to_owned()));
        assert_eq!(
            in_progress.duty,
            Some("Implement the accepted plan".to_owned())
        );
    }

    #[test]
    fn clear_model_unknown_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update_with_clear_model(
            &project,
            None,
            None,
            None,
            None,
            None,
            Some("nonexistent"),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn clear_model_on_mandatory_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error = run_phase_update_with_clear_model(
            &project,
            None,
            None,
            None,
            None,
            None,
            Some("pending"),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(error, PhaseUpdateError::MandatoryPhaseNoModel { name } if name == "pending")
        );
    }

    #[test]
    fn set_model_on_mandatory_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let pending_args = vec!["pending".to_owned(), "opus".to_owned()];
        let error =
            run_phase_update(&project, None, None, None, None, Some(&pending_args)).unwrap_err();
        assert!(
            matches!(error, PhaseUpdateError::MandatoryPhaseNoModel { name } if name == "pending")
        );

        let done_args = vec!["done".to_owned(), "sonnet".to_owned()];
        let error =
            run_phase_update(&project, None, None, None, None, Some(&done_args)).unwrap_err();
        assert!(
            matches!(error, PhaseUpdateError::MandatoryPhaseNoModel { name } if name == "done")
        );
    }

    #[test]
    fn set_model_unknown_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["nonexistent".to_owned(), "haiku".to_owned()];
        let error = run_phase_update(&project, None, None, None, None, Some(&args)).unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn set_model_accepts_freeform_model_labels() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["enriching".to_owned(), "my_agent".to_owned()];
        run_phase_update(&project, None, None, None, None, Some(&args)).unwrap();
        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(enriching.model, Some("my_agent".to_owned()));
    }

    #[test]
    fn set_model_rejects_empty_or_whitespace_model_labels() {
        for bad_model in ["", "my agent"] {
            let (_temp_dir, project, _config) = setup();
            let args = vec!["enriching".to_owned(), bad_model.to_owned()];
            let error =
                run_phase_update(&project, None, None, None, None, Some(&args)).unwrap_err();
            assert!(
                matches!(error, PhaseUpdateError::InvalidModelLabel { model } if model == bad_model),
                "expected InvalidModelLabel for model '{bad_model}'"
            );
        }
    }

    #[test]
    fn write_only_patches_phases_field() {
        let (_temp_dir, project, _config) = setup();
        let config_path = project.state_dir.join("config.json");
        let original = fs::read_to_string(&config_path).unwrap();
        let original_value: serde_json::Value = serde_json::from_str(&original).unwrap();

        let args = vec![
            "enriching".to_owned(),
            "Clarify requirements before implementation".to_owned(),
        ];
        run_phase_update_inner(
            &project,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&args),
            None,
        )
        .unwrap();

        let updated = fs::read_to_string(&config_path).unwrap();
        let updated_value: serde_json::Value = serde_json::from_str(&updated).unwrap();

        for (key, orig_val) in original_value.as_object().unwrap() {
            if key != "phases" {
                assert_eq!(
                    updated_value.get(key),
                    Some(orig_val),
                    "field '{key}' changed unexpectedly"
                );
            }
        }
    }

    #[test]
    fn remove_phase_with_no_active_tasks_succeeds() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(&project, None, None, None, Some("in_progress"), None).unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.len(), 3);
    }

    #[test]
    fn multiple_tasks_blocked_removal_lists_all() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task A", "", vec![], None, None).unwrap();
        store.add_task("Task B", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-002").unwrap();
        let error =
            run_phase_update(&project, None, None, None, Some("enriching"), None).unwrap_err();
        match error {
            PhaseUpdateError::PhaseBusy { task_ids, .. } => {
                assert!(task_ids.contains(&"task-001".to_owned()));
                assert!(task_ids.contains(&"task-002".to_owned()));
            }
            other => panic!("expected PhaseBusy, got: {other}"),
        }
    }
}
