use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum PhasesError {
    #[error("at least one of --add or --remove is required")]
    NoOperation,

    #[error("--after and --before cannot be used together")]
    ConflictingPositionFlags,

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

pub fn run_config_phases(
    project: &Project,
    add: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    remove: Option<&str>,
) -> Result<(), PhasesError> {
    if add.is_none() && remove.is_none() {
        return Err(PhasesError::NoOperation);
    }

    if after.is_some() && before.is_some() {
        return Err(PhasesError::ConflictingPositionFlags);
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    if let Some(after) = after {
        if !config.state_machine.contains(&after.to_owned()) {
            return Err(PhasesError::PhaseNotFound {
                name: after.to_owned(),
            });
        }
    }

    if let Some(before) = before {
        if !config.state_machine.contains(&before.to_owned()) {
            return Err(PhasesError::PhaseNotFound {
                name: before.to_owned(),
            });
        }
    }

    if let Some(add) = add {
        if config.state_machine.contains(&add.to_owned()) {
            return Err(PhasesError::DuplicatePhase {
                name: add.to_owned(),
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
            return Err(PhasesError::PhaseBusy {
                name: remove.to_owned(),
                task_ids: blocking_ids,
            });
        }
    }

    let mut new_phases = config.state_machine.clone();

    if let Some(remove) = remove {
        new_phases.retain(|phase| phase != remove);
    }

    if let Some(add) = add {
        if let Some(after) = after {
            let pos = new_phases.iter().position(|p| p == after).unwrap();
            new_phases.insert(pos + 1, add.to_owned());
        } else if let Some(before) = before {
            let pos = new_phases.iter().position(|p| p == before).unwrap();
            new_phases.insert(pos, add.to_owned());
        } else {
            new_phases.push(add.to_owned());
        }
    }

    patch_state_machine(&config_path, &new_phases)?;

    println!("state_machine: {}", new_phases.join(" → "));

    Ok(())
}

fn patch_state_machine(config_path: &Path, new_phases: &[String]) -> Result<(), PhasesError> {
    let contents = fs::read_to_string(config_path).map_err(|source| PhasesError::ConfigRead {
        path: config_path.to_path_buf(),
        source,
    })?;

    let mut value: serde_json::Value =
        serde_json::from_str(&contents).map_err(|source| PhasesError::ConfigLoad {
            path: config_path.to_path_buf(),
            source,
        })?;

    value["state_machine"] = serde_json::json!(new_phases);

    let bytes = serde_json::to_vec_pretty(&value).map_err(|source| PhasesError::ConfigLoad {
        path: config_path.to_path_buf(),
        source,
    })?;

    let tmp_path = config_path.with_extension("json.tmp");
    fs::write(&tmp_path, bytes).map_err(|source| PhasesError::ConfigWrite {
        path: config_path.to_path_buf(),
        source,
    })?;
    fs::rename(&tmp_path, config_path).map_err(|source| PhasesError::ConfigWrite {
        path: config_path.to_path_buf(),
        source,
    })?;

    Ok(())
}

fn map_config_error(error: ConfigError) -> PhasesError {
    match error {
        ConfigError::NotFound { path } => PhasesError::ConfigNotFound { path },
        ConfigError::Read { path, source } => PhasesError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => PhasesError::ConfigLoad { path, source },
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{Config, VerificationConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            state_machine: vec![
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "done".to_owned(),
            ],
            models: BTreeMap::new(),
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            default_model: "sonnet".to_owned(),
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

    fn loaded_phases(project: &Project) -> Vec<String> {
        let config_path = project.state_dir.join("config.json");
        let config = load_project_config(&config_path, &GlobalConfig::default()).unwrap();
        config.state_machine
    }

    // Named test: insert
    #[test]
    fn insert_after_existing_phase() {
        let (_temp_dir, project, _config) = setup();

        run_config_phases(&project, Some("review"), Some("enriching"), None, None).unwrap();

        assert_eq!(
            loaded_phases(&project),
            vec!["enriching", "review", "in_progress", "done"]
        );
    }

    // Named test: insert (before variant)
    #[test]
    fn insert_before_existing_phase() {
        let (_temp_dir, project, _config) = setup();

        run_config_phases(&project, Some("review"), None, Some("done"), None).unwrap();

        assert_eq!(
            loaded_phases(&project),
            vec!["enriching", "in_progress", "review", "done"]
        );
    }

    // Named test: insert (append-to-end variant)
    #[test]
    fn insert_appends_to_end_by_default() {
        let (_temp_dir, project, _config) = setup();

        run_config_phases(&project, Some("deployed"), None, None, None).unwrap();

        assert_eq!(
            loaded_phases(&project),
            vec!["enriching", "in_progress", "done", "deployed"]
        );
    }

    // Named test: remove
    #[test]
    fn remove_existing_phase() {
        let (_temp_dir, project, _config) = setup();

        run_config_phases(&project, None, None, None, Some("in_progress")).unwrap();

        assert_eq!(loaded_phases(&project), vec!["enriching", "done"]);
    }

    // Named test: duplicate-name rejection
    #[test]
    fn duplicate_name_rejection() {
        let (_temp_dir, project, _config) = setup();

        let error = run_config_phases(&project, Some("enriching"), None, None, None).unwrap_err();

        assert!(matches!(error, PhasesError::DuplicatePhase { name } if name == "enriching"));
    }

    // Named test: blocked-removal (with task IDs asserted)
    #[test]
    fn blocked_removal_lists_task_ids() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Blocked task", "", None, vec![]).unwrap();

        let error = run_config_phases(&project, None, None, None, Some("enriching")).unwrap_err();

        match error {
            PhasesError::PhaseBusy { name, task_ids } => {
                assert_eq!(name, "enriching");
                assert_eq!(task_ids, vec!["task-001"]);
            }
            other => panic!("expected PhaseBusy, got: {other}"),
        }
    }

    #[test]
    fn no_operation_returns_error() {
        let (_temp_dir, project, _config) = setup();

        let error = run_config_phases(&project, None, None, None, None).unwrap_err();

        assert!(matches!(error, PhasesError::NoOperation));
    }

    #[test]
    fn after_and_before_together_returns_error() {
        let (_temp_dir, project, _config) = setup();

        let error = run_config_phases(
            &project,
            Some("new"),
            Some("done"),
            Some("in_progress"),
            None,
        )
        .unwrap_err();

        assert!(matches!(error, PhasesError::ConflictingPositionFlags));
    }

    #[test]
    fn after_nonexistent_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();

        let error =
            run_config_phases(&project, Some("new"), Some("nonexistent"), None, None).unwrap_err();

        assert!(matches!(error, PhasesError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn before_nonexistent_phase_returns_error() {
        let (_temp_dir, project, _config) = setup();

        let error =
            run_config_phases(&project, Some("new"), None, Some("nonexistent"), None).unwrap_err();

        assert!(matches!(error, PhasesError::PhaseNotFound { name } if name == "nonexistent"));
    }

    #[test]
    fn add_and_remove_together() {
        let (_temp_dir, project, _config) = setup();

        run_config_phases(&project, Some("review"), None, None, Some("in_progress")).unwrap();

        assert_eq!(loaded_phases(&project), vec!["enriching", "done", "review"]);
    }

    #[test]
    fn write_only_patches_state_machine_field() {
        let (_temp_dir, project, _config) = setup();
        let config_path = project.state_dir.join("config.json");
        let original = fs::read_to_string(&config_path).unwrap();
        let original_value: serde_json::Value = serde_json::from_str(&original).unwrap();

        run_config_phases(&project, Some("review"), None, Some("done"), None).unwrap();

        let updated = fs::read_to_string(&config_path).unwrap();
        let updated_value: serde_json::Value = serde_json::from_str(&updated).unwrap();

        // Every field except state_machine must be unchanged
        for (key, orig_val) in original_value.as_object().unwrap() {
            if key != "state_machine" {
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

        run_config_phases(&project, None, None, None, Some("in_progress")).unwrap();

        assert_eq!(loaded_phases(&project), vec!["enriching", "done"]);
    }

    #[test]
    fn multiple_tasks_blocked_removal_lists_all() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task A", "", None, vec![]).unwrap();
        store.add_task("Task B", "", None, vec![]).unwrap();

        let error = run_config_phases(&project, None, None, None, Some("enriching")).unwrap_err();

        match error {
            PhasesError::PhaseBusy { task_ids, .. } => {
                assert!(task_ids.contains(&"task-001".to_owned()));
                assert!(task_ids.contains(&"task-002".to_owned()));
            }
            other => panic!("expected PhaseBusy, got: {other}"),
        }
    }
}
