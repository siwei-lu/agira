use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use thiserror::Error;

use crate::core::{
    advance::{commit_prompt, read_recent_commits},
    config::{ConfigError, load_project_config},
    hooks::{ALL_TASKS_DONE_EVENT, HookContext, dispatch_hooks, hooks_for_event, hooks_for_phase},
    pick::{format_pick_output, select_next_task},
    project::Project,
    tasks::{StoreError, Task, TaskStore, all_tasks_done},
};

const DIRTY_WORKING_TREE_MESSAGE: &str =
    "Working tree is dirty — commit your changes, then run `agira task todo` again.";

const DEPRECATION_WARNING: &str = "warning: --artifact without --task is deprecated; use --task <id> --from <phase> to target explicitly";

#[derive(Debug, Error)]
pub enum TodoError {
    #[error("no actionable task — all remaining tasks are blocked, failed, or complete")]
    NoActionableTask,

    #[error("artifact must not be empty")]
    EmptyArtifact,

    #[error("task {id} not found")]
    TaskNotFound { id: String },

    #[error("task {id} already advanced past {phase}")]
    AlreadyAdvancedPast { id: String, phase: String },

    #[error("task {id} is {state} and cannot be advanced")]
    NotAdvanceable { id: String, state: String },

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

pub fn run_todo(
    project: &Project,
    artifact: Option<&str>,
    task_id: Option<&str>,
    from_phase: Option<&str>,
) -> Result<(), TodoError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    match artifact {
        None => {
            let store = TaskStore::new(&project.state_dir, &config)?;
            if is_working_tree_dirty(&project.git_root) {
                let convention = read_recent_commits(&project.git_root);
                let (task_id, task_title) = dirty_commit_target(store.all_tasks(), &config)
                    .map(|task| (task.id.as_str(), task.title.as_str()))
                    .unwrap_or(("uncommitted", "Uncommitted changes"));

                print_todo_output(&commit_prompt(task_id, task_title, convention.as_deref()));
                print_todo_output(DIRTY_WORKING_TREE_MESSAGE);
                return Ok(());
            }

            let output = format_pick_output(&config, store.all_tasks(), &project.state_dir);
            print_todo_output(&output);
        }
        Some(artifact) => {
            if artifact.trim().is_empty() {
                return Err(TodoError::EmptyArtifact);
            }

            let terminal_phase = config
                .terminal_phase()
                .ok_or_else(|| TodoError::InvalidConfig {
                    path: config_path.clone(),
                    reason: "phases must not be empty".to_owned(),
                })?
                .to_owned();

            let mut store = TaskStore::new(&project.state_dir, &config)?;

            let resolved_task_id = if let Some(id) = task_id {
                // CAS path: locate task by explicit ID
                let task = store
                    .get_task(id)
                    .ok_or_else(|| TodoError::TaskNotFound { id: id.to_owned() })?;

                // Check the task is not blocked, failed, or terminal
                if task.state == "blocked" || task.state == "failed" || task.state == terminal_phase
                {
                    return Err(TodoError::NotAdvanceable {
                        id: id.to_owned(),
                        state: task.state.clone(),
                    });
                }

                // CAS check: if --from was also provided, verify the task is still in that phase
                if let Some(expected_phase) = from_phase {
                    if task.state != expected_phase {
                        return Err(TodoError::AlreadyAdvancedPast {
                            id: id.to_owned(),
                            phase: expected_phase.to_owned(),
                        });
                    }
                }

                id.to_owned()
            } else {
                // Legacy path: emit deprecation warning, use select_next_task
                eprintln!("{DEPRECATION_WARNING}");
                let current_task = select_next_task(store.all_tasks(), &config)
                    .ok_or(TodoError::NoActionableTask)?;
                current_task.id.clone()
            };

            let completed_at = Utc::now().to_rfc3339();
            let recorded_from_phase =
                store.record_phase_artifact(&resolved_task_id, artifact, completed_at)?;
            store.next_phase(&resolved_task_id)?;

            // shadow task_id with the resolved one for remainder of block
            let task_id = resolved_task_id;

            let resulting_task = store.get_task(&task_id).unwrap().clone();
            let resulting_state = resulting_task.state.clone();
            let hooks = hooks_for_phase(
                &project.global_hooks,
                &project.project_hooks,
                &resulting_state,
            );
            let hook_ctx = HookContext::new(
                &resulting_task,
                &project.slug,
                &project.git_root,
                &project.state_dir,
                &recorded_from_phase,
                &resulting_state,
                artifact,
            );
            dispatch_hooks(
                &hooks,
                &resulting_state,
                &hook_ctx,
                project.global_config.hook_debug,
            );

            if all_tasks_done(store.all_tasks()) {
                let all_done_hooks = hooks_for_event(
                    &project.global_hooks,
                    &project.project_hooks,
                    ALL_TASKS_DONE_EVENT,
                );
                dispatch_hooks(
                    &all_done_hooks,
                    ALL_TASKS_DONE_EVENT,
                    &hook_ctx,
                    project.global_config.hook_debug,
                );
            }

            if resulting_state == terminal_phase {
                print_todo_output(&format!("{task_id} done ✓"));
                let convention = read_recent_commits(&project.git_root);
                print_todo_output(&commit_prompt(
                    &task_id,
                    &resulting_task.title,
                    convention.as_deref(),
                ));
                let next_output =
                    format_pick_output(&config, store.all_tasks(), &project.state_dir);
                print_todo_output(&next_output);
            } else {
                print_todo_output(&format!("{task_id} → {resulting_state}"));
            }
        }
    }

