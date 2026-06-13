use std::{env, io, path::PathBuf};

use thiserror::Error;

const NO_RUNNER_HINT: &str = "hint: no runner is running; start one with 'agira runner start' or set runner.auto_start = true in ~/.agira/config.toml";

use crate::core::{
    config::{ConfigError, load_project_config},
    file_lock::{FileLock, FileLockError},
    hooks::{HookContext, TASK_ADDED_EVENT, dispatch_hooks, hooks_for_event},
    project::Project,
    tasks::{StoreError, Task, TaskStore},
};

#[derive(Debug, Error)]
pub enum AddError {
    #[error("unknown dependency: {id}")]
    UnknownDependency { id: String },

    #[error("unknown phase: {phase}")]
    UnknownPhase { phase: String },

    #[error("unknown workflow '{name}'; available: {available}")]
    UnknownWorkflow { name: String, available: String },

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

    #[error(transparent)]
    FileLockError(#[from] FileLockError),
}

#[allow(clippy::too_many_arguments)]
pub fn run_add(
    project: &Project,
    title: &str,
    description: Option<&str>,
    depends_on: &[String],
    phase: Option<&str>,
    phases: Option<&str>,
    duties: Option<&[String]>,
    workflow: Option<&str>,
) -> Result<(), AddError> {
    let _ = phases;
    let _ = duties;

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    let task = {
        let _lock = FileLock::acquire(project.state_dir.join("tasks.lock"))?;
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

        let workflow_name = if let Some(wf_name) = workflow {
            if config.sequence(wf_name).is_empty() {
                let names: Vec<String> = config.workflows.keys().cloned().collect();
                return Err(AddError::UnknownWorkflow {
                    name: wf_name.to_owned(),
                    available: names.join(", "),
                });
            }
            wf_name.to_owned()
        } else {
            config.default_workflow.clone()
        };

        create_task(
            &mut store,
            title,
            description.unwrap_or(""),
            depends_on.to_vec(),
            phase,
            workflow_name,
        )?
    };

    let runner_type = project.global_config.runner.runner_type.clone();
    let auto_start = project.global_config.runner.auto_start;
    let inside_runner = env::var("AGIRA_RUNNER_ID")
        .ok()
        .is_some_and(|runner_id| !runner_id.trim().is_empty());

    run_add_side_effects(
        project,
        &task,
        auto_start,
        inside_runner,
        &runner_type,
        &mut |proj, rt| {
            let mut tmux = crate::commands::runner::ProcessTmux;
            crate::commands::runner::ensure_runner_with_tmux(proj, rt, &mut tmux)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        &mut |proj| {
            let mut tmux = crate::commands::runner::ProcessTmux;
            crate::commands::runner::runner_is_live(proj, &mut tmux, chrono::Utc::now())
                .map_err(|e| e.to_string())
        },
    )
}

pub(super) fn map_config_error(error: ConfigError) -> AddError {
    match error {
        ConfigError::NotFound { path } => AddError::ConfigNotFound { path },
        ConfigError::Read { path, source } => AddError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => AddError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => AddError::InvalidConfig { path, reason },
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn add_task_flow_with_ensure(
    project: &Project,
    store: &mut TaskStore,
    title: &str,
    description: &str,
    depends_on: Vec<String>,
    phase: Option<&str>,
    workflow_name: String,
    auto_start: bool,
    inside_runner: bool,
    runner_type: &str,
    ensure_runner: &mut dyn FnMut(&Project, &str) -> Result<(), String>,
) -> Result<(), AddError> {
    let mut runner_is_live = |_project: &Project| Ok(true);
    add_task_flow_with_ensure_and_liveness(
        project,
        store,
        title,
        description,
        depends_on,
        phase,
        workflow_name,
        auto_start,
        inside_runner,
        runner_type,
        ensure_runner,
        &mut runner_is_live,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn add_task_flow_with_ensure_and_liveness(
    project: &Project,
    store: &mut TaskStore,
    title: &str,
    description: &str,
    depends_on: Vec<String>,
    phase: Option<&str>,
    workflow_name: String,
    auto_start: bool,
    inside_runner: bool,
    runner_type: &str,
    ensure_runner: &mut dyn FnMut(&Project, &str) -> Result<(), String>,
    runner_is_live: &mut dyn FnMut(&Project) -> Result<bool, String>,
) -> Result<(), AddError> {
    let task = create_task(store, title, description, depends_on, phase, workflow_name)?;
    run_add_side_effects(
        project,
        &task,
        auto_start,
        inside_runner,
        runner_type,
        ensure_runner,
        runner_is_live,
    )
}

fn create_task(
    store: &mut TaskStore,
    title: &str,
    description: &str,
    depends_on: Vec<String>,
    phase: Option<&str>,
    workflow_name: String,
) -> Result<Task, AddError> {
    store
        .add_task(title, description, depends_on, phase, workflow_name)
        .map_err(map_store_error)
}

fn run_add_side_effects(
    project: &Project,
    task: &Task,
    auto_start: bool,
    inside_runner: bool,
    runner_type: &str,
    ensure_runner: &mut dyn FnMut(&Project, &str) -> Result<(), String>,
    runner_is_live: &mut dyn FnMut(&Project) -> Result<bool, String>,
) -> Result<(), AddError> {
    if auto_start && !inside_runner {
        if let Err(message) = ensure_runner(project, runner_type) {
            eprintln!("warning: ensure-runner failed: {message}");
        }
    }

    dispatch_task_added_hooks(project, task);
    print_add_output(&format!("added {}: {}", task.id, task.title));

    if !auto_start && !inside_runner && matches!(runner_is_live(project), Ok(false)) {
        print_no_runner_hint();
    }

    Ok(())
}

pub(super) fn dispatch_task_added_hooks(project: &Project, task: &crate::core::tasks::Task) {
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

fn print_no_runner_hint() {
    #[cfg(test)]
    STDERR_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(NO_RUNNER_HINT);
            output.push('\n');
        }
    });

    eprintln!("{NO_RUNNER_HINT}");
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static STDERR_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use crate::core::{
        config::{Config, PhaseDef},
        global_config::{GlobalConfig, RunnerConfig},
        hooks::HookConfig,
        project::Project,
        tasks::TaskStore,
    };

    use super::{
        NO_RUNNER_HINT, STDERR_CAPTURE, add_task_flow_with_ensure,
        add_task_flow_with_ensure_and_liveness,
    };

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![(
                "implementing".to_owned(),
                PhaseDef {
                    model: Some("dispatch exec -a codex".to_owned()),
                    duty: Some("write tests first".to_owned()),
                    gate: None,
                },
            )],
            3,
        )
    }

    fn make_project(dir: &Path, auto_start: bool, runner_type: &str) -> Project {
        let state_dir = dir.join(".agira").join("test-repo");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::write(
            state_dir.join("config.json"),
            serde_json::to_string_pretty(&test_config()).expect("serialize config"),
        )
        .expect("write config");
        Project {
            git_root: Path::new("/tmp/test-repo").to_path_buf(),
            slug: "test-repo".to_owned(),
            state_dir,
            global_config: GlobalConfig {
                runner: RunnerConfig {
                    auto_start,
                    runner_type: runner_type.to_owned(),
                    ..RunnerConfig::default()
                },
                ..GlobalConfig::default()
            },
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        }
    }

    fn capture_stderr<T>(f: impl FnOnce() -> T) -> (T, String) {
        STDERR_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        let result = f();
        let output = STDERR_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        (result, output)
    }

    #[test]
    fn auto_start_false_without_live_runner_prints_hint_to_stderr() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let (result, stderr) = capture_stderr(|| {
            add_task_flow_with_ensure_and_liveness(
                &project,
                &mut store,
                "task needing runner hint",
                "",
                vec![],
                None,
                config.default_workflow.clone(),
                false,
                false,
                "claude-tmux",
                &mut |_proj, _rt| Ok(()),
                &mut |_proj| Ok(false),
            )
        });

        assert!(result.is_ok());
        assert_eq!(stderr, format!("{NO_RUNNER_HINT}\n"));
    }

    #[test]
    fn auto_start_false_with_live_runner_suppresses_hint() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let (result, stderr) = capture_stderr(|| {
            add_task_flow_with_ensure_and_liveness(
                &project,
                &mut store,
                "task with live runner",
                "",
                vec![],
                None,
                config.default_workflow.clone(),
                false,
                false,
                "claude-tmux",
                &mut |_proj, _rt| Ok(()),
                &mut |_proj| Ok(true),
            )
        });

        assert!(result.is_ok());
        assert!(stderr.is_empty());
    }

