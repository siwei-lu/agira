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

pub fn run_add(
    project: &Project,
    title: &str,
    description: Option<&str>,
    prd_module_id: Option<&str>,
    depends_on: &[String],
) -> Result<(), AddError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    if config.phases.is_empty() {
        return Err(AddError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        });
    }

    let mut store = TaskStore::new(&project.state_dir, &config)?;
    add_task_flow(
        project,
        &mut store,
        title,
        description.unwrap_or(""),
        prd_module_id.map(ToOwned::to_owned),
        depends_on.to_vec(),
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

fn add_task_flow(
    project: &Project,
    store: &mut TaskStore,
    title: &str,
    description: &str,
    prd_module_id: Option<String>,
    depends_on: Vec<String>,
) -> Result<(), AddError> {
    let task = match store.add_task(title, description, prd_module_id, depends_on) {
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
        &HookContext::new(task, &project.slug, &project.git_root, "", &task.state, ""),
    );
}

fn map_store_error(error: StoreError) -> AddError {
    match error {
        StoreError::DependencyBlocked { blocking_id, .. } => {
            AddError::UnknownDependency { id: blocking_id }
        }
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig, VerificationConfig},
        global_config::GlobalConfig,
        tasks::{TaskStore, TasksFile},
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: "sonnet".to_owned(),
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: "opus".to_owned(),
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
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        }
    }

    fn write_config(project: &Project, config: &Config) {
        let contents = serde_json::to_vec_pretty(config).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn write_config_without_max_retries(project: &Project) {
        fs::write(
            project.state_dir.join("config.json"),
            r#"{
  "stack": "rust",
  "phases": [{"name":"enriching","model":"opus"},{"name":"done","model":"haiku"}],
  "verification": { "commands": [] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();
    }

    fn test_project_with_config() -> (TempDir, Project, Config) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        write_config(&project, &config);

        (temp_dir, project, config)
    }

    fn read_tasks(project: &Project) -> TasksFile {
        let contents = fs::read_to_string(project.state_dir.join("tasks.json")).unwrap();
        serde_json::from_str(&contents).unwrap()
    }

    fn capture_output<F>(run: F) -> (Result<(), AddError>, String)
    where
        F: FnOnce() -> Result<(), AddError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });

        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());

        (result, output)
    }

    #[test]
    fn add_task_creates_with_first_phase() {
        let (_temp_dir, project, config) = test_project_with_config();

        let (result, output) =
            capture_output(|| run_add(&project, "Implement login endpoint", None, None, &[]));
        result.unwrap();

        assert_eq!(output, "added task-001: Implement login endpoint\n");

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks.len(), 1);

        let task = &tasks_file.tasks[0];
        assert_eq!(task.id, "task-001");
        assert_eq!(task.title, "Implement login endpoint");
        assert_eq!(task.description, "");
        assert_eq!(task.prd_module_id, None);
        assert_eq!(task.state, config.phases[0].name);
        assert!(task.dependencies.is_empty());
        assert_eq!(task.retry_count, 0);
        assert_eq!(task.max_retries, config.max_retries);
        assert!(task.phases.is_empty());
        assert!(DateTime::parse_from_rfc3339(&task.created_at).is_ok());

        let history = task.history.as_slice();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].from, None);
        assert_eq!(history[0].to, "pending");
        assert_eq!(history[0].reason, "task created");
        assert!(DateTime::parse_from_rfc3339(&history[0].timestamp).is_ok());
    }

    #[test]
    fn add_task_with_dependencies() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Prepare", None, None, &[]))
            .0
            .unwrap();

        let depends_on = vec!["task-001".to_owned()];
        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Deploy",
                Some("ship release"),
                Some("FM-007"),
                &depends_on,
            )
        });
        result.unwrap();

        assert_eq!(output, "added task-002: Deploy\n");

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks.len(), 2);

        let task = &tasks_file.tasks[1];
        assert_eq!(task.id, "task-002");
        assert_eq!(task.description, "ship release");
        assert_eq!(task.prd_module_id.as_deref(), Some("FM-007"));
        assert_eq!(task.dependencies, vec!["task-001".to_owned()]);
    }

    #[test]
    fn unknown_dependency_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();
        let depends_on = vec!["task-999".to_owned()];

        let (result, output) =
            capture_output(|| run_add(&project, "Blocked", None, None, &depends_on));
        let error = result.unwrap_err();

        match &error {
            AddError::UnknownDependency { id } => assert_eq!(id, "task-999"),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(output, "");
        assert_eq!(error.to_string(), "unknown dependency: task-999");
        assert!(!project.state_dir.join("tasks.json").exists());
    }

    #[test]
    fn add_multiple_tasks_sequential_ids() {
        let (_temp_dir, project, _config) = test_project_with_config();

        for title in ["First", "Second", "Third"] {
            capture_output(|| run_add(&project, title, None, None, &[]))
                .0
                .unwrap();
        }

        let tasks_file = read_tasks(&project);
        let ids: Vec<&str> = tasks_file
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect();
        assert_eq!(ids, ["task-001", "task-002", "task-003"]);
    }

    #[test]
    fn add_task_uses_global_max_retries_when_project_config_omits_it() {
        let temp_dir = TempDir::new().unwrap();
        let mut project = test_project(&temp_dir);
        project.global_config.default_max_retries = 5;
        write_config_without_max_retries(&project);

        capture_output(|| run_add(&project, "Uses global retries", None, None, &[]))
            .0
            .unwrap();

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks[0].max_retries, 5);
    }

    #[test]
    fn config_load_errors_and_empty_phase_list_normalization() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_add(&project, "Missing config", None, None, &[]).unwrap_err();
        assert!(matches!(error, AddError::ConfigNotFound { .. }));

        fs::write(project.state_dir.join("config.json"), "{").unwrap();
        let error = run_add(&project, "Malformed config", None, None, &[]).unwrap_err();
        assert!(matches!(error, AddError::ConfigLoad { .. }));

        let mut config = test_config();
        config.phases.clear();
        write_config(&project, &config);
        run_add(&project, "Mandatory-only config", None, None, &[]).unwrap();
        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks[0].state, "pending");
    }

    #[test]
    fn add_task_flow_maps_unknown_dependency_without_saving() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        let mut store = TaskStore::new(temp_dir.path(), &config).unwrap();

        let error = add_task_flow(
            &project,
            &mut store,
            "Blocked",
            "",
            None,
            vec!["task-999".to_owned()],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AddError::UnknownDependency { id } if id == "task-999"
        ));
        assert!(!temp_dir.path().join("tasks.json").exists());
    }
}