    Ok(())
}

fn is_working_tree_dirty(git_root: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["status", "--porcelain"])
        .output()
    else {
        return false;
    };

    output.status.success() && !output.stdout.is_empty()
}

fn dirty_commit_target<'a>(
    tasks: &'a [Task],
    config: &crate::core::config::Config,
) -> Option<&'a Task> {
    let initial_phase = config.initial_phase()?;

    tasks
        .iter()
        .filter(|task| task.state != initial_phase)
        .max_by(|left, right| latest_history_timestamp(left).cmp(latest_history_timestamp(right)))
}

fn latest_history_timestamp(task: &Task) -> &str {
    task.history
        .last()
        .map(|entry| entry.timestamp.as_str())
        .unwrap_or(task.created_at.as_str())
}

fn map_config_error(error: ConfigError) -> TodoError {
    match error {
        ConfigError::NotFound { path } => TodoError::Io {
            path,
            source: io::Error::new(io::ErrorKind::NotFound, "config file not found"),
        },
        ConfigError::Read { path, source } => TodoError::Io { path, source },
        ConfigError::Parse { path, source } => TodoError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => TodoError::InvalidConfig { path, reason },
    }
}

fn print_todo_output(message: &str) {
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
    use std::{collections::BTreeMap, fs};

    use crate::core::{
        config::{Config, DEFAULT_WORKFLOW_NAME, PhaseDef},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
        tasks::{HistoryEntry, Task, TaskStore},
    };

    use super::{TodoError, dirty_commit_target, run_todo};

    // ---------------------------------------------------------------------------
    // Helpers for run_todo unit tests
    // ---------------------------------------------------------------------------

    fn three_phase_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        )
    }

    fn setup_project_with_config(config: &Config) -> (tempfile::TempDir, Project) {
        let home_dir = tempfile::TempDir::new().expect("create home temp dir");
        let repo_dir = tempfile::TempDir::new().expect("create repo temp dir");

        // Create a fake git root with .git directory
        let git_root = repo_dir.path().to_path_buf();
        fs::create_dir_all(git_root.join(".git")).expect("create .git dir");

        // Create the state dir and write the config
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

    fn add_task_in_phase(project: &Project, config: &Config, title: &str, phase: &str) -> String {
        let mut store = TaskStore::new(&project.state_dir, config).expect("open store");
        let task = store
            .add_task(
                title,
                "desc",
                Vec::new(),
                Some(phase),
                DEFAULT_WORKFLOW_NAME.to_owned(),
            )
            .expect("add task");
        task.id
    }

    // ---------------------------------------------------------------------------
    // CAS mismatch: --artifact + --task + --from where task is in a different phase
    // ---------------------------------------------------------------------------

    #[test]
    fn cas_mismatch_returns_already_advanced_past_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);
        let task_id = add_task_in_phase(&project, &config, "cas task", "implementing");

        let result = run_todo(
            &project,
            Some("my artifact"),
            Some(&task_id),
            Some("enriching"), // task is actually in "implementing"
        );

        match result {
            Err(TodoError::AlreadyAdvancedPast { id, phase }) => {
                assert_eq!(id, task_id);
                assert_eq!(phase, "enriching");
            }
            other => panic!("expected AlreadyAdvancedPast, got: {other:?}"),
        }

        // State must not have been mutated
        let store = TaskStore::new(&project.state_dir, &config).expect("open store");
        let task = store.get_task(&task_id).expect("task exists");
        assert_eq!(
            task.state, "implementing",
            "state must not change on CAS mismatch"
        );
        assert!(
            task.phases.is_empty(),
            "no phase artifact must be recorded on CAS mismatch"
        );
    }

    // ---------------------------------------------------------------------------
    // Targeting a blocked task returns NotAdvanceable without mutating state
    // ---------------------------------------------------------------------------

    #[test]
    fn targeting_blocked_task_returns_not_advanceable_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);
        let task_id = add_task_in_phase(&project, &config, "blocked task", "implementing");

        // Block the task
        let mut store = TaskStore::new(&project.state_dir, &config).expect("open store");
        store
            .block_task(&task_id, "needs input")
            .expect("block task");
        drop(store);

        let result = run_todo(&project, Some("my artifact"), Some(&task_id), None);

        match result {
            Err(TodoError::NotAdvanceable { id, state }) => {
                assert_eq!(id, task_id);
                assert_eq!(state, "blocked");
            }
            other => panic!("expected NotAdvanceable, got: {other:?}"),
        }

        // State must not have changed
        let store = TaskStore::new(&project.state_dir, &config).expect("open store");
        let task = store.get_task(&task_id).expect("task exists");
        assert_eq!(task.state, "blocked");
        assert!(task.phases.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Targeting a failed task returns NotAdvanceable without mutating state
    // ---------------------------------------------------------------------------

    #[test]
    fn targeting_failed_task_returns_not_advanceable_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);
        let task_id = add_task_in_phase(&project, &config, "failed task", "implementing");

        let mut store = TaskStore::new(&project.state_dir, &config).expect("open store");
        store.fail_task(&task_id, "whoops").expect("fail task");
        drop(store);

        let result = run_todo(&project, Some("my artifact"), Some(&task_id), None);

        match result {
            Err(TodoError::NotAdvanceable { id, state }) => {
                assert_eq!(id, task_id);
                assert_eq!(state, "failed");
            }
            other => panic!("expected NotAdvanceable, got: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Targeting a terminal-phase task returns NotAdvanceable
    // ---------------------------------------------------------------------------

    #[test]
    fn targeting_terminal_task_returns_not_advanceable_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);
        let task_id = add_task_in_phase(&project, &config, "terminal task", "done");

        let result = run_todo(&project, Some("my artifact"), Some(&task_id), None);

        match result {
            Err(TodoError::NotAdvanceable { id, state }) => {
                assert_eq!(id, task_id);
                assert_eq!(state, "done");
            }
            other => panic!("expected NotAdvanceable, got: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Unknown task ID returns TaskNotFound
    // ---------------------------------------------------------------------------

    #[test]
    fn unknown_task_id_returns_task_not_found_error() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);

        let result = run_todo(&project, Some("my artifact"), Some("task-999"), None);

        match result {
            Err(TodoError::TaskNotFound { id }) => {
                assert_eq!(id, "task-999");
            }
            other => panic!("expected TaskNotFound, got: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Legacy path: --artifact without --task emits deprecation warning and advances
    // ---------------------------------------------------------------------------

    #[test]
    fn legacy_path_without_task_id_advances_and_returns_ok() {
        let config = three_phase_config();
        let (_home, project) = setup_project_with_config(&config);
        let task_id = add_task_in_phase(&project, &config, "legacy task", "enriching");

        // Capture stderr by redirecting within the test is not straightforward; instead we
        // validate that the function returns Ok and the task is advanced — the deprecation
        // warning is emitted to stderr as a side-effect verified by the integration test.
        let result = run_todo(&project, Some("legacy artifact"), None, None);
        assert!(
            result.is_ok(),
            "legacy path must return Ok; got: {result:?}"
        );

        let store = TaskStore::new(&project.state_dir, &config).expect("open store");
        let task = store.get_task(&task_id).expect("task exists");
        assert_eq!(
            task.state, "implementing",
            "legacy path must advance the task"
        );
    }

    fn test_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        )
    }

    fn task_with_history(id: &str, state: &str, timestamp: &str) -> Task {
        Task {
            id: id.to_owned(),
            title: format!("{id} title"),
            description: "description".to_owned(),
            state: state.to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: BTreeMap::new(),
            history: vec![HistoryEntry {
                from: None,
                to: state.to_owned(),
                timestamp: timestamp.to_owned(),
                reason: "test".to_owned(),
            }],
            created_at: "2026-06-10T00:00:00Z".to_owned(),
            workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            locked_at: None,
        }
    }

    #[test]
    fn dirty_commit_target_prefers_latest_reviewing_task_over_done_task() {
        let config = test_config();
        let tasks = vec![
            task_with_history("task-older-done", "done", "2026-06-10T01:00:00Z"),
            task_with_history("task-newer-reviewing", "reviewing", "2026-06-10T02:00:00Z"),
        ];

        let target = dirty_commit_target(&tasks, &config).expect("select task");

        assert_eq!(target.id, "task-newer-reviewing");
    }

    #[test]
    fn dirty_commit_target_still_selects_done_task_when_it_is_latest() {
        let config = test_config();
        let tasks = vec![
            task_with_history("task-older-reviewing", "reviewing", "2026-06-10T01:00:00Z"),
            task_with_history("task-newer-done", "done", "2026-06-10T02:00:00Z"),
        ];

        let target = dirty_commit_target(&tasks, &config).expect("select task");

        assert_eq!(target.id, "task-newer-done");
    }

    #[test]
    fn dirty_commit_target_returns_none_for_pending_only_or_empty_tasks() {
        let config = test_config();
        let pending_tasks = vec![
            task_with_history("task-pending-1", "pending", "2026-06-10T01:00:00Z"),
            task_with_history("task-pending-2", "pending", "2026-06-10T02:00:00Z"),
        ];

        assert!(dirty_commit_target(&pending_tasks, &config).is_none());
        assert!(dirty_commit_target(&[], &config).is_none());
    }

    #[test]
    fn config_initial_phase_returns_first_default_workflow_phase() {
        let config = test_config();

        assert_eq!(config.initial_phase(), Some("pending"));
    }
}
