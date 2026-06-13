use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::tasks::Task;

pub const TASK_ADDED_EVENT: &str = "task_added";
pub const ALL_TASKS_DONE_EVENT: &str = "all_tasks_done";
pub const BLOCKED_EVENT: &str = "blocked";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    pub on: String,
    pub run: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

pub struct HookContext {
    pub task_id: String,
    pub task_title: String,
    pub task_description: String,
    pub task_state: String,
    pub task_dependencies: String,
    pub task_retry_count: String,
    pub task_max_retries: String,
    pub task_created_at: String,
    pub project_slug: String,
    pub project_path: PathBuf,
    pub state_dir: PathBuf,
    pub from_phase: String,
    pub to_phase: String,
    pub artifact: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookDebugEntry {
    pub spawned_at: String,
    pub event: String,
    pub task_id: String,
    pub command: String,
    pub spawn_result: String,
    pub pid: Option<u32>,
    pub exit_status: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl HookContext {
    pub fn new(
        task: &Task,
        project_slug: &str,
        project_path: &Path,
        state_dir: &Path,
        from_phase: &str,
        to_phase: &str,
        artifact: &str,
    ) -> Self {
        Self {
            task_id: task.id.clone(),
            task_title: task.title.clone(),
            task_description: task.description.clone(),
            task_state: task.state.clone(),
            task_dependencies: task.dependencies.join(","),
            task_retry_count: task.retry_count.to_string(),
            task_max_retries: task.max_retries.to_string(),
            task_created_at: task.created_at.clone(),
            project_slug: project_slug.to_owned(),
            project_path: project_path.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            from_phase: from_phase.to_owned(),
            to_phase: to_phase.to_owned(),
            artifact: artifact.to_owned(),
        }
    }
}

#[derive(Debug, Error)]
pub enum HookConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid hooks config at {path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("failed to serialize hooks config for {path}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn load_hooks(path: &Path) -> Result<HookConfig, HookConfigError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(HookConfig::default());
        }
        Err(source) => {
            return Err(HookConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    toml::from_str::<HookConfig>(&contents).map_err(|error| HookConfigError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn save_hooks(path: &Path, config: &HookConfig) -> Result<(), HookConfigError> {
    let contents = toml::to_string_pretty(config).map_err(|source| HookConfigError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;

    write_hooks_file(path, contents)
}

pub fn save_hooks_preserving_toml(path: &Path, config: &HookConfig) -> Result<(), HookConfigError> {
    let mut document = match fs::read_to_string(path) {
        Ok(contents) => {
            toml::from_str::<toml::Table>(&contents).map_err(|error| HookConfigError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => toml::Table::new(),
        Err(source) => {
            return Err(HookConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    if config.hooks.is_empty() {
        document.remove("hooks");
    } else {
        document.insert("hooks".to_owned(), hooks_value(config));
    }

    let contents =
        toml::to_string_pretty(&document).map_err(|source| HookConfigError::Serialize {
            path: path.to_path_buf(),
            source,
        })?;

    write_hooks_file(path, contents)
}

fn hooks_value(config: &HookConfig) -> toml::Value {
    toml::Value::Array(
        config
            .hooks
            .iter()
            .map(|hook| {
                let mut hook_table = toml::Table::new();
                hook_table.insert("on".to_owned(), toml::Value::String(hook.on.clone()));
                hook_table.insert("run".to_owned(), toml::Value::String(hook.run.clone()));
                toml::Value::Table(hook_table)
            })
            .collect(),
    )
}

fn write_hooks_file(path: &Path, contents: String) -> Result<(), HookConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| HookConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let temporary_path = path.with_extension("toml.tmp");

    fs::write(&temporary_path, contents)
        .and_then(|_| fs::rename(&temporary_path, path))
        .map_err(|source| HookConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(any())]
fn collect_hooks(
    agira_root: &Path,
    project_slug: &str,
    event: &str,
) -> Result<Vec<HookEntry>, HookConfigError> {
    let global_hooks = load_hooks(&agira_root.join("config.toml"))?;
    let project_hooks = load_hooks(&agira_root.join(project_slug).join("hooks.toml"))?;

    Ok(hooks_for_event(&global_hooks, &project_hooks, event))
}

pub fn hooks_for_event(
    global_hooks: &HookConfig,
    project_hooks: &HookConfig,
    event: &str,
) -> Vec<HookEntry> {
    matching_hooks(global_hooks, event)
        .chain(matching_hooks(project_hooks, event))
        .cloned()
        .collect()
}

pub fn hooks_for_phase(
    global_hooks: &HookConfig,
    project_hooks: &HookConfig,
    to_phase: &str,
) -> Vec<HookEntry> {
    hooks_for_event(global_hooks, project_hooks, to_phase)
}

fn hook_debug_enabled(hook_debug: bool) -> bool {
    hook_debug || matches!(std::env::var("AGIRA_HOOK_DEBUG").as_deref(), Ok("1"))
}

pub fn dispatch_hooks(hooks: &[HookEntry], event: &str, ctx: &HookContext, hook_debug: bool) {
    let debug_log = hook_debug_enabled(hook_debug).then(|| ctx.state_dir.join("hook-debug.log"));
    let failure_log = ctx.state_dir.join("hooks.log");

    if let (true, Some(path)) = (hooks.is_empty(), debug_log.as_deref()) {
        append_hook_debug_entry(
            path,
            &HookDebugEntry {
                spawned_at: Utc::now().to_rfc3339(),
                event: event.to_owned(),
                task_id: ctx.task_id.clone(),
                command: String::new(),
                spawn_result: "no_matching_hooks".to_owned(),
                pid: None,
                exit_status: None,
                stdout: None,
                stderr: None,
            },
        );
    }

    for hook in hooks {
        match debug_log.as_deref() {
            Some(path) => dispatch_hook_with_debug(hook, event, ctx, path, &failure_log),
            None => dispatch_hook(hook, event, ctx, &failure_log),
        }
    }
}

fn dispatch_hook(hook: &HookEntry, event: &str, ctx: &HookContext, failure_log: &Path) {
    let mut command = hook_command(hook, ctx);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Block until the hook child exits. stdio is null so it cannot pollute
    // agira's own output. On spawn failure, warn but do not propagate.
    match command.spawn() {
        Ok(mut child) => {
            if let Ok(status) = child.wait() {
                if !status.success() {
                    append_hook_failure_exit_status(
                        failure_log,
                        event,
                        &ctx.task_id,
                        hook,
                        status.code(),
                    );
                }
            }
        }
        Err(error) => {
            eprintln!("warning: hook spawn failed: {error}");
            append_hook_failure_spawn(failure_log, event, &ctx.task_id, hook, &error);
        }
    }
}

fn dispatch_hook_with_debug(
    hook: &HookEntry,
    event: &str,
    ctx: &HookContext,
    log_path: &Path,
    failure_log: &Path,
) {
    let mut command = hook_command(hook, ctx);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    match command.spawn() {
        Ok(child) => {
            let pid = Some(child.id());
            append_hook_debug_entry(
                log_path,
                &HookDebugEntry {
                    spawned_at: Utc::now().to_rfc3339(),
                    event: hook.on.clone(),
                    task_id: ctx.task_id.clone(),
                    command: hook.run.clone(),
                    spawn_result: "spawned".to_owned(),
                    pid,
                    exit_status: None,
                    stdout: None,
                    stderr: None,
                },
            );

            let entry = match child.wait_with_output() {
                Ok(output) => {
                    if !output.status.success() {
                        append_hook_failure_exit_status(
                            failure_log,
                            event,
                            &ctx.task_id,
                            hook,
                            output.status.code(),
                        );
                    }

                    HookDebugEntry {
                        spawned_at: Utc::now().to_rfc3339(),
                        event: hook.on.clone(),
                        task_id: ctx.task_id.clone(),
                        command: hook.run.clone(),
                        spawn_result: "ok".to_owned(),
                        pid,
                        exit_status: output.status.code(),
                        stdout: Some(String::from_utf8_lossy(&output.stdout).into_owned()),
                        stderr: Some(String::from_utf8_lossy(&output.stderr).into_owned()),
                    }
                }
                Err(error) => {
                    eprintln!("warning: hook wait failed: {error}");
                    HookDebugEntry {
                        spawned_at: Utc::now().to_rfc3339(),
                        event: hook.on.clone(),
                        task_id: ctx.task_id.clone(),
                        command: hook.run.clone(),
                        spawn_result: format!("error:wait failed: {error}"),
                        pid,
                        exit_status: None,
                        stdout: None,
                        stderr: None,
                    }
                }
            };

            append_hook_debug_entry(log_path, &entry);
        }
        Err(error) => {
            eprintln!("warning: hook spawn failed: {error}");
            append_hook_failure_spawn(failure_log, event, &ctx.task_id, hook, &error);
            append_hook_debug_entry(
                log_path,
                &HookDebugEntry {
                    spawned_at: Utc::now().to_rfc3339(),
                    event: hook.on.clone(),
                    task_id: ctx.task_id.clone(),
                    command: hook.run.clone(),
                    spawn_result: format!("error:{error}"),
                    pid: None,
                    exit_status: None,
                    stdout: None,
                    stderr: None,
                },
            );
        }
    }
}

fn hook_command(hook: &HookEntry, ctx: &HookContext) -> Command {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&hook.run)
        .env("AGIRA_TASK_ID", &ctx.task_id)
        .env("AGIRA_TASK_TITLE", &ctx.task_title)
        .env("AGIRA_TASK_DESCRIPTION", &ctx.task_description)
        .env("AGIRA_TASK_STATE", &ctx.task_state)
        .env("AGIRA_TASK_DEPENDENCIES", &ctx.task_dependencies)
        .env("AGIRA_TASK_RETRY_COUNT", &ctx.task_retry_count)
        .env("AGIRA_TASK_MAX_RETRIES", &ctx.task_max_retries)
        .env("AGIRA_TASK_CREATED_AT", &ctx.task_created_at)
        .env("AGIRA_PROJECT_SLUG", &ctx.project_slug)
        .env("AGIRA_PROJECT_PATH", &ctx.project_path)
        .env("AGIRA_FROM_PHASE", &ctx.from_phase)
        .env("AGIRA_TO_PHASE", &ctx.to_phase)
        .env("AGIRA_ARTIFACT", &ctx.artifact);
    command
}

fn append_hook_failure_spawn(
    path: &Path,
    event: &str,
    task_id: &str,
    hook: &HookEntry,
    error: &io::Error,
) {
    append_hook_failure_line(
        path,
        event,
        task_id,
        &hook.run,
        &format!("spawn_error: {}", sanitize_field(&error.to_string(), 500)),
    );
}

fn append_hook_failure_exit_status(
    path: &Path,
    event: &str,
    task_id: &str,
    hook: &HookEntry,
    code: Option<i32>,
) {
    let status = code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    append_hook_failure_line(
        path,
        event,
        task_id,
        &hook.run,
        &format!("exit_status: {status}"),
    );
}

fn append_hook_failure_line(path: &Path, event: &str, task_id: &str, command: &str, reason: &str) {
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{}\t{}\t{}\t{}\t{}",
            Utc::now().to_rfc3339(),
            sanitize_field(event, usize::MAX),
            sanitize_field(task_id, usize::MAX),
            sanitize_field(command, 200),
            reason
        );
    }
}

fn append_hook_debug_entry(path: &Path, entry: &HookDebugEntry) {
    let contents = match serde_json::to_string(entry) {
        Ok(contents) => contents,
        Err(_) => return,
    };

    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{contents}");
    }
}

fn sanitize_field(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\t' | '\n' | '\r' => ' ',
            ch => ch,
        })
        .take(limit)
        .collect()
}

fn matching_hooks<'a>(
    config: &'a HookConfig,
    event: &'a str,
) -> impl Iterator<Item = &'a HookEntry> {
    config
        .hooks
        .iter()
        .filter(move |hook| hook.on == "*" || hook.on == event)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path, sync::Mutex};

    use chrono::DateTime;
    use tempfile::TempDir;

    use super::{HookContext, HookEntry, dispatch_hooks};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn context(state_dir: &Path) -> HookContext {
        HookContext {
            task_id: "task-001".to_owned(),
            task_title: "hook task".to_owned(),
            task_description: String::new(),
            task_state: "pending".to_owned(),
            task_dependencies: String::new(),
            task_retry_count: "0".to_owned(),
            task_max_retries: "3".to_owned(),
            task_created_at: "2026-06-11T00:00:00+00:00".to_owned(),
            project_slug: "hook-project".to_owned(),
            project_path: state_dir.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            from_phase: String::new(),
            to_phase: "pending".to_owned(),
            artifact: String::new(),
        }
    }

    fn read_hook_log(state_dir: &Path) -> String {
        fs::read_to_string(state_dir.join("hooks.log")).unwrap()
    }

    #[test]
    fn appends_hook_failure_log_on_spawn_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let path = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", temp.path());
        }
        let ctx = context(temp.path());
        let hook = HookEntry {
            on: "task_added".to_owned(),
            run: "definitely-not-a-command-for-agira-hook-test".to_owned(),
        };

        dispatch_hooks(&[hook], "task_added", &ctx, false);
        unsafe {
            match path {
                Some(path) => env::set_var("PATH", path),
                None => env::remove_var("PATH"),
            }
        }

        let contents = read_hook_log(temp.path());
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields.len(), 5);
        DateTime::parse_from_rfc3339(fields[0]).unwrap();
        assert_eq!(fields[1], "task_added");
        assert_eq!(fields[2], "task-001");
        assert_eq!(fields[3], "definitely-not-a-command-for-agira-hook-test");
        assert!(fields[4].starts_with("spawn_error: "));
    }

    #[test]
    fn appends_hook_failure_log_on_non_zero_exit_status() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let ctx = context(temp.path());
        let hook = HookEntry {
            on: "done".to_owned(),
            run: "exit 7".to_owned(),
        };

        dispatch_hooks(&[hook], "done", &ctx, false);

        let contents = read_hook_log(temp.path());
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields.len(), 5);
        DateTime::parse_from_rfc3339(fields[0]).unwrap();
        assert_eq!(fields[1], "done");
        assert_eq!(fields[2], "task-001");
        assert_eq!(fields[3], "exit 7");
        assert_eq!(fields[4], "exit_status: 7");
    }

    #[test]
    fn hook_failure_logging_failure_does_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let state_dir = temp.path().join("not-a-directory");
        fs::write(&state_dir, "file blocks create_dir_all").unwrap();
        let ctx = context(&state_dir);
        let hook = HookEntry {
            on: "done".to_owned(),
            run: "exit 2".to_owned(),
        };

        dispatch_hooks(&[hook], "done", &ctx, false);

        assert!(state_dir.is_file());
    }
}
