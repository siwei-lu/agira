use std::{
    cmp::Ordering,
    fmt::Write as FmtWrite,
    fs, io,
    io::BufRead,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    commands::runner::{RunnerStatusOutput, Tmux, status_runner},
    core::{
        config::{ConfigError, load_project_config},
        project::Project,
        tasks::{StoreError, Task, TaskStore},
    },
};

const NO_TASKS_MESSAGE: &str = "No tasks. Run `agira task add` to get started.";
const TITLE_LIMIT: usize = 40;
const LAST_ACTION_LIMIT: usize = 30;
const STATE_LIMIT: usize = 13;
const WORKFLOW_LIMIT: usize = 14;

#[derive(Debug, Error)]
pub enum StatusError {
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

    #[error("failed to read {path}")]
    JsonOutput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("task not found: {id}")]
    TaskNotFound { id: String },
}

/// Compute the runner status header using an injected Tmux implementation.
/// Returns the `RunnerStatusOutput` with liveness normalized to one of:
/// `live`, `idle`, `stale`, `zombie`, `unregistered`, `none`.
/// If the store cannot be opened, returns a safe "none" status so task-list
/// never hard-fails due to a runner-store error.
pub(crate) fn runner_header_with_tmux<T: Tmux>(
    project: &Project,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> RunnerStatusOutput {
    let mut status = match status_runner(project, tmux, now) {
        Ok(status) => status,
        Err(_) => {
            return RunnerStatusOutput {
                runner_id: None,
                runner_type: None,
                current_task: None,
                liveness: "none".to_owned(),
                heartbeat_age: None,
            };
        }
    };

    // Normalize status_runner's free-text liveness to the documented enum set:
    // live | idle | stale | zombie | unregistered | none
    status.liveness = match status.liveness.as_str() {
        "live" => {
            if status.current_task.is_some() {
                "live".to_owned()
            } else {
                "idle".to_owned()
            }
        }
        "stale" => "stale".to_owned(),
        "zombie" => "zombie".to_owned(),
        "no runner registered" => "none".to_owned(),
        "session running but no runner registered" => "unregistered".to_owned(),
        other => other.to_owned(),
    };
    status
}

/// Format a one-line human-readable runner header from a `RunnerStatusOutput`.
pub(crate) fn format_runner_header(status: &RunnerStatusOutput) -> String {
    let liveness = status.liveness.as_str();

    // No runner registered and no live session
    if status.runner_id.is_none() {
        return match liveness {
            "unregistered" => "runner session running but unregistered".to_owned(),
            _ => "no runner".to_owned(),
        };
    }

    let runner_id = status.runner_id.as_deref().unwrap_or("unknown");
    let runner_type = status.runner_type.as_deref().unwrap_or("unknown");
    let runner_label = format!("runner {runner_id} ({runner_type})");

    match liveness {
        "live" => {
            let heartbeat = status.heartbeat_age.as_deref().unwrap_or("unknown");
            match &status.current_task {
                Some(task_id) => {
                    format!("{task_id} held by {runner_label}, heartbeat {heartbeat}")
                }
                None => format!("{runner_label} idle, heartbeat {heartbeat}"),
            }
        }
        "idle" => {
            let heartbeat = status.heartbeat_age.as_deref().unwrap_or("unknown");
            format!("{runner_label} idle, heartbeat {heartbeat}")
        }
        // stale, zombie — both render as "stale" in the one-line header
        _ => match &status.current_task {
            Some(task_id) => format!("{task_id} held by {runner_label} — stale"),
            None => format!("{runner_label} stale"),
        },
    }
}

pub fn run_status(
    project: &Project,
    json: bool,
    filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<(), StatusError> {
    use crate::commands::runner::ProcessTmux;
    run_status_with_tmux(project, json, filter, limit, offset, &mut ProcessTmux)
}

pub(crate) fn run_status_with_tmux<T: Tmux>(
    project: &Project,
    json: bool,
    filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
    tmux: &mut T,
) -> Result<(), StatusError> {
    run_status_with_tmux_at(project, json, filter, limit, offset, tmux, Utc::now())
}

pub(crate) fn run_status_with_tmux_at<T: Tmux>(
    project: &Project,
    json: bool,
    filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> Result<(), StatusError> {
    // Warn for hook failures from the last 24 hours so stale hook problems are
    // visible without maintaining a separate last-read sentinel file.
    warn_recent_hook_failures(&project.state_dir.join("hooks.log"));

    if json {
        if let Some(id) = filter {
            return output_task_json(project, id);
        }
        return output_raw_json_with_runner(project, tmux, now);
    }

    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            if let Some(id) = filter {
                return Err(StatusError::TaskNotFound { id: id.to_owned() });
            }
            // Print runner header even when there are no tasks
            if filter.is_none() {
                let runner_status = runner_header_with_tmux(project, tmux, now);
                let header = format_runner_header(&runner_status);
                print_status_output(&format!("{header}\n"));
            }
            print_status_output(NO_TASKS_MESSAGE);
            return Ok(());
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase = config
        .terminal_phase()
        .ok_or_else(|| StatusError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?
        .to_owned();

    let store = TaskStore::new(&project.state_dir, &config)?;
    let tasks = store.all_tasks();
    if tasks.is_empty() {
        if let Some(id) = filter {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        // Print runner header even when there are no tasks
        let runner_status = runner_header_with_tmux(project, tmux, now);
        let header = format_runner_header(&runner_status);
        print_status_output(&format!("{header}\n"));
        print_status_output(NO_TASKS_MESSAGE);
        return Ok(());
    }

    if let Some(id) = filter {
        let task = tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;
        print_status_output(&format_status_table(&[task], &terminal_phase));
    } else {
        let runner_status = runner_header_with_tmux(project, tmux, now);
        let header = format_runner_header(&runner_status);

        let sorted_tasks = sort_tasks_by_id_asc(tasks);
        let total = sorted_tasks.len();
        let explicit = limit.is_some() || offset.is_some();
        let limit_eff = limit.unwrap_or(20);
        let offset_eff = if explicit {
            offset.unwrap_or(0)
        } else {
            total.saturating_sub(20)
        };
        let paginated_tasks = paginate_tasks(&sorted_tasks, limit_eff, offset_eff);
        let mut output = format_status_table(&paginated_tasks, &terminal_phase);

        if limit_eff != 0 && total > limit_eff {
            output.push('\n');
            if explicit {
                output.push_str(&format!(
                    "Showing {limit_eff} of {total} tasks. Use --offset or --limit to see more."
                ));
            } else {
                output.push_str(&format!(
                    "Showing latest {limit_eff} of {total} tasks. Use --offset or --limit to see more."
                ));
            }
        }

        print_status_output(&format!("{header}\n\n{output}"));
    }
    Ok(())
}

pub fn run_inspect(project: &Project, id: &str) -> Result<(), StatusError> {
    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let task = store
        .all_tasks()
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;

    let detail = format_task_detail(task);
    print_status_output(&detail);
    Ok(())
}

fn output_raw_json_with_runner<T: Tmux>(
    project: &Project,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> Result<(), StatusError> {
    let path = project.state_dir.join("tasks.json");

    // Read tasks array from file; if missing return {"tasks":[],"runner":{...}}
    let tasks_value: serde_json::Value = match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| StatusError::JsonOutput {
                path: path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, source),
            })?
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            serde_json::json!({ "tasks": [] })
        }
        Err(source) => {
            return Err(StatusError::JsonOutput {
                path: path.clone(),
                source,
            });
        }
    };

    let runner_status = runner_header_with_tmux(project, tmux, now);
    let runner_value = serde_json::json!({
        "runner_id": runner_status.runner_id,
        "runner_type": runner_status.runner_type,
        "current_task": runner_status.current_task,
        "liveness": runner_status.liveness,
        "heartbeat_age": runner_status.heartbeat_age,
    });

    // Build output: preserve existing tasks content, add runner at top level
    let tasks_array = tasks_value
        .get("tasks")
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![]));
    let combined = serde_json::json!({
        "tasks": tasks_array,
        "runner": runner_value,
    });

    let json_str =
        serde_json::to_string_pretty(&combined).map_err(|source| StatusError::JsonOutput {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, source),
        })?;

    write_status_output(&json_str, &path)
}

