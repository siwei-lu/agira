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

#[derive(Debug, Error)]
pub enum TodoError {
    #[error("no actionable task — all remaining tasks are blocked, failed, or complete")]
    NoActionableTask,

    #[error("artifact must not be empty")]
    EmptyArtifact,

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

pub fn run_todo(project: &Project, artifact: Option<&str>) -> Result<(), TodoError> {
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

            let task_id = {
                let current_task = select_next_task(store.all_tasks(), &config)
                    .ok_or(TodoError::NoActionableTask)?;
                current_task.id.clone()
            };

            let completed_at = Utc::now().to_rfc3339();
            let from_phase = store.record_phase_artifact(&task_id, artifact, completed_at)?;
            store.next_phase(&task_id)?;

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
                &from_phase,
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
    let terminal_phase = config.terminal_phase()?;

    tasks
        .iter()
        .filter(|task| task.state == terminal_phase)
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
    use std::{fs, process::Command};

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig},
        global_config::GlobalConfig,
        tasks::TaskStore,
    };

    const PENDING_ACCEPT_PROMPT: &str =
        "Run: agira task todo --artifact \"task accepted\" to advance this task to the next phase.";

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

    fn setup_with_git_repo() -> (TempDir, TempDir, Project, Config) {
        let git_dir = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .arg(git_dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        let state_dir = TempDir::new().unwrap();
        let project = Project {
            git_root: git_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: state_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        };
        let config = test_config();
        write_config(&project, &config);
        (git_dir, state_dir, project, config)
    }

    fn capture_output<F>(run: F) -> (Result<(), TodoError>, String)
    where
        F: FnOnce() -> Result<(), TodoError>,
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
            .add_task("My task", "description", vec![], None, None)
            .unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert_eq!(output, format!("{PENDING_ACCEPT_PROMPT}\n"));
    }

    #[test]
    fn no_artifact_skips_blocked_current_task() {
        let (temp_dir, project, mut config) = setup();
        config.phases.insert(
            1,
            PhaseConfig {
                name: "blocked".to_owned(),
                model: Some("haiku".to_owned()),
                duty: None,
            },
        );
        write_config(&project, &config);
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task(
                "Blocked current task",
                "blocked description",
                vec![],
                None,
                None,
            )
            .unwrap();
        store
            .add_task(
                "Next actionable task",
                "next description",
                vec![],
                None,
                None,
            )
            .unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains(PENDING_ACCEPT_PROMPT));
        assert!(!output.contains("Blocked current task"));
    }

    #[test]
    fn no_artifact_dirty_tree_blocks_next_task_prompt() {
        let (git_dir, state_dir, project, config) = setup_with_git_repo();
        let mut store = test_store(&state_dir, &config);
        store.add_task("Done task", "", vec![], None, None).unwrap();
        store
            .add_task("Next task", "", vec!["task-001".to_owned()], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        fs::write(git_dir.path().join("dirty.txt"), "dirty").unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains("# Commit"));
        assert!(output.contains("Task task-001 \"Done task\" is complete"));
        assert!(output.contains("feat(task-001): Done task"));
        assert!(output.contains(DIRTY_WORKING_TREE_MESSAGE));
        assert!(!output.contains("# Agira Task Prompt"));
        assert!(!output.contains("Next task"));
        assert!(output.ends_with(&format!("{DIRTY_WORKING_TREE_MESSAGE}\n")));
    }

    #[test]
    fn no_artifact_clean_tree_proceeds_normally() {
        let (_git_dir, state_dir, project, config) = setup_with_git_repo();
        let mut store = test_store(&state_dir, &config);
        store
            .add_task("Clean tree task", "description", vec![], None, None)
            .unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains(PENDING_ACCEPT_PROMPT));
        assert!(!output.contains(DIRTY_WORKING_TREE_MESSAGE));
    }

    #[test]
    fn no_artifact_dispatch_model_prints_raw_prompt_without_writing_prompt_file() {
        let (temp_dir, project, mut config) = setup();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "enriching")
            .unwrap()
            .model = Some("dispatch -a codex".to_owned());
        write_config(&project, &config);
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task(
                "Dispatch task",
                "implement dispatch prompt output",
                vec![],
                None,
                None,
            )
            .unwrap();
        store.next_phase("task-001").unwrap();
        let prompt_path = std::path::Path::new("/tmp/agira-task-001.txt");
        let _ = fs::remove_file(prompt_path);

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.starts_with("# Agira Task Prompt"));
        assert!(output.contains("- Agent role: dispatch -a codex"));
        assert!(output.contains("Dispatch task"));
        assert!(output.contains("## Checkpoints"));
        assert!(!output.contains("## Advance State"));
        assert!(!output.contains("# Agira Orchestrator Instructions"));
        assert!(!output.contains("--- SUBAGENT PROMPT ---"));
        assert!(!output.contains("dispatch -a codex exec"));
        assert!(!prompt_path.exists());
    }

    #[test]
    fn no_artifact_git_failure_skips_dirty_check() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Non-git task", "description", vec![], None, None)
            .unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains(PENDING_ACCEPT_PROMPT));
        assert!(!output.contains(DIRTY_WORKING_TREE_MESSAGE));
    }

    #[test]
    fn no_artifact_all_done_shows_completion_summary() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Done task", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains("# Agira Completion Summary"));
    }

    #[test]
    fn task_prompt_references_todo_command() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", vec![], None, None).unwrap();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert_eq!(output, format!("{PENDING_ACCEPT_PROMPT}\n"));
    }

    #[test]
    fn with_artifact_non_terminal_shows_arrow() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", vec![], None, None).unwrap();

        let (result, output) = capture_output(|| run_todo(&project, Some("enriched")));

        result.unwrap();
        assert!(output.contains("task-001 → enriching"));
        assert!(!output.contains("# Commit"));
        assert!(!output.contains("# Agira Task Prompt"));
    }

    #[test]
    fn with_artifact_skips_blocked_current_task() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Blocked current task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Next actionable task", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let (result, output) = capture_output(|| run_todo(&project, Some("next artifact")));

        result.unwrap();
        assert!(output.contains("task-002 → enriching"));

        let store = test_store(&temp_dir, &config);
        let blocked_task = store.get_task("task-001").unwrap();
        assert_eq!(blocked_task.state, "blocked");
        assert!(blocked_task.phases.is_empty());

        let next_task = store.get_task("task-002").unwrap();
        assert_eq!(next_task.state, "enriching");
        assert_eq!(
            next_task.phases.get("pending").unwrap().artifact,
            "next artifact"
        );
    }

    #[test]
    fn with_artifact_terminal_shows_commit_and_next_task() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("First task", "", vec![], None, None)
            .unwrap();
        // task-002 depends on task-001 so it is blocked until task-001 is done
        store
            .add_task("Second task", "", vec!["task-001".to_owned()], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap(); // pending -> enriching
        store.next_phase("task-001").unwrap(); // enriching -> in_progress

        let (result, output) = capture_output(|| run_todo(&project, Some("implemented")));

        result.unwrap();
        assert!(output.contains("task-001 done ✓"));
        assert!(output.contains("# Commit"));
        assert!(output.contains(PENDING_ACCEPT_PROMPT));
        assert!(!output.contains("# Agira Completion Summary"));
    }

    #[test]
    fn with_artifact_terminal_all_done_shows_completion() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Only task", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) = capture_output(|| run_todo(&project, Some("implemented")));

        result.unwrap();
        assert!(output.contains("task-001 done ✓"));
        assert!(output.contains("# Commit"));
        assert!(output.contains("# Agira Completion Summary"));
    }

    #[test]
    fn with_artifact_terminal_next_dispatch_task_prints_raw_prompt_without_writing_prompt_file() {
        let (temp_dir, project, mut config) = setup();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "enriching")
            .unwrap()
            .model = Some("dispatch -a codex".to_owned());
        write_config(&project, &config);
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Finishing task", "finish this first", vec![], None, None)
            .unwrap();
        store
            .add_task(
                "Next dispatch task",
                "dispatch after terminal advance",
                vec![],
                None,
                None,
            )
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-002").unwrap();
        let prompt_path = std::path::Path::new("/tmp/agira-task-002.txt");
        let _ = fs::remove_file(prompt_path);

        let (result, output) = capture_output(|| run_todo(&project, Some("implemented")));

        result.unwrap();
        assert!(output.contains("task-001 done"));
        assert!(output.contains("# Agira Task Prompt"));
        assert!(output.contains("- Agent role: dispatch -a codex"));
        assert!(output.contains("Next dispatch task"));
        assert!(output.contains("## Checkpoints"));
        assert!(!output.contains("## Advance State"));
        assert!(!output.contains("# Agira Orchestrator Instructions"));
        assert!(!output.contains("--- SUBAGENT PROMPT ---"));
        assert!(!output.contains("dispatch -a codex exec"));
        assert!(!prompt_path.exists());
    }

    #[test]
    fn with_artifact_no_actionable_task_returns_error() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Done task", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = run_todo(&project, Some("artifact")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "no actionable task — all remaining tasks are blocked, failed, or complete"
        );
        assert!(matches!(error, TodoError::NoActionableTask));
    }

    #[test]
    fn empty_artifact_returns_error() {
        let (_temp_dir, project, _config) = setup();

        for artifact in ["", "  "] {
            let error = run_todo(&project, Some(artifact)).unwrap_err();
            assert!(matches!(error, TodoError::EmptyArtifact));
        }
    }

    #[test]
    fn with_artifact_records_in_store() {
        let (temp_dir, project, config) = setup();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("My task", "", vec![], None, None).unwrap();

        let (result, _) = capture_output(|| run_todo(&project, Some("done it")));
        result.unwrap();

        let store = test_store(&temp_dir, &config);
        let task = store.get_task("task-001").unwrap();
        assert_eq!(task.state, "enriching");
        let phase = task.phases.get("pending").unwrap();
        assert_eq!(phase.artifact, "done it");
    }

    fn project_with_all_done_hook(base: Project, sentinel_path: &std::path::Path) -> Project {
        Project {
            project_hooks: crate::core::hooks::HookConfig {
                hooks: vec![crate::core::hooks::HookEntry {
                    on: ALL_TASKS_DONE_EVENT.to_owned(),
                    run: format!("printf 'all_done' > {}", sentinel_path.display()),
                }],
            },
            ..base
        }
    }

    #[test]
    fn with_artifact_all_tasks_done_fires_all_done_hook() {
        let (temp_dir, project, config) = setup();
        let sentinel_path = temp_dir.path().join("all_done_sentinel.txt");
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Only task", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap(); // pending -> enriching
        store.next_phase("task-001").unwrap(); // enriching -> in_progress
        let project = project_with_all_done_hook(project, &sentinel_path);

        let (result, _) = capture_output(|| run_todo(&project, Some("done")));

        result.unwrap();
        assert!(sentinel_path.exists(), "all_tasks_done hook must fire");
    }

    #[test]
    fn with_artifact_pending_tasks_remain_does_not_fire_all_done_hook() {
        let (temp_dir, project, config) = setup();
        let sentinel_path = temp_dir.path().join("all_done_sentinel.txt");
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task one", "", vec![], None, None).unwrap();
        store.add_task("Task two", "", vec![], None, None).unwrap();
        store.next_phase("task-001").unwrap(); // pending -> enriching
        store.next_phase("task-001").unwrap(); // enriching -> in_progress
        let project = project_with_all_done_hook(project, &sentinel_path);

        let (result, _) = capture_output(|| run_todo(&project, Some("done")));

        result.unwrap();
        assert!(
            !sentinel_path.exists(),
            "all_tasks_done hook must not fire when tasks remain"
        );
    }

    #[test]
    fn with_artifact_failed_tasks_count_as_terminal_fires_all_done_hook() {
        let (temp_dir, project, config) = setup();
        let sentinel_path = temp_dir.path().join("all_done_sentinel.txt");
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Failed task", "", vec![], None, None)
            .unwrap();
        store.add_task("Last task", "", vec![], None, None).unwrap();
        store.fail_task("task-001", "oops").unwrap();
        store.next_phase("task-002").unwrap(); // pending -> enriching
        store.next_phase("task-002").unwrap(); // enriching -> in_progress
        let project = project_with_all_done_hook(project, &sentinel_path);

        let (result, _) = capture_output(|| run_todo(&project, Some("done")));

        result.unwrap();
        assert!(
            sentinel_path.exists(),
            "failed + done should trigger all_tasks_done hook"
        );
    }

    #[test]
    fn with_artifact_non_terminal_phase_does_not_fire_all_done_hook() {
        let (temp_dir, project, config) = setup();
        let sentinel_path = temp_dir.path().join("all_done_sentinel.txt");
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Only task", "", vec![], None, None).unwrap();
        let project = project_with_all_done_hook(project, &sentinel_path);

        // advances to enriching, not terminal
        let (result, _) = capture_output(|| run_todo(&project, Some("enriched")));

        result.unwrap();
        assert!(
            !sentinel_path.exists(),
            "non-terminal advance must not fire all_tasks_done hook"
        );
    }

    #[test]
    fn no_artifact_no_tasks_shows_no_tasks_message() {
        let (_temp_dir, project, _config) = setup();

        let (result, output) = capture_output(|| run_todo(&project, None));

        result.unwrap();
        assert!(output.contains("agira task add"));
    }
}
