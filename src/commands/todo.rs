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
    use std::collections::BTreeMap;

    use crate::core::{
        config::{Config, DEFAULT_WORKFLOW_NAME, PhaseDef},
        tasks::{HistoryEntry, Task},
    };

    use super::dirty_commit_target;

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