fn output_task_json(project: &Project, id: &str) -> Result<(), StatusError> {
    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let task = store
        .all_tasks()
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;
    let json_str = serde_json::to_string_pretty(task).map_err(StoreError::Serialize)?;

    write_status_output(&json_str, &tasks_path)
}

fn map_config_error(error: ConfigError) -> StatusError {
    match error {
        ConfigError::NotFound { path } => StatusError::ConfigNotFound { path },
        ConfigError::Read { path, source } => StatusError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => StatusError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => StatusError::InvalidConfig { path, reason },
    }
}

fn format_task_detail(task: &Task) -> String {
    let mut output = String::new();
    let dependencies = if task.dependencies.is_empty() {
        "—".to_owned()
    } else {
        task.dependencies.join(", ")
    };
    let blocked = format_blocked(task);
    let description = format_multiline_value(&task.description, 2);
    let workflow = task.workflow.as_str();

    writeln!(output, "ID:           {}", task.id).unwrap();
    writeln!(output, "Title:        {}", task.title).unwrap();
    writeln!(output, "State:        {}", task.state).unwrap();
    writeln!(output, "Workflow:     {workflow}").unwrap();
    writeln!(output, "Created:      {}", task.created_at).unwrap();
    writeln!(
        output,
        "Retries:      {}/{}",
        task.retry_count, task.max_retries
    )
    .unwrap();
    writeln!(output, "Depends on:   {dependencies}").unwrap();
    writeln!(output, "Blocked:      {blocked}").unwrap();
    writeln!(output, "Description:").unwrap();
    writeln!(output, "{description}").unwrap();
    writeln!(output, "Phases:").unwrap();
    if task.phases.is_empty() {
        writeln!(output, "  —").unwrap();
    } else {
        for (name, phase) in &task.phases {
            writeln!(output, "  {name} (completed {}):", phase.completed_at).unwrap();
            writeln!(output, "{}", format_multiline_value(&phase.artifact, 4)).unwrap();
        }
    }
    writeln!(output, "History:").unwrap();
    if task.history.is_empty() {
        write!(output, "  —").unwrap();
    } else {
        for entry in &task.history {
            let from = entry.from.as_deref().unwrap_or("(none)");
            writeln!(
                output,
                "  {from} → {}  {}  {}",
                entry.to, entry.timestamp, entry.reason
            )
            .unwrap();
        }
        output.pop();
    }

    output
}

