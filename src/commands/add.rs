use std::{collections::HashSet, io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{
        ConfigError, INITIAL_PHASE_NAME, PhaseConfig, TERMINAL_PHASE_NAME, load_project_config,
        normalize_mandatory_phases,
    },
    hooks::{HookContext, TASK_ADDED_EVENT, dispatch_hooks, hooks_for_event},
    project::Project,
    tasks::{StoreError, TaskPhaseConfig, TaskStore},
};

#[derive(Debug, Error)]
pub enum AddError {
    #[error("unknown dependency: {id}")]
    UnknownDependency { id: String },

    #[error("unknown phase: {phase}")]
    UnknownPhase { phase: String },

    #[error("invalid --phases: {reason}")]
    InvalidPhases { reason: String },

    #[error("invalid --duties: {reason}")]
    InvalidDuties { reason: String },

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

pub fn run_add(
    project: &Project,
    title: &str,
    description: Option<&str>,
    depends_on: &[String],
    phase: Option<&str>,
    phases: Option<&str>,
    duties: Option<&[String]>,
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

    let mut state_machine = phases
        .map(parse_phases)
        .transpose()
        .map_err(|reason| AddError::InvalidPhases { reason })?;

    if let Some(duty_entries) = duties {
        if state_machine.is_none() {
            return Err(AddError::InvalidDuties {
                reason: "--phases is required when --duties is used".to_owned(),
            });
        }
        apply_duties(duty_entries, state_machine.as_mut().unwrap(), &config)?;
    }

    add_task_flow(
        project,
        &mut store,
        title,
        description.unwrap_or(""),
        depends_on.to_vec(),
        phase,
        state_machine,
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

fn parse_phases(input: &str) -> Result<Vec<TaskPhaseConfig>, String> {
    let mut parsed = Vec::new();

    for entry in input.split(',') {
        let (name, model) = match entry.split_once(':') {
            Some((name, model)) => {
                validate_phase_name(name)?;
                if model.is_empty() {
                    return Err(format!("empty model for phase '{name}'"));
                }
                if !is_valid_phase_label(model) {
                    return Err(format!("invalid model for phase '{name}'"));
                }
                (name, Some(model.to_owned()))
            }
            None => {
                validate_phase_name(entry)?;
                (entry, None)
            }
        };

        parsed.push(TaskPhaseConfig {
            name: name.to_owned(),
            model,
            duty: None,
        });
    }

    let mut names = HashSet::new();
    for phase in &parsed {
        if !names.insert(phase.name.as_str()) {
            return Err(format!("duplicate phase '{}'", phase.name));
        }
    }

    let phase_configs = parsed
        .into_iter()
        .map(|phase| PhaseConfig {
            name: phase.name,
            model: phase.model,
            duty: None,
        })
        .collect();

    let normalized = normalize_mandatory_phases(phase_configs);
    debug_assert_eq!(
        normalized.first().map(|phase| phase.name.as_str()),
        Some(INITIAL_PHASE_NAME)
    );
    debug_assert_eq!(
        normalized.last().map(|phase| phase.name.as_str()),
        Some(TERMINAL_PHASE_NAME)
    );

    Ok(normalized
        .into_iter()
        .map(|phase| TaskPhaseConfig {
            name: phase.name,
            model: phase.model,
            duty: None,
        })
        .collect())
}

fn apply_duties(
    duty_entries: &[String],
    state_machine: &mut [TaskPhaseConfig],
    config: &crate::core::config::Config,
) -> Result<(), AddError> {
    let mut seen = HashSet::new();

    for entry in duty_entries {
        let Some((phase_name, duty_text)) = entry.split_once(':') else {
            return Err(AddError::InvalidDuties {
                reason: format!("invalid format '{entry}': expected PHASE:DUTY"),
            });
        };

        if duty_text.is_empty() {
            return Err(AddError::InvalidDuties {
                reason: format!("empty duty for phase '{phase_name}'"),
            });
        }

        if !seen.insert(phase_name.to_owned()) {
            return Err(AddError::InvalidDuties {
                reason: format!("duplicate phase '{phase_name}' in --duties"),
            });
        }

        if phase_name == INITIAL_PHASE_NAME || phase_name == TERMINAL_PHASE_NAME {
            return Err(AddError::InvalidDuties {
                reason: format!(
                    "cannot set duty on mandatory phase '{phase_name}': pending and done are transition phases with no duty"
                ),
            });
        }

        if config.phases.iter().any(|p| p.name == phase_name) {
            return Err(AddError::InvalidDuties {
                reason: format!(
                    "cannot set duty on existing phase '{phase_name}'; use 'agira phase update --set-duty'"
                ),
            });
        }

        let phase = state_machine
            .iter_mut()
            .find(|p| p.name == phase_name)
            .ok_or_else(|| AddError::InvalidDuties {
                reason: format!("phase '{phase_name}' not found in --phases"),
            })?;

        phase.duty = Some(duty_text.to_owned());
    }

    Ok(())
}

fn validate_phase_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("empty phase name".to_owned());
    }
    if !is_valid_phase_label(name) {
        return Err(format!("invalid phase name '{name}'"));
    }
    Ok(())
}

fn is_valid_phase_label(label: &str) -> bool {
    !label.trim().is_empty()
}

fn add_task_flow(
    project: &Project,
    store: &mut TaskStore,
    title: &str,
    description: &str,
    depends_on: Vec<String>,
    phase: Option<&str>,
    state_machine: Option<Vec<TaskPhaseConfig>>,
) -> Result<(), AddError> {
    let task = match store.add_task(title, description, depends_on, phase, state_machine) {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig},
        global_config::GlobalConfig,
        tasks::{TaskStore, TasksFile},
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

    fn write_config_without_max_retries(project: &Project) {
        fs::write(
            project.state_dir.join("config.json"),
            r#"{
  "stack": "rust",
  "phases": [{"name":"enriching","model":"opus"},{"name":"done","model":"haiku"}],
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

    fn phase_names(phases: &[crate::core::tasks::TaskPhaseConfig]) -> Vec<&str> {
        phases.iter().map(|phase| phase.name.as_str()).collect()
    }

    #[test]
    fn parse_simple_list() {
        let phases = parse_phases("pending,in_progress,done").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert!(phases.iter().all(|phase| phase.model.is_none()));
    }

    #[test]
    fn parse_with_models() {
        let phases = parse_phases("pending,in_progress:sonnet,done").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert_eq!(phases[1].model.as_deref(), Some("sonnet"));
        assert!(phases[0].model.is_none());
        assert!(phases[2].model.is_none());
    }

    #[test]
    fn parse_with_spaced_model_label() {
        let phases = parse_phases("pending,in_progress:dispatch -a codex,done").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert_eq!(phases[1].model.as_deref(), Some("dispatch -a codex"));
    }

    #[test]
    fn normalize_prepends_pending() {
        let phases = parse_phases("in_progress:sonnet,done").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert!(phases[0].model.is_none());
    }

    #[test]
    fn normalize_appends_done() {
        let phases = parse_phases("pending,in_progress:sonnet").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert!(phases.last().unwrap().model.is_none());
    }

    #[test]
    fn normalize_repositions_done_not_last() {
        let phases = parse_phases("in_progress,done,verifying").unwrap();

        assert_eq!(
            phase_names(&phases),
            ["pending", "in_progress", "verifying", "done"]
        );
    }

    #[test]
    fn normalize_strips_model_from_mandatory() {
        let phases = parse_phases("pending:opus,in_progress:sonnet,done:haiku").unwrap();

        assert_eq!(phase_names(&phases), ["pending", "in_progress", "done"]);
        assert!(phases[0].model.is_none());
        assert_eq!(phases[1].model.as_deref(), Some("sonnet"));
        assert!(phases[2].model.is_none());
    }

    #[test]
    fn reject_empty_name() {
        let error = parse_phases(",in_progress").unwrap_err();

        assert!(error.contains("empty phase name"));
        assert_eq!(
            AddError::InvalidPhases { reason: error }.to_string(),
            "invalid --phases: empty phase name"
        );

        let error = parse_phases(":opus").unwrap_err();
        assert!(error.contains("empty phase name"));

        let error = parse_phases("   :opus").unwrap_err();
        assert!(error.contains("invalid phase name"));
    }

    #[test]
    fn reject_duplicate() {
        let error = parse_phases("pending,in_progress,in_progress,done").unwrap_err();

        assert!(error.contains("duplicate phase 'in_progress'"));
    }

    #[test]
    fn reject_empty_model() {
        let error = parse_phases("enriching:").unwrap_err();

        assert!(error.contains("empty model"));

        let error = parse_phases("enriching:   ").unwrap_err();
        assert!(error.contains("invalid model"));
    }

    #[test]
    fn add_task_creates_with_first_phase() {
        let (_temp_dir, project, config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Implement login endpoint",
                None,
                &[],
                None,
                None,
                None,
            )
        });
        result.unwrap();

        assert_eq!(output, "added task-001: Implement login endpoint\n");

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks.len(), 1);

        let task = &tasks_file.tasks[0];
        assert_eq!(task.id, "task-001");
        assert_eq!(task.title, "Implement login endpoint");
        assert_eq!(task.description, "");
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
    fn run_add_with_phases_stores_state_machine() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Review custom phase",
                None,
                &[],
                None,
                Some("pending,security_review:opus,done"),
                None,
            )
        });
        result.unwrap();

        assert_eq!(output, "added task-001: Review custom phase\n");

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        let state_machine = task.state_machine.as_ref().unwrap();
        assert_eq!(task.state, "pending");
        assert_eq!(
            phase_names(state_machine),
            ["pending", "security_review", "done"]
        );
        assert_eq!(state_machine[1].model.as_deref(), Some("opus"));
    }

    #[test]
    fn run_add_invalid_phases_no_write() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Bad phases",
                None,
                &[],
                None,
                Some("pending,in_progress,in_progress,done"),
                None,
            )
        });
        let error = result.unwrap_err();

        match &error {
            AddError::InvalidPhases { reason } => {
                assert_eq!(reason, "duplicate phase 'in_progress'");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            error.to_string(),
            "invalid --phases: duplicate phase 'in_progress'"
        );
        assert_eq!(output, "");
        assert!(!project.state_dir.join("tasks.json").exists());
    }

    #[test]
    fn add_task_with_dependencies() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Prepare", None, &[], None, None, None))
            .0
            .unwrap();

        let depends_on = vec!["task-001".to_owned()];
        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Deploy",
                Some("ship release"),
                &depends_on,
                None,
                None,
                None,
            )
        });
        result.unwrap();

        assert_eq!(output, "added task-002: Deploy\n");

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks.len(), 2);

        let task = &tasks_file.tasks[1];
        assert_eq!(task.id, "task-002");
        assert_eq!(task.description, "ship release");
        assert_eq!(task.dependencies, vec!["task-001".to_owned()]);
    }

    #[test]
    fn unknown_dependency_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();
        let depends_on = vec!["task-999".to_owned()];

        let (result, output) =
            capture_output(|| run_add(&project, "Blocked", None, &depends_on, None, None, None));
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
    fn add_task_with_unknown_phase_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Review me",
                None,
                &[],
                Some("reviewing"),
                None,
                None,
            )
        });
        let error = result.unwrap_err();

        match &error {
            AddError::UnknownPhase { phase } => assert_eq!(phase, "reviewing"),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(output, "");
        assert_eq!(error.to_string(), "unknown phase: reviewing");
        assert!(!project.state_dir.join("tasks.json").exists());
    }

    #[test]
    fn add_task_with_same_case_duplicate_title_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Deploy", None, &[], None, None, None))
            .0
            .unwrap();

        let (result, output) =
            capture_output(|| run_add(&project, "Deploy", None, &[], None, None, None));
        let error = result.unwrap_err();

        match &error {
            AddError::DuplicateTitle { id, title } => {
                assert_eq!(id, "task-001");
                assert_eq!(title, "Deploy");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(output, "");
        assert_eq!(
            error.to_string(),
            "a task with this title already exists: task-001 \"Deploy\""
        );
        assert_eq!(read_tasks(&project).tasks.len(), 1);
    }

    #[test]
    fn add_task_with_case_insensitive_duplicate_title_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Deploy", None, &[], None, None, None))
            .0
            .unwrap();

        let (result, output) =
            capture_output(|| run_add(&project, "deploy", None, &[], None, None, None));
        let error = result.unwrap_err();

        match &error {
            AddError::DuplicateTitle { id, title } => {
                assert_eq!(id, "task-001");
                assert_eq!(title, "Deploy");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(output, "");
        assert_eq!(
            error.to_string(),
            "a task with this title already exists: task-001 \"Deploy\""
        );
        assert_eq!(read_tasks(&project).tasks.len(), 1);
    }

    #[test]
    fn add_task_allows_duplicate_title_when_existing_task_is_done() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Repeatable", None, &[], Some("done"), None, None))
            .0
            .unwrap();

        capture_output(|| run_add(&project, "Repeatable", None, &[], None, None, None))
            .0
            .unwrap();

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks.len(), 2);
        assert_eq!(tasks_file.tasks[1].id, "task-002");
    }

    #[test]
    fn add_task_allows_distinct_titles() {
        let (_temp_dir, project, _config) = test_project_with_config();
        capture_output(|| run_add(&project, "Deploy", None, &[], None, None, None))
            .0
            .unwrap();
        capture_output(|| run_add(&project, "Release", None, &[], None, None, None))
            .0
            .unwrap();

        assert_eq!(read_tasks(&project).tasks.len(), 2);
    }

    #[test]
    fn add_multiple_tasks_sequential_ids() {
        let (_temp_dir, project, _config) = test_project_with_config();

        for title in ["First", "Second", "Third"] {
            capture_output(|| run_add(&project, title, None, &[], None, None, None))
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

        capture_output(|| run_add(&project, "Uses global retries", None, &[], None, None, None))
            .0
            .unwrap();

        let tasks_file = read_tasks(&project);
        assert_eq!(tasks_file.tasks[0].max_retries, 5);
    }

    #[test]
    fn config_load_errors_and_empty_phase_list_normalization() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_add(&project, "Missing config", None, &[], None, None, None).unwrap_err();
        assert!(matches!(error, AddError::ConfigNotFound { .. }));

        fs::write(project.state_dir.join("config.json"), "{").unwrap();
        let error = run_add(&project, "Malformed config", None, &[], None, None, None).unwrap_err();
        assert!(matches!(error, AddError::ConfigLoad { .. }));

        let mut config = test_config();
        config.phases.clear();
        write_config(&project, &config);
        run_add(
            &project,
            "Mandatory-only config",
            None,
            &[],
            None,
            None,
            None,
        )
        .unwrap();
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
            vec!["task-999".to_owned()],
            None,
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AddError::UnknownDependency { id } if id == "task-999"
        ));
        assert!(!temp_dir.path().join("tasks.json").exists());
    }

    #[test]
    fn add_task_with_phase_override_places_task_in_specified_phase() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Backfilled task",
                None,
                &[],
                Some("enriching"),
                None,
                None,
            )
        });
        result.unwrap();

        assert_eq!(output, "added task-001: Backfilled task\n");

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        assert_eq!(task.state, "enriching");
        assert_eq!(task.history[0].to, "enriching");
    }

    #[test]
    fn add_task_with_unknown_phase_override_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let (result, output) = capture_output(|| {
            run_add(
                &project,
                "Bad phase task",
                None,
                &[],
                Some("nonexistent"),
                None,
                None,
            )
        });
        let error = result.unwrap_err();

        match &error {
            AddError::UnknownPhase { phase } => assert_eq!(phase, "nonexistent"),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(output, "");
        assert_eq!(error.to_string(), "unknown phase: nonexistent");
    }

    // --duties tests

    #[test]
    fn duties_without_phases_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review:Check for SQL injection".to_owned()];
        let (result, _) =
            capture_output(|| run_add(&project, "Task", None, &[], None, None, Some(&duties)));
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("--phases")),
            "unexpected error: {error}"
        );
        assert_eq!(
            error.to_string(),
            "invalid --duties: --phases is required when --duties is used"
        );
    }

    #[test]
    fn duties_phase_not_in_phases_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["other_phase:some duty".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("'other_phase'") && reason.contains("not found in --phases")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_on_existing_global_phase_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        // enriching is in the global config
        let duties = vec!["enriching:some duty".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("enriching:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("'enriching'") && reason.contains("agira phase update --set-duty")),
            "unexpected error: {error}"
        );
        assert_eq!(
            error.to_string(),
            "invalid --duties: cannot set duty on existing phase 'enriching'; use 'agira phase update --set-duty'"
        );
    }

    #[test]
    fn duties_on_mandatory_pending_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["pending:some duty".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("'pending'") && reason.contains("mandatory")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_on_mandatory_done_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["done:some duty".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("'done'") && reason.contains("mandatory")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_empty_text_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review:".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("empty duty")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_missing_separator_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("invalid format")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_duplicate_phase_returns_error() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec![
            "security_review:first duty".to_owned(),
            "security_review:second duty".to_owned(),
        ];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        let error = result.unwrap_err();

        assert!(
            matches!(&error, AddError::InvalidDuties { reason } if reason.contains("duplicate") && reason.contains("'security_review'")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duties_stored_in_state_machine() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review:Check for SQL injection vulnerabilities".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Secure task",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        result.unwrap();

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        let state_machine = task.state_machine.as_ref().unwrap();
        let review = state_machine
            .iter()
            .find(|p| p.name == "security_review")
            .unwrap();

        assert_eq!(
            review.duty.as_deref(),
            Some("Check for SQL injection vulnerabilities")
        );
    }

    #[test]
    fn duties_duty_text_may_contain_colon() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review:Run: cargo test && cargo clippy".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Task with colon duty",
                None,
                &[],
                None,
                Some("security_review:opus"),
                Some(&duties),
            )
        });
        result.unwrap();

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        let state_machine = task.state_machine.as_ref().unwrap();
        let review = state_machine
            .iter()
            .find(|p| p.name == "security_review")
            .unwrap();

        assert_eq!(
            review.duty.as_deref(),
            Some("Run: cargo test && cargo clippy")
        );
    }

    #[test]
    fn multiple_duties_all_stored() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec![
            "security_review:Check for SQL injection".to_owned(),
            "compliance_check:Verify GDPR compliance".to_owned(),
        ];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Multi-duty task",
                None,
                &[],
                None,
                Some("security_review:opus,compliance_check:haiku"),
                Some(&duties),
            )
        });
        result.unwrap();

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        let state_machine = task.state_machine.as_ref().unwrap();

        let security = state_machine
            .iter()
            .find(|p| p.name == "security_review")
            .unwrap();
        let compliance = state_machine
            .iter()
            .find(|p| p.name == "compliance_check")
            .unwrap();

        assert_eq!(security.duty.as_deref(), Some("Check for SQL injection"));
        assert_eq!(compliance.duty.as_deref(), Some("Verify GDPR compliance"));
    }

    #[test]
    fn phases_without_duties_have_none_duty() {
        let (_temp_dir, project, _config) = test_project_with_config();

        let duties = vec!["security_review:Check security".to_owned()];
        let (result, _) = capture_output(|| {
            run_add(
                &project,
                "Partial duties",
                None,
                &[],
                None,
                Some("security_review:opus,compliance_check:haiku"),
                Some(&duties),
            )
        });
        result.unwrap();

        let tasks_file = read_tasks(&project);
        let task = &tasks_file.tasks[0];
        let state_machine = task.state_machine.as_ref().unwrap();
        let compliance = state_machine
            .iter()
            .find(|p| p.name == "compliance_check")
            .unwrap();

        assert!(compliance.duty.is_none());
    }
}
