use std::{
    fs, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use thiserror::Error;

use crate::{
    advance::{commit_prompt, read_recent_commits},
    config::{ConfigError, load_project_config},
    pick::{format_pick_output, select_next_task},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum WorkError {
    #[error("no actionable task — all tasks are done or blocked")]
    NoActionableTask,

    #[error("artifact must not be empty")]
    EmptyArtifact,

    #[error("prd file not found: {path}")]
    PrdNotFound { path: PathBuf },

    #[error("failed to read {path}")]
    Io {
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

pub fn run_work(
    project: &Project,
    prd_path: Option<&Path>,
    artifact: Option<&str>,
) -> Result<(), WorkError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let prd_content = prd_path.map(read_prd).transpose()?;

    match artifact {
        None => {
            let store = TaskStore::new(&project.state_dir, &config)?;
            let output =
                format_pick_output(&config, store.all_tasks(), prd_content.as_deref(), None);
            print_work_output(&output);
        }
        Some(artifact) => {
            if artifact.trim().is_empty() {
                return Err(WorkError::EmptyArtifact);
            }

            let terminal_phase = config
                .terminal_phase()
                .ok_or_else(|| WorkError::InvalidConfig {
                    path: config_path.clone(),
                    reason: "phases must not be empty".to_owned(),
                })?
                .to_owned();

            let mut store = TaskStore::new(&project.state_dir, &config)?;

            let (task_id, task_title) = {
                let current_task = select_next_task(store.all_tasks(), &config)
                    .ok_or(WorkError::NoActionableTask)?;
                (current_task.id.clone(), current_task.title.clone())
            };

            let completed_at = Utc::now().to_rfc3339();
            store.record_phase_artifact(&task_id, artifact, completed_at)?;
            store.next_phase(&task_id)?;

            let resulting_state = store.get_task(&task_id).unwrap().state.clone();

            if resulting_state == terminal_phase {
                print_work_output(&format!("{task_id} done ✓"));
                let convention = read_recent_commits(&project.git_root);
                print_work_output(&commit_prompt(&task_id, &task_title, convention.as_deref()));
                let next_output = format_pick_output(
                    &config,
                    store.all_tasks(),
                    None,
                    Some((&task_id, &task_title)),
                );
                print_work_output(&next_output);
            } else {
                print_work_output(&format!("{task_id} → {resulting_state}"));
            }
        }
    }

    Ok(())
}

fn read_prd(path: &Path) -> Result<String, WorkError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(WorkError::PrdNotFound {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(WorkError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn map_config_error(error: ConfigError) -> WorkError {
    match error {
        ConfigError::NotFound { path } => WorkError::Io {
            path,
            source: io::Error::new(io::ErrorKind::NotFound, "config file not found"),
        },
        ConfigError::Read { path, source } => WorkError::Io { path, source },
        ConfigError::Parse { path, source } => WorkError::ConfigLoad { path, source },
    }
}

fn print_work_output(message: &str) {
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::{
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
            max_retries: 3,
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
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

    fn capture_output<F>(run: F) -> (Result<(), WorkError>, String)
    where
        F: FnOnce() -> Result<(), WorkError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn no_artifact_prints_task_prompt() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("My task", "description", None, vec![])
            .unwrap();

        let (result, output) = capture_output(|| run_work(&project, None, None));

        result.unwrap();
        assert!(output.contains("# Agira Task Prompt"));
        assert!(output.contains("My task"));
    }

    #[test]
    fn no_artifact_all_done_shows_completion_summary() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Done task", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) = capture_output(|| run_work(&project, None, None));

        result.unwrap();
        assert!(output.contains("# Agira Completion Summary"));
    }

    #[test]
    fn task_prompt_references_work_command() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", None, vec![]).unwrap();

        let (result, output) = capture_output(|| run_work(&project, None, None));

        result.unwrap();
        assert!(output.contains("agira task work --artifact"));
    }

    #[test]
    fn with_artifact_non_terminal_shows_arrow() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", None, vec![]).unwrap();

        let (result, output) = capture_output(|| run_work(&project, None, Some("enriched")));

        result.unwrap();
        assert!(output.contains("task-001 → in_progress"));
        assert!(!output.contains("# Commit"));
        assert!(!output.contains("# Agira Task Prompt"));
    }

    #[test]
    fn with_artifact_terminal_shows_commit_and_next_task() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First task", "", None, vec![]).unwrap();
        // task-002 depends on task-001 so it is blocked until task-001 is done
        store
            .add_task("Second task", "", None, vec!["task-001".to_owned()])
            .unwrap();
        store.next_phase("task-001").unwrap(); // enriching → in_progress

        let (result, output) = capture_output(|| run_work(&project, None, Some("implemented")));

        result.unwrap();
        assert!(output.contains("task-001 done ✓"));
        assert!(output.contains("# Commit"));
        assert!(output.contains("# Agira Task Prompt"));
        assert!(output.contains("Second task"));
    }

    #[test]
    fn with_artifact_terminal_all_done_shows_completion() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Only task", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) = capture_output(|| run_work(&project, None, Some("implemented")));

        result.unwrap();
        assert!(output.contains("task-001 done ✓"));
        assert!(output.contains("# Commit"));
        assert!(output.contains("# Agira Completion Summary"));
    }

    #[test]
    fn with_artifact_no_actionable_task_returns_error() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Done task", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_work(&project, None, Some("artifact")).unwrap_err();

        assert!(matches!(error, WorkError::NoActionableTask));
    }

    #[test]
    fn empty_artifact_returns_error() {
        let (_temp_dir, project, _config) = setup();

        for artifact in ["", "  "] {
            let error = run_work(&project, None, Some(artifact)).unwrap_err();
            assert!(matches!(error, WorkError::EmptyArtifact));
        }
    }

    #[test]
    fn with_artifact_records_in_store() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", None, vec![]).unwrap();

        let (result, _) = capture_output(|| run_work(&project, None, Some("done it")));
        result.unwrap();

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "in_progress");
        let phase = task.phases.get("enriching").unwrap();
        assert_eq!(phase.artifact, "done it");
    }

    #[test]
    fn no_artifact_no_tasks_shows_no_tasks_message() {
        let (_temp_dir, project, _config) = setup();

        let (result, output) = capture_output(|| run_work(&project, None, None));

        result.unwrap();
        assert!(output.contains("agira task add"));
    }
}