fn format_blocked(task: &Task) -> String {
    match (
        task.blocked_at_phase.as_deref(),
        task.blocked_reason.as_deref(),
    ) {
        (None, None) => "—".to_owned(),
        (Some(phase), Some(reason)) => format!("{phase} — {reason}"),
        (Some(phase), None) => format!("{phase} — —"),
        (None, Some(reason)) => format!("— — {reason}"),
    }
}

fn format_multiline_value(value: &str, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    if value.is_empty() {
        return format!("{prefix}—");
    }

    value
        .lines()
        .map(|line| {
            if line.is_empty() {
                format!("{prefix}—")
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_status_table(tasks: &[&Task], terminal_phase: &str) -> String {
    let mut lines = vec![
        format!(
            "{:<10}  {:<41}  {:<15}  {:>7}  {:<15}  {}",
            "ID", "Title", "State", "Retries", "Workflow", "Last Action"
        ),
        format!(
            "{:-<10}  {:-<41}  {:-<15}  {:-<7}  {:-<15}  {:-<30}",
            "", "", "", "", "", ""
        ),
    ];

    lines.extend(
        tasks
            .iter()
            .map(|task| format_status_row(task, terminal_phase)),
    );
    lines.join("\n")
}

fn sort_tasks_by_id_asc(tasks: &[Task]) -> Vec<&Task> {
    let mut sorted_tasks: Vec<&Task> = tasks.iter().collect();
    sorted_tasks.sort_by(|left, right| compare_task_ids_asc(&left.id, &right.id));
    sorted_tasks
}

fn paginate_tasks<'a>(tasks: &[&'a Task], limit: usize, offset: usize) -> Vec<&'a Task> {
    let window = tasks.iter().skip(offset).copied();

    if limit == 0 {
        window.collect()
    } else {
        window.take(limit).collect()
    }
}

fn format_status_row(task: &Task, terminal_phase: &str) -> String {
    let title = format_title(&task.title);
    let state = format_state(&task.state, terminal_phase);
    let retries = format!("{}/{}", task.retry_count, task.max_retries);
    let workflow = truncate_chars(&task.workflow, WORKFLOW_LIMIT);
    let last_action = task
        .history
        .last()
        .map(|entry| entry.reason.as_str())
        .unwrap_or("-");
    let last_action = truncate_chars(last_action, LAST_ACTION_LIMIT);

    format!(
        "{:<10}  {:<41}  {:<15}  {:>7}  {:<15}  {}",
        task.id, title, state, retries, workflow, last_action
    )
}

fn format_title(title: &str) -> String {
    truncate_chars(title, TITLE_LIMIT)
}

fn format_state(state: &str, terminal_phase: &str) -> String {
    let display = if state == "blocked" {
        "⊘ blocked".to_owned()
    } else if state == "failed" {
        "✗ failed".to_owned()
    } else if state == terminal_phase {
        format!("✓ {state}")
    } else {
        state.to_owned()
    };

    truncate_chars(&display, STATE_LIMIT)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() > limit {
        value
            .chars()
            .take(limit)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        value.to_owned()
    }
}

fn compare_task_ids_asc(left: &str, right: &str) -> Ordering {
    match (task_id_number(left), task_id_number(right)) {
        (Some(left_number), Some(right_number)) => {
            left_number.cmp(&right_number).then_with(|| left.cmp(right))
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

fn task_id_number(id: &str) -> Option<u64> {
    id.rsplit_once('-')?.1.parse().ok()
}

fn print_status_output(message: &str) {
    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(message);
            output.push('\n');
        }
    });

    println!("{message}");
}

fn write_status_output(contents: &str, path: &Path) -> Result<(), StatusError> {
    #[cfg(test)]
    {
        let captured = OUTPUT_CAPTURE.with(|capture| {
            if let Some(output) = capture.borrow_mut().as_mut() {
                output.push_str(contents);
                true
            } else {
                false
            }
        });

        if captured {
            return Ok(());
        }
    }

    io::stdout()
        .write_all(contents.as_bytes())
        .map_err(|source| StatusError::JsonOutput {
            path: path.to_path_buf(),
            source,
        })
}

fn warn_recent_hook_failures(path: &Path) {
    if !hook_log_has_recent_failure(path, Utc::now()) {
        return;
    }

    let display_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    eprintln!(
        "warning: recent hook failures in {}",
        display_path.display()
    );
}

fn hook_log_has_recent_failure(path: &Path, now: DateTime<Utc>) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let cutoff = now - Duration::hours(24);

    for line in io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            return false;
        };
        let Some((timestamp, _)) = line.split_once('\t') else {
            continue;
        };
        let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp) else {
            continue;
        };

        if timestamp.with_timezone(&Utc) >= cutoff {
            return true;
        }
    }

    false
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod runner_header_tests {
    use std::{fs, path::Path};

    use chrono::{DateTime, Duration, Utc};

    use crate::{
        commands::runner::{RunnerCommandError, RunnerStatusOutput, Tmux},
        core::{
            config::{Config, PhaseDef},
            global_config::GlobalConfig,
            hooks::HookConfig,
            project::Project,
            runner::RunnerStore,
        },
    };

    use super::{
        OUTPUT_CAPTURE, format_runner_header, run_status_with_tmux_at, runner_header_with_tmux,
    };

    // ── Fake Tmux ──────────────────────────────────────────────────────────────

    /// A fake Tmux implementation where `live` controls `has_session` and
    /// `pane_alive_result` controls whether the pane is considered alive.
    struct FakeTmux {
        has_session: bool,
        pane_alive: bool,
    }

    impl FakeTmux {
        fn live() -> Self {
            Self {
                has_session: true,
                pane_alive: true,
            }
        }

        fn dead() -> Self {
            Self {
                has_session: false,
                pane_alive: false,
            }
        }

        fn zombie() -> Self {
            Self {
                has_session: true,
                pane_alive: false,
            }
        }
    }

    impl Tmux for FakeTmux {
        fn has_session(&mut self, _session_name: &str) -> Result<bool, RunnerCommandError> {
            Ok(self.has_session)
        }

        fn pane_alive(&mut self, _session_name: &str) -> Result<bool, RunnerCommandError> {
            Ok(self.has_session && self.pane_alive)
        }

        fn pane_process_group(
            &mut self,
            _session_name: &str,
        ) -> Result<Option<i32>, RunnerCommandError> {
            Ok(None)
        }

        fn new_session(
            &mut self,
            _session_name: &str,
            _launch_command: &str,
        ) -> Result<(), RunnerCommandError> {
            Ok(())
        }

        fn pipe_pane(
            &mut self,
            _session_name: &str,
            _log_path: &Path,
        ) -> Result<(), RunnerCommandError> {
            Ok(())
        }

        fn kill_session(&mut self, _session_name: &str) -> Result<(), RunnerCommandError> {
            Ok(())
        }

        fn kill_process_group(&mut self, _pgid: i32) -> Result<(), RunnerCommandError> {
            Ok(())
        }

        fn attach(&mut self, _session_name: &str) -> Result<(), RunnerCommandError> {
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
            .expect("parse fixed time")
            .with_timezone(&Utc)
    }

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![(
                "implementing".to_owned(),
                PhaseDef {
                    model: Some("codex".to_owned()),
                    duty: None,
                    gate: None,
                },
            )],
            3,
        )
    }

    fn make_project(state_dir: &Path) -> Project {
        Project {
            git_root: Path::new("/tmp/test-status-repo").to_path_buf(),
            slug: "test-status-repo".to_owned(),
            state_dir: state_dir.to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        }
    }

    fn setup_project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let state_dir = dir.path().join(".agira").join("test-status-repo");
        fs::create_dir_all(&state_dir).expect("state dir");
        let project = make_project(&state_dir);
        // Write config.json so TaskStore can be initialized
        fs::write(
            state_dir.join("config.json"),
            serde_json::to_string_pretty(&test_config()).expect("serialize config"),
        )
        .expect("write config.json");
        (dir, project)
    }

    fn capture_output<F>(f: F) -> String
    where
        F: FnOnce(),
    {
        OUTPUT_CAPTURE.with(|c| *c.borrow_mut() = Some(String::new()));
        f();
        OUTPUT_CAPTURE.with(|c| c.borrow_mut().take().expect("captured output"))
    }

    fn register_runner_with_task(store: &mut RunnerStore, runner_id: &str, task_id: &str) {
        store
            .register_at(
                runner_id,
                "claude-tmux",
                "agira-test-status-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(runner_id, task_id, Duration::minutes(5), fixed_now())
            .expect("acquire lease");
    }

    fn register_idle_runner(store: &mut RunnerStore, runner_id: &str) {
        store
            .register_at(
                runner_id,
                "claude-tmux",
                "agira-test-status-repo",
                fixed_now(),
            )
            .expect("register runner");
    }

    // ── format_runner_header unit tests ───────────────────────────────────────

    #[test]
    fn header_live_runner_holding_task() {
        let status = RunnerStatusOutput {
            runner_id: Some("runner-a1b2c3".to_owned()),
            runner_type: Some("claude-tmux".to_owned()),
            current_task: Some("task-003".to_owned()),
            liveness: "live".to_owned(),
            heartbeat_age: Some("12s ago".to_owned()),
        };
        assert_eq!(
            format_runner_header(&status),
            "task-003 held by runner runner-a1b2c3 (claude-tmux), heartbeat 12s ago"
        );
    }

    #[test]
    fn header_live_runner_idle_no_task() {
        let status = RunnerStatusOutput {
            runner_id: Some("runner-a1b2c3".to_owned()),
            runner_type: Some("claude-tmux".to_owned()),
            current_task: None,
            liveness: "live".to_owned(),
            heartbeat_age: Some("5s ago".to_owned()),
        };
        assert_eq!(
            format_runner_header(&status),
            "runner runner-a1b2c3 (claude-tmux) idle, heartbeat 5s ago"
        );
    }

    #[test]
    fn header_no_runner_registered_no_session() {
        let status = RunnerStatusOutput {
            runner_id: None,
            runner_type: None,
            current_task: None,
            liveness: "none".to_owned(),
            heartbeat_age: None,
        };
        assert_eq!(format_runner_header(&status), "no runner");
    }

    #[test]
    fn header_stale_dead_session_with_task() {
        let status = RunnerStatusOutput {
            runner_id: Some("runner-a1b2c3".to_owned()),
            runner_type: Some("claude-tmux".to_owned()),
            current_task: Some("task-003".to_owned()),
            liveness: "stale".to_owned(),
            heartbeat_age: Some("2h ago".to_owned()),
        };
        assert_eq!(
            format_runner_header(&status),
            "task-003 held by runner runner-a1b2c3 (claude-tmux) — stale"
        );
    }

    #[test]
    fn header_stale_dead_session_no_task() {
        let status = RunnerStatusOutput {
            runner_id: Some("runner-a1b2c3".to_owned()),
            runner_type: Some("claude-tmux".to_owned()),
            current_task: None,
            liveness: "stale".to_owned(),
            heartbeat_age: None,
        };
        assert_eq!(
            format_runner_header(&status),
            "runner runner-a1b2c3 (claude-tmux) stale"
        );
    }

    #[test]
    fn header_zombie_renders_as_stale() {
        let status = RunnerStatusOutput {
            runner_id: Some("runner-a1b2c3".to_owned()),
            runner_type: Some("claude-tmux".to_owned()),
            current_task: Some("task-005".to_owned()),
            liveness: "zombie".to_owned(),
            heartbeat_age: Some("3m ago".to_owned()),
        };
        // zombie with task → "held by ... — stale"
        let header = format_runner_header(&status);
        assert!(
            header.contains("stale"),
            "zombie should render as stale, got: {header}"
        );
        assert!(header.contains("task-005"));
    }

    #[test]
    fn header_orphaned_session_no_registry_entry() {
        let status = RunnerStatusOutput {
            runner_id: None,
            runner_type: None,
            current_task: None,
            liveness: "unregistered".to_owned(),
            heartbeat_age: None,
        };
        assert_eq!(
            format_runner_header(&status),
            "runner session running but unregistered"
        );
    }

    // ── runner_header_with_tmux integration with store ────────────────────────

    #[test]
    fn runner_header_with_tmux_live_with_task() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_runner_with_task(&mut store, "runner-abc", "task-007");
        drop(store);

        let mut tmux = FakeTmux::live();
        let status =
            runner_header_with_tmux(&project, &mut tmux, fixed_now() + Duration::seconds(12));
        assert_eq!(status.liveness, "live");
        assert_eq!(status.current_task.as_deref(), Some("task-007"));
        assert_eq!(status.heartbeat_age.as_deref(), Some("12s ago"));
    }

    #[test]
    fn runner_header_with_tmux_idle_no_task() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_idle_runner(&mut store, "runner-idle");
        drop(store);

        let mut tmux = FakeTmux::live();
        let status = runner_header_with_tmux(&project, &mut tmux, fixed_now());
        // Normalized: live without current_task → "idle"
        assert_eq!(status.liveness, "idle");
        assert!(status.current_task.is_none());
    }

    #[test]
    fn runner_header_with_tmux_no_runner() {
        let (_dir, project) = setup_project();

        let mut tmux = FakeTmux::dead();
        let status = runner_header_with_tmux(&project, &mut tmux, fixed_now());
        // Normalized: "no runner registered" → "none"
        assert_eq!(status.liveness, "none");
        assert!(status.runner_id.is_none());
    }

    #[test]
    fn runner_header_with_tmux_stale_dead_session() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_idle_runner(&mut store, "runner-dead");
        drop(store);

        let mut tmux = FakeTmux::dead();
        let status = runner_header_with_tmux(&project, &mut tmux, fixed_now());
        assert_eq!(status.liveness, "stale");
    }

    #[test]
    fn runner_header_with_tmux_stale_expired_lease() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_runner_with_task(&mut store, "runner-stale-lease", "task-010");
        drop(store);

        let mut tmux = FakeTmux::live();
        // Move time past lease expiry (lease TTL is 5 minutes)
        let status =
            runner_header_with_tmux(&project, &mut tmux, fixed_now() + Duration::minutes(10));
        assert_eq!(status.liveness, "stale");
    }

    #[test]
    fn runner_header_with_tmux_zombie() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_idle_runner(&mut store, "runner-zombie");
        drop(store);

        let mut tmux = FakeTmux::zombie();
        let status = runner_header_with_tmux(&project, &mut tmux, fixed_now());
        assert_eq!(status.liveness, "zombie");
    }

    #[test]
    fn runner_header_with_tmux_orphaned_session() {
        let (_dir, project) = setup_project();
        // No runner registered in store, but tmux says session is live
        let mut tmux = FakeTmux::live();
        let status = runner_header_with_tmux(&project, &mut tmux, fixed_now());
        // Normalized: "session running but no runner registered" → "unregistered"
        assert_eq!(
            status.liveness, "unregistered",
            "expected unregistered liveness, got: {}",
            status.liveness
        );
        assert!(status.runner_id.is_none());
    }

    // ── run_status_with_tmux: header appears in plain output ──────────────────

    #[test]
    fn task_list_plain_shows_runner_header_above_table() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_runner_with_task(&mut store, "runner-hdr", "task-001");
        drop(store);

        // Write a tasks.json with one task
        let tasks_json = serde_json::json!({
            "tasks": [{
                "id": "task-001",
                "title": "header test task",
                "description": "",
                "state": "implementing",
                "dependencies": [],
                "retry_count": 0,
                "max_retries": 3,
                "phases": {},
                "history": [],
                "created_at": "2026-06-11T12:00:00Z",
                "workflow": "rust"
            }]
        });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::live();
        let output = capture_output(|| {
            run_status_with_tmux_at(
                &project,
                false,
                None,
                None,
                None,
                &mut tmux,
                fixed_now() + Duration::seconds(12),
            )
            .expect("run_status");
        });

        // Runner header must appear before the task table header line
        let header_pos = output
            .find("runner runner-hdr")
            .expect("runner header not found in output");
        let table_header_pos = output.find("ID").expect("table header not found");
        assert!(
            header_pos < table_header_pos,
            "runner header must appear before table header\noutput:\n{output}"
        );
        // Blank line separates header from table
        assert!(
            output.contains("\n\n"),
            "expected blank line between header and table\noutput:\n{output}"
        );
    }

    #[test]
    fn task_list_plain_shows_no_runner_when_none_registered() {
        let (_dir, project) = setup_project();

        let tasks_json = serde_json::json!({
            "tasks": [{
                "id": "task-001",
                "title": "no runner task",
                "description": "",
                "state": "implementing",
                "dependencies": [],
                "retry_count": 0,
                "max_retries": 3,
                "phases": {},
                "history": [],
                "created_at": "2026-06-11T12:00:00Z",
                "workflow": "rust"
            }]
        });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::dead();
        let output = capture_output(|| {
            run_status_with_tmux_at(&project, false, None, None, None, &mut tmux, fixed_now())
                .expect("run_status");
        });

        assert!(
            output.contains("no runner"),
            "expected 'no runner' in output\noutput:\n{output}"
        );
    }

    #[test]
    fn task_list_plain_no_runner_header_when_single_task_filter() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_idle_runner(&mut store, "runner-filter-test");
        drop(store);

        let tasks_json = serde_json::json!({
            "tasks": [{
                "id": "task-001",
                "title": "filter task",
                "description": "",
                "state": "implementing",
                "dependencies": [],
                "retry_count": 0,
                "max_retries": 3,
                "phases": {},
                "history": [],
                "created_at": "2026-06-11T12:00:00Z",
                "workflow": "rust"
            }]
        });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::live();
        let output = capture_output(|| {
            run_status_with_tmux_at(
                &project,
                false,
                Some("task-001"),
                None,
                None,
                &mut tmux,
                fixed_now(),
            )
            .expect("run_status");
        });

        assert!(
            !output.contains("runner runner-filter-test"),
            "runner header must NOT appear when single-task filter is supplied\noutput:\n{output}"
        );
    }

    #[test]
    fn task_list_plain_shows_runner_header_above_no_tasks_message() {
        let (_dir, project) = setup_project();
        // No tasks.json — empty project

        let mut tmux = FakeTmux::dead();
        let output = capture_output(|| {
            run_status_with_tmux_at(&project, false, None, None, None, &mut tmux, fixed_now())
                .expect("run_status");
        });

        assert!(
            output.contains("no runner"),
            "runner header expected above No tasks message\noutput:\n{output}"
        );
        assert!(
            output.contains("No tasks"),
            "No tasks message must be present\noutput:\n{output}"
        );
        let runner_pos = output.find("no runner").expect("no runner header");
        let tasks_pos = output.find("No tasks").expect("No tasks message");
        assert!(
            runner_pos < tasks_pos,
            "runner header must appear before No tasks message"
        );
    }

    // ── run_status_with_tmux: --json output includes runner object ─────────────

    #[test]
    fn task_list_json_includes_runner_object() {
        let (_dir, project) = setup_project();
        let mut store = RunnerStore::new(&project.state_dir).expect("store");
        register_runner_with_task(&mut store, "runner-json", "task-003");
        drop(store);

        let tasks_json = serde_json::json!({ "tasks": [] });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::live();
        let output = capture_output(|| {
            run_status_with_tmux_at(
                &project,
                true,
                None,
                None,
                None,
                &mut tmux,
                fixed_now() + Duration::seconds(5),
            )
            .expect("run_status");
        });

        let value: serde_json::Value = serde_json::from_str(&output).expect("output is valid JSON");
        assert!(
            value.get("tasks").is_some(),
            "JSON must contain 'tasks' key"
        );
        let runner_obj = value.get("runner").expect("JSON must contain 'runner' key");
        assert!(runner_obj.get("runner_id").is_some(), "runner_id missing");
        assert!(
            runner_obj.get("runner_type").is_some(),
            "runner_type missing"
        );
        assert!(
            runner_obj.get("current_task").is_some(),
            "current_task missing"
        );
        assert!(runner_obj.get("liveness").is_some(), "liveness missing");
        assert!(
            runner_obj.get("heartbeat_age").is_some(),
            "heartbeat_age missing"
        );
        assert_eq!(runner_obj["liveness"], "live");
        assert_eq!(runner_obj["current_task"], "task-003");
    }

    #[test]
    fn task_list_json_runner_none_when_no_runner_registered() {
        let (_dir, project) = setup_project();

        let tasks_json = serde_json::json!({ "tasks": [] });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::dead();
        let output = capture_output(|| {
            run_status_with_tmux_at(&project, true, None, None, None, &mut tmux, fixed_now())
                .expect("run_status");
        });

        let value: serde_json::Value = serde_json::from_str(&output).expect("output is valid JSON");
        let runner_obj = value.get("runner").expect("runner key must exist");
        // Normalized liveness for no-runner case is "none"
        assert_eq!(runner_obj["liveness"], "none");
        assert!(runner_obj["runner_id"].is_null());
        assert!(runner_obj["runner_type"].is_null());
        assert!(runner_obj["current_task"].is_null());
        assert!(runner_obj["heartbeat_age"].is_null());
    }

    #[test]
    fn task_list_json_no_runner_object_when_single_task_filter() {
        let (_dir, project) = setup_project();

        let tasks_json = serde_json::json!({
            "tasks": [{
                "id": "task-001",
                "title": "json filter task",
                "description": "",
                "state": "implementing",
                "dependencies": [],
                "retry_count": 0,
                "max_retries": 3,
                "phases": {},
                "history": [],
                "created_at": "2026-06-11T12:00:00Z",
                "workflow": "rust"
            }]
        });
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks_json).unwrap(),
        )
        .expect("write tasks.json");

        let mut tmux = FakeTmux::live();
        let output = capture_output(|| {
            run_status_with_tmux_at(
                &project,
                true,
                Some("task-001"),
                None,
                None,
                &mut tmux,
                fixed_now(),
            )
            .expect("run_status");
        });

        let value: serde_json::Value = serde_json::from_str(&output).expect("output is valid JSON");
        // Single-task filter → returns the task object directly, no 'runner' key
        assert!(
            value.get("runner").is_none(),
            "runner key must NOT be present for single-task --json\noutput:\n{output}"
        );
    }
}