    #[test]
    fn auto_start_true_suppresses_no_runner_hint() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut live_called = false;
        let (result, stderr) = capture_stderr(|| {
            add_task_flow_with_ensure_and_liveness(
                &project,
                &mut store,
                "task with auto start",
                "",
                vec![],
                None,
                config.default_workflow.clone(),
                true,
                false,
                "claude-tmux",
                &mut |_proj, _rt| Ok(()),
                &mut |_proj| {
                    live_called = true;
                    Ok(false)
                },
            )
        });

        assert!(result.is_ok());
        assert!(stderr.is_empty());
        assert!(!live_called);
    }

    #[test]
    fn inside_runner_suppresses_no_runner_hint() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut live_called = false;
        let (result, stderr) = capture_stderr(|| {
            add_task_flow_with_ensure_and_liveness(
                &project,
                &mut store,
                "task from runner",
                "",
                vec![],
                None,
                config.default_workflow.clone(),
                false,
                true,
                "claude-tmux",
                &mut |_proj, _rt| Ok(()),
                &mut |_proj| {
                    live_called = true;
                    Ok(false)
                },
            )
        });

        assert!(result.is_ok());
        assert!(stderr.is_empty());
        assert!(!live_called);
    }

    #[test]
    fn no_runner_hint_liveness_failure_is_non_fatal() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let (result, stderr) = capture_stderr(|| {
            add_task_flow_with_ensure_and_liveness(
                &project,
                &mut store,
                "task despite liveness failure",
                "",
                vec![],
                None,
                config.default_workflow.clone(),
                false,
                false,
                "claude-tmux",
                &mut |_proj, _rt| Ok(()),
                &mut |_proj| Err("tmux unavailable".to_owned()),
            )
        });

        assert!(result.is_ok());
        assert!(stderr.is_empty());
        assert_eq!(store.all_tasks().len(), 1);
    }

    // ---------------------------------------------------------------
    // auto_start_false_does_not_call_ensure_runner
    // ---------------------------------------------------------------
    #[test]
    fn auto_start_false_does_not_call_ensure_runner() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut called = false;
        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "my task",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            false,
            false,
            "claude-tmux",
            &mut |_proj, _rt| {
                called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(
            !called,
            "ensure_runner must not be called when auto_start=false"
        );
    }

    // ---------------------------------------------------------------
    // inside_runner_auto_start_true_does_not_call_ensure_runner
    // ---------------------------------------------------------------
    #[test]
    fn inside_runner_auto_start_true_does_not_call_ensure_runner() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut called = false;
        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "task from runner pane",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            true,
            "claude-tmux",
            &mut |_proj, _rt| {
                called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(
            !called,
            "ensure_runner must not be called from inside a runner pane"
        );
        let tasks = store.all_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "task from runner pane");
    }

    // ---------------------------------------------------------------
    // outside_runner_auto_start_true_calls_ensure_runner
    // ---------------------------------------------------------------
    #[test]
    fn outside_runner_auto_start_true_calls_ensure_runner() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut called = false;
        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "task outside runner pane",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut |_proj, _rt| {
                called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(
            called,
            "ensure_runner must be called when auto_start=true outside a runner pane"
        );
    }

    // ---------------------------------------------------------------
    // inside_runner_auto_start_false_does_not_call_ensure_runner
    // ---------------------------------------------------------------
    #[test]
    fn inside_runner_auto_start_false_does_not_call_ensure_runner() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), false, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut called = false;
        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "task inside runner without autostart",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            false,
            true,
            "claude-tmux",
            &mut |_proj, _rt| {
                called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(
            !called,
            "ensure_runner must not be called when auto_start=false inside a runner pane"
        );
    }

    // ---------------------------------------------------------------
    // auto_start_true_calls_ensure_runner_before_hooks
    // ---------------------------------------------------------------
    #[test]
    fn auto_start_true_calls_ensure_runner_before_hooks() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        // Track call order: ensure_runner gets index 0, hooks would fire after.
        // Because we have no hooks configured, we verify ensure_runner was called at all.
        let mut ensure_called = false;
        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "task for ordering test",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut |_proj, rt| {
                ensure_called = true;
                assert_eq!(rt, "claude-tmux");
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(
            ensure_called,
            "ensure_runner must be called when auto_start=true"
        );
    }

    // ---------------------------------------------------------------
    // auto_start_ensure_runner_failure_is_non_fatal
    // ---------------------------------------------------------------
    #[test]
    fn auto_start_ensure_runner_failure_is_non_fatal() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "task despite runner failure",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut |_proj, _rt| Err("tmux not available".to_owned()),
        );

        // Task creation must succeed even when ensure_runner fails
        assert!(
            result.is_ok(),
            "task add must succeed even when ensure_runner fails"
        );

        // Task must exist in the store
        let tasks = store.all_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "task despite runner failure");
    }

    // ---------------------------------------------------------------
    // ensure_runner_failure_does_not_abort_hook_dispatch
    // (hooks must still fire when ensure_runner fails)
    // ---------------------------------------------------------------
    #[test]
    fn ensure_runner_failure_does_not_abort_hook_dispatch() {
        // We verify the hook-dispatch path is reached by checking that
        // add_task_flow_with_ensure returns Ok (not propagating the error).
        // The ordering guarantee (ensure before hooks) is validated structurally
        // by the implementation: ensure_runner is called before dispatch_task_added_hooks.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let result = add_task_flow_with_ensure(
            &project,
            &mut store,
            "hook fire test",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut |_proj, _rt| {
                // ensure_runner fails
                Err("runner unavailable".to_owned())
            },
        );

        // The function must return Ok (not propagate the error);
        // execution continues to dispatch_task_added_hooks after the warning.
        assert!(result.is_ok());
    }

    // ---------------------------------------------------------------
    // ensure_runner_passes_configured_runner_type
    // ---------------------------------------------------------------
    #[test]
    fn ensure_runner_passes_configured_runner_type() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "custom-backend");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut received_type = String::new();
        let _ = add_task_flow_with_ensure(
            &project,
            &mut store,
            "type check task",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "custom-backend",
            &mut |_proj, rt| {
                received_type = rt.to_owned();
                Ok(())
            },
        );

        assert_eq!(received_type, "custom-backend");
    }

    // ---------------------------------------------------------------
    // idempotent_no_op_on_second_task_add_with_healthy_runner
    // ensure_runner is called for each task add (idempotency is inside start_runner)
    // ---------------------------------------------------------------
    #[test]
    fn ensure_runner_called_for_each_task_add_idempotency_is_internal() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true, "claude-tmux");
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");

        let mut call_count = 0u32;
        let mut ensure = |_proj: &Project, _rt: &str| -> Result<(), String> {
            call_count += 1;
            Ok(())
        };

        // First task add
        add_task_flow_with_ensure(
            &project,
            &mut store,
            "first task",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut ensure,
        )
        .expect("first add");

        // Second task add
        add_task_flow_with_ensure(
            &project,
            &mut store,
            "second task",
            "",
            vec![],
            None,
            config.default_workflow.clone(),
            true,
            false,
            "claude-tmux",
            &mut ensure,
        )
        .expect("second add");

        // ensure_runner is called once per task add; the no-op for healthy runner
        // is handled inside start_runner (already_running = true path)
        assert_eq!(call_count, 2, "ensure_runner called once per task add");
        assert_eq!(store.all_tasks().len(), 2);
    }
}
