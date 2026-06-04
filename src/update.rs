use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, TaskStore},
};

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("at least one of --title, --description, --prd, or --depends-on is required")]
    NoFields,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("unknown dependency: {id}")]
    UnknownDependency { id: String },

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

    #[error(transparent)]
    StoreError(#[from] StoreError),
}

pub struct UpdateInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub prd: Option<String>,
    pub depends_on: Option<Vec<String>>,
}

pub fn run_update(project: &Project, id: &str, input: UpdateInput) -> Result<(), UpdateError> {
    if input.title.is_none()
        && input.description.is_none()
        && input.prd.is_none()
        && input.depends_on.is_none()
    {
        return Err(UpdateError::NoFields);
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    let mut store = TaskStore::new(&project.state_dir, &config)?;

    let depends_on_ref = input.depends_on.as_deref();
    let result = store.update_task(
        id,
        input.title.as_deref(),
        input.description.as_deref(),
        input.prd.as_deref(),
        depends_on_ref,
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
        Err(error) => Err(error.into()),
    }
}

fn map_config_error(error: ConfigError) -> UpdateError {
    match error {
        ConfigError::NotFound { path } => UpdateError::ConfigNotFound { path },
        ConfigError::Read { path, source } => UpdateError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => UpdateError::ConfigLoad { path, source },
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

    fn test_project_with_config() -> (TempDir, Project, Config) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        write_config(&project, &config);
        (temp_dir, project, config)
    }

    fn capture_output<F>(run: F) -> (Result<(), UpdateError>, String)
    where
        F: FnOnce() -> Result<(), UpdateError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn no_fields_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_update(
            &project,
            "task-001",
            UpdateInput {
                title: None,
                description: None,
                prd: None,
                depends_on: None,
            },
        )
        .unwrap_err();

        assert!(matches!(error, UpdateError::NoFields));
        assert_eq!(
            error.to_string(),
            "at least one of --title, --description, --prd, or --depends-on is required"
        );
    }

    #[test]
    fn unknown_task_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let error = run_update(
            &project,
            "task-999",
            UpdateInput {
                title: Some("New title".to_owned()),
                description: None,
                prd: None,
                depends_on: None,
            },
        )
        .unwrap_err();

        match error {
            UpdateError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn update_title_persists() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Original title", "", None, vec![]).unwrap();

        let (result, output) = capture_output(|| {
            run_update(
                &project,
                "task-001",
                UpdateInput {
                    title: Some("Updated title".to_owned()),
                    description: None,
                    prd: None,
                    depends_on: None,
                },
            )
        });
        result.unwrap();

        assert_eq!(output, "task-001 updated\n");
        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap().title, "Updated title");
    }

    #[test]
    fn update_description_persists() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Task", "original desc", None, vec![])
            .unwrap();

        run_update(
            &project,
            "task-001",
            UpdateInput {
                title: None,
                description: Some("new desc".to_owned()),
                prd: None,
                depends_on: None,
            },
        )
        .unwrap();

        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap().description, "new desc");
    }

    #[test]
    fn update_prd_persists() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", None, vec![]).unwrap();

        run_update(
            &project,
            "task-001",
            UpdateInput {
                title: None,
                description: None,
                prd: Some("FM-042".to_owned()),
                depends_on: None,
            },
        )
        .unwrap();

        let store = test_store(&temp_dir, &config);
        assert_eq!(
            store.get_task("task-001").unwrap().prd_module_id.as_deref(),
            Some("FM-042")
        );
    }

    #[test]
    fn update_depends_on_replaces_list() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Dep A", "", None, vec![]).unwrap();
        store.add_task("Dep B", "", None, vec![]).unwrap();
        store
            .add_task("Subject", "", None, vec!["task-001".to_owned()])
            .unwrap();

        run_update(
            &project,
            "task-003",
            UpdateInput {
                title: None,
                description: None,
                prd: None,
                depends_on: Some(vec!["task-002".to_owned()]),
            },
        )
        .unwrap();

        let store = test_store(&temp_dir, &config);
        assert_eq!(
            store.get_task("task-003").unwrap().dependencies,
            vec!["task-002".to_owned()]
        );
    }

    #[test]
    fn update_depends_on_unknown_dep_returns_error() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", None, vec![]).unwrap();

        let error = run_update(
            &project,
            "task-001",
            UpdateInput {
                title: None,
                description: None,
                prd: None,
                depends_on: Some(vec!["task-999".to_owned()]),
            },
        )
        .unwrap_err();

        match error {
            UpdateError::UnknownDependency { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn update_terminal_task_allowed() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let result = run_update(
            &project,
            "task-001",
            UpdateInput {
                title: Some("Updated terminal".to_owned()),
                description: None,
                prd: None,
                depends_on: None,
            },
        );

        result.unwrap();
        let store = test_store(&temp_dir, &config);
        assert_eq!(
            store.get_task("task-001").unwrap().title,
            "Updated terminal"
        );
    }

    #[test]
    fn update_failed_task_allowed() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", None, vec![]).unwrap();
        store.fail_task("task-001", "oops").unwrap();

        let result = run_update(
            &project,
            "task-001",
            UpdateInput {
                title: Some("Updated failed".to_owned()),
                description: None,
                prd: None,
                depends_on: None,
            },
        );

        result.unwrap();
        let store = test_store(&temp_dir, &config);
        assert_eq!(store.get_task("task-001").unwrap().title, "Updated failed");
    }

    #[test]
    fn update_multiple_fields_at_once() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Old title", "old desc", None, vec![])
            .unwrap();

        run_update(
            &project,
            "task-001",
            UpdateInput {
                title: Some("New title".to_owned()),
                description: Some("new desc".to_owned()),
                prd: Some("FM-001".to_owned()),
                depends_on: None,
            },
        )
        .unwrap();

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.title, "New title");
        assert_eq!(task.description, "new desc");
        assert_eq!(task.prd_module_id.as_deref(), Some("FM-001"));
    }
}
