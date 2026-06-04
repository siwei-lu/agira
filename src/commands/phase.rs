use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, PhaseConfig, load_project_config},
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
}

const VALID_MODELS: &[&str] = &["opus", "sonnet", "haiku"];

#[derive(Debug, Error)]
pub enum PhaseUpdateError {
    #[error("at least one of --add, --remove, or --set-model is required")]
    NoOperation,

    #[error("--after and --before cannot be used together")]
    ConflictingPositionFlags,

    #[error(
        "invalid --add format: use phase:model (e.g. review:opus); valid models: opus, sonnet, haiku"
    )]
    InvalidAddFormat,

    #[error("unknown model: {model}; valid models: opus, sonnet, haiku")]
    UnknownModel { model: String },

    #[error("phase not found: {name}")]
    PhaseNotFound { name: String },

    #[error("phase already exists: {name}")]
    DuplicatePhase { name: String },

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
        .map(|p| format!("{}:{}", p.name, p.model))
        .collect();
    println!("phases: {}", display.join(" \u{2192} "));

    Ok(())
}

pub fn run_phase_update(
    project: &Project,
    add: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    remove: Option<&str>,
    set_model: Option<&[String]>,
) -> Result<(), PhaseUpdateError> {
    if add.is_none() && remove.is_none() && set_model.is_none() {
        return Err(PhaseUpdateError::NoOperation);
    }

    if after.is_some() && before.is_some() {
        return Err(PhaseUpdateError::ConflictingPositionFlags);
    }

    let new_phase = if let Some(add_arg) = add {
        let (name, model) = parse_phase_model_arg(add_arg)?;
        validate_model(model)?;
        Some(PhaseConfig {
            name: name.to_owned(),
            model: model.to_owned(),
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

    if let Some(sm) = set_model {
        let (phase_name, model_name) = (&sm[0], &sm[1]);
        validate_model(model_name)?;
        if !config.phases.iter().any(|p| &p.name == phase_name) {
            return Err(PhaseUpdateError::PhaseNotFound {
                name: phase_name.clone(),
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
                p.model = model_name.clone();
                break;
            }
        }
    }

    patch_phases(&config_path, &new_phases)?;

    let display: Vec<String> = new_phases
        .iter()
        .map(|p| format!("{}:{}", p.name, p.model))
        .collect();
    println!("phases: {}", display.join(" \u{2192} "));

    Ok(())
}

fn parse_phase_model_arg(input: &str) -> Result<(&str, &str), PhaseUpdateError> {
    input
        .split_once(':')
        .filter(|(name, model)| {
            !name.is_empty()
                && !model.is_empty()
                && !name.chars().any(char::is_whitespace)
                && !model.chars().any(char::is_whitespace)
        })
        .ok_or(PhaseUpdateError::InvalidAddFormat)
}

fn validate_model(model: &str) -> Result<(), PhaseUpdateError> {
    if VALID_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(PhaseUpdateError::UnknownModel {
            model: model.to_owned(),
        })
    }
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
    }
}

fn map_update_config_error(error: ConfigError) -> PhaseUpdateError {
    match error {
        ConfigError::NotFound { path } => PhaseUpdateError::ConfigNotFound { path },
        ConfigError::Read { path, source } => PhaseUpdateError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => PhaseUpdateError::ConfigLoad { path, source },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig, VerificationConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: "opus".to_owned(),
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: "sonnet".to_owned(),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: "haiku".to_owned(),
                },
            ],
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
        }
    }

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
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
        assert_eq!(phases[0].name, "enriching");
        assert_eq!(phases[1].name, "review");
        assert_eq!(phases[1].model, "sonnet");
        assert_eq!(phases[2].name, "in_progress");
        assert_eq!(phases[3].name, "done");
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
        assert_eq!(phases[2].name, "review");
        assert_eq!(phases[2].model, "sonnet");
        assert_eq!(phases[3].name, "done");
    }

    #[test]
    fn insert_appends_to_end_by_default() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(&project, Some("deployed:haiku"), None, None, None, None).unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.last().unwrap().name, "deployed");
        assert_eq!(phases.last().unwrap().model, "haiku");
    }

    #[test]
    fn invalid_add_format_returns_error() {
        let (_temp_dir, project, _config) = setup();
        for bad_input in ["review", ":sonnet", "review:", "review sonnet"] {
            let error =
                run_phase_update(&project, Some(bad_input), None, None, None, None).unwrap_err();
            assert!(
                matches!(error, PhaseUpdateError::InvalidAddFormat),
                "expected InvalidAddFormat for input '{bad_input}'"
            );
        }
    }

    #[test]
    fn unknown_model_in_add_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let error =
            run_phase_update(&project, Some("review:gpt4"), None, None, None, None).unwrap_err();
        assert!(matches!(error, PhaseUpdateError::UnknownModel { model } if model == "gpt4"));
    }

    #[test]
    fn remove_existing_phase() {
        let (_temp_dir, project, _config) = setup();
        run_phase_update(&project, None, None, None, Some("in_progress"), None).unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].name, "enriching");
        assert_eq!(phases[1].name, "done");
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
        store.add_task("Blocked task", "", None, vec![]).unwrap();
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
            None,
            Some("in_progress"),
            None,
        )
        .unwrap();
        let phases = loaded_phases(&project);
        assert_eq!(phases.len(), 3);
        assert!(!phases.iter().any(|p| p.name == "in_progress"));
        assert!(phases.iter().any(|p| p.name == "review"));
    }

    #[test]
    fn set_model_changes_existing_phase_model() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["enriching".to_owned(), "haiku".to_owned()];
        run_phase_update(&project, None, None, None, None, Some(&args)).unwrap();
        let phases = loaded_phases(&project);
        let enriching = phases.iter().find(|p| p.name == "enriching").unwrap();
        assert_eq!(enriching.model, "haiku");
    }

    #[test]
    fn set_model_unknown_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["nonexistent".to_owned(), "haiku".to_owned()];
        let error = run_phase_update(&project, None, None, None, None, Some(&args)).unwrap_err();
        assert!(matches!(error, PhaseUpdateError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn set_model_unknown_model_returns_error() {
        let (_temp_dir, project, _config) = setup();
        let args = vec!["enriching".to_owned(), "gpt4".to_owned()];
        let error = run_phase_update(&project, None, None, None, None, Some(&args)).unwrap_err();
        assert!(matches!(error, PhaseUpdateError::UnknownModel { model } if model == "gpt4"));
    }

    #[test]
    fn write_only_patches_phases_field() {
        let (_temp_dir, project, _config) = setup();
        let config_path = project.state_dir.join("config.json");
        let original = fs::read_to_string(&config_path).unwrap();
        let original_value: serde_json::Value = serde_json::from_str(&original).unwrap();

        run_phase_update(
            &project,
            Some("review:sonnet"),
            None,
            Some("done"),
            None,
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
        assert_eq!(phases.len(), 2);
    }

    #[test]
    fn multiple_tasks_blocked_removal_lists_all() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task A", "", None, vec![]).unwrap();
        store.add_task("Task B", "", None, vec![]).unwrap();
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
