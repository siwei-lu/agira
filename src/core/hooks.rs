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
    pub task_prd_module_id: String,
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
            task_prd_module_id: task.prd_module_id.clone().unwrap_or_default(),
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

#[cfg(test)]
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

fn hook_debug_enabled() -> bool {
    matches!(std::env::var("AGIRA_HOOK_DEBUG").as_deref(), Ok("1"))
}

pub fn dispatch_hooks(hooks: &[HookEntry], ctx: &HookContext) {
    let debug_log = hook_debug_enabled().then(|| ctx.state_dir.join("hook-debug.log"));

    for hook in hooks {
        match debug_log.as_deref() {
            Some(path) => dispatch_hook_with_debug(hook, ctx, path),
            None => dispatch_hook(hook, ctx),
        }
    }
}

fn dispatch_hook(hook: &HookEntry, ctx: &HookContext) {
    if let Err(error) = hook_command(hook, ctx).spawn() {
        eprintln!("warning: hook spawn failed: {error}");
    }
}

fn dispatch_hook_with_debug(hook: &HookEntry, ctx: &HookContext, log_path: &Path) {
    let mut command = hook_command(hook, ctx);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut entry = HookDebugEntry {
        spawned_at: Utc::now().to_rfc3339(),
        event: hook.on.clone(),
        task_id: ctx.task_id.clone(),
        command: hook.run.clone(),
        spawn_result: "ok".to_owned(),
        pid: None,
        exit_status: None,
        stdout: None,
        stderr: None,
    };

    match command.spawn() {
        Ok(child) => {
            entry.pid = Some(child.id());
            match child.wait_with_output() {
                Ok(output) => {
                    entry.exit_status = output.status.code();
                    entry.stdout = Some(String::from_utf8_lossy(&output.stdout).into_owned());
                    entry.stderr = Some(String::from_utf8_lossy(&output.stderr).into_owned());
                }
                Err(error) => {
                    entry.spawn_result = format!("error:wait failed: {error}");
                    eprintln!("warning: hook wait failed: {error}");
                }
            }
        }
        Err(error) => {
            entry.spawn_result = format!("error:{error}");
            eprintln!("warning: hook spawn failed: {error}");
        }
    }

    append_hook_debug_entry(log_path, &entry);
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
        .env("AGIRA_TASK_PRD_MODULE_ID", &ctx.task_prd_module_id)
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::OsString,
        fs,
        sync::{Mutex, MutexGuard},
        thread,
        time::Duration,
    };

    use chrono::DateTime;
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::core::tasks::Task;

    #[test]
    fn absent_file_returns_empty_config() {
        let temp_dir = TempDir::new().unwrap();

        let config = load_hooks(&temp_dir.path().join("hooks.toml")).unwrap();

        assert!(config.hooks.is_empty());
    }

    #[test]
    fn valid_toml_parses_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(
            &path,
            r#"
[[hooks]]
on = "done"
run = "echo done"

[[hooks]]
on = "*"
run = "echo changed"
"#,
        )
        .unwrap();

        let config = load_hooks(&path).unwrap();

        assert_eq!(
            config,
            HookConfig {
                hooks: vec![
                    HookEntry {
                        on: "done".to_owned(),
                        run: "echo done".to_owned(),
                    },
                    HookEntry {
                        on: "*".to_owned(),
                        run: "echo changed".to_owned(),
                    },
                ],
            }
        );
    }

    #[test]
    fn save_hooks_writes_toml_that_load_hooks_reads_back_unchanged() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        let config = HookConfig {
            hooks: vec![
                HookEntry {
                    on: "done".to_owned(),
                    run: "printf ok".to_owned(),
                },
                HookEntry {
                    on: "*".to_owned(),
                    run: "echo all".to_owned(),
                },
            ],
        };

        save_hooks(&path, &config).unwrap();
        let loaded = load_hooks(&path).unwrap();

        assert_eq!(loaded, config);
    }

    #[test]
    fn save_hooks_creates_parent_directories_as_needed() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nested").join("hooks.toml");
        let config = HookConfig {
            hooks: vec![HookEntry {
                on: "failed".to_owned(),
                run: "echo fail".to_owned(),
            }],
        };

        save_hooks(&path, &config).unwrap();

        assert_eq!(load_hooks(&path).unwrap(), config);
    }

    #[test]
    fn save_hooks_preserving_toml_replaces_hooks_and_preserves_other_keys() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"default_max_retries = 9
extra = "keep"

[[hooks]]
on = "done"
run = "echo old"
"#,
        )
        .unwrap();
        let config = HookConfig {
            hooks: vec![HookEntry {
                on: "failed".to_owned(),
                run: "echo new".to_owned(),
            }],
        };

        save_hooks_preserving_toml(&path, &config).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("default_max_retries = 9"));
        assert!(contents.contains("extra = \"keep\""));
        assert!(!contents.contains("echo old"));
        assert_eq!(load_hooks(&path).unwrap(), config);
    }

    #[test]
    fn save_hooks_preserving_toml_removes_empty_hooks_key() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        fs::write(
            &path,
            r#"default_max_retries = 3

[[hooks]]
on = "done"
run = "echo done"
"#,
        )
        .unwrap();

        save_hooks_preserving_toml(&path, &HookConfig::default()).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("default_max_retries = 3"));
        assert!(!contents.contains("[[hooks]]"));
        assert_eq!(load_hooks(&path).unwrap(), HookConfig::default());
    }

    #[test]
    fn malformed_toml_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(&path, "[[hooks]]\non = [").unwrap();

        let error = load_hooks(&path).unwrap_err();

        match error {
            HookConfigError::Parse {
                path: error_path, ..
            } => assert_eq!(error_path, path),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn collect_hooks_matches_wildcard_for_any_phase() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "*"
run = "echo global"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "verifying").unwrap();

        assert_eq!(
            hooks,
            vec![HookEntry {
                on: "*".to_owned(),
                run: "echo global".to_owned(),
            }]
        );
    }

    #[test]
    fn collect_hooks_filters_specific_phase_in_global_first_order() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo global done"

[[hooks]]
on = "failed"
run = "echo global failed"
"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo project done"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "done").unwrap();

        assert_eq!(
            hooks,
            vec![
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo global done".to_owned(),
                },
                HookEntry {
                    on: "done".to_owned(),
                    run: "echo project done".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn collect_hooks_excludes_non_matching_hooks() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"
[[hooks]]
on = "done"
run = "echo done"
"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
on = "failed"
run = "echo failed"
"#,
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "in_progress").unwrap();

        assert!(hooks.is_empty());
    }

    #[test]
    fn collect_hooks_absent_project_file_returns_empty_list() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("config.toml"),
            "default_max_retries = 3\n",
        )
        .unwrap();

        let hooks = collect_hooks(temp_dir.path(), "test-project", "done").unwrap();

        assert!(hooks.is_empty());
    }

    #[test]
    fn dispatch_hooks_exposes_project_path_env() {
        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().join("state");
        let output_path = temp_dir.path().join("project_path.txt");
        let task = test_task();
        let ctx = HookContext::new(
            &task,
            "test-project",
            temp_dir.path(),
            &state_dir,
            "",
            "done",
            "",
        );
        let hooks = vec![HookEntry {
            on: "done".to_owned(),
            run: format!(
                "printf '%s' \"$AGIRA_PROJECT_PATH\" > {}",
                output_path.display()
            ),
        }];

        dispatch_hooks(&hooks, &ctx);

        let contents = read_file_eventually(&output_path);
        assert_eq!(contents, temp_dir.path().to_string_lossy());
    }

    #[test]
    fn default_mode_no_log_file_created() {
        let _env = HookDebugEnvGuard::set(None);
        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().join("state");
        let task = test_task();
        let ctx = HookContext::new(
            &task,
            "test-project",
            temp_dir.path(),
            &state_dir,
            "",
            "done",
            "",
        );
        let debug_path = state_dir.join("hook-debug.log");
        let hooks = vec![HookEntry {
            on: "done".to_owned(),
            run: "true".to_owned(),
        }];

        dispatch_hooks(&hooks, &ctx);

        assert!(!debug_path.exists());
    }

    #[test]
    fn debug_mode_log_file_created_with_expected_fields() {
        let _env = HookDebugEnvGuard::set(Some("1"));
        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().join("state");
        let task = test_task();
        let ctx = HookContext::new(
            &task,
            "test-project",
            temp_dir.path(),
            &state_dir,
            "",
            "done",
            "",
        );
        let debug_path = state_dir.join("hook-debug.log");
        let hooks = vec![HookEntry {
            on: "done".to_owned(),
            run: "printf 'hook stdout'; printf 'hook stderr' >&2".to_owned(),
        }];

        dispatch_hooks(&hooks, &ctx);

        let entries = read_debug_values(&debug_path);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_debug_keys(entry);
        assert_eq!(entry["event"], "done");
        assert_eq!(entry["task_id"], "task-001");
        assert_eq!(
            entry["command"],
            "printf 'hook stdout'; printf 'hook stderr' >&2"
        );
        assert_eq!(entry["spawn_result"], "ok");
        assert!(entry["pid"].as_u64().is_some_and(|pid| pid > 0));
        assert_eq!(entry["exit_status"], 0);
        assert_eq!(entry["stdout"], "hook stdout");
        assert_eq!(entry["stderr"], "hook stderr");
        DateTime::parse_from_rfc3339(entry["spawned_at"].as_str().unwrap()).unwrap();
    }

    #[test]
    fn debug_mode_spawn_failure_logged() {
        let _env = HookDebugEnvGuard::set(Some("1"));
        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().join("state");
        let task = test_task();
        let ctx = HookContext::new(
            &task,
            "test-project",
            temp_dir.path(),
            &state_dir,
            "",
            "failed",
            "",
        );
        let debug_path = state_dir.join("hook-debug.log");
        let hooks = vec![HookEntry {
            on: "failed".to_owned(),
            run: "invalid\0command".to_owned(),
        }];

        dispatch_hooks(&hooks, &ctx);

        let entries = read_debug_values(&debug_path);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_debug_keys(entry);
        assert_eq!(entry["event"], "failed");
        assert_eq!(entry["task_id"], "task-001");
        assert!(
            entry["spawn_result"]
                .as_str()
                .unwrap()
                .starts_with("error:")
        );
        assert!(entry["pid"].is_null());
        assert!(entry["exit_status"].is_null());
        assert!(entry["stdout"].is_null());
        assert!(entry["stderr"].is_null());
    }

    fn test_task() -> Task {
        Task {
            id: "task-001".to_owned(),
            title: "Test task".to_owned(),
            description: "Task description".to_owned(),
            state: "done".to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            prd_module_id: None,
            dependencies: vec![],
            retry_count: 0,
            max_retries: 3,
            phases: BTreeMap::new(),
            history: vec![],
            created_at: "2026-06-05T00:00:00Z".to_owned(),
        }
    }

    fn read_file_eventually(path: &Path) -> String {
        for _ in 0..50 {
            if let Ok(contents) = fs::read_to_string(path) {
                return contents;
            }

            thread::sleep(Duration::from_millis(10));
        }

        panic!("hook output was not written to {}", path.display());
    }

    fn read_debug_values(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn assert_debug_keys(entry: &Value) {
        let actual = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "spawned_at",
            "event",
            "task_id",
            "command",
            "spawn_result",
            "pid",
            "exit_status",
            "stdout",
            "stderr",
        ]);

        assert_eq!(actual, expected);
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct HookDebugEnvGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl HookDebugEnvGuard {
        fn set(value: Option<&str>) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let previous = std::env::var_os("AGIRA_HOOK_DEBUG");

            unsafe {
                match value {
                    Some(value) => std::env::set_var("AGIRA_HOOK_DEBUG", value),
                    None => std::env::remove_var("AGIRA_HOOK_DEBUG"),
                }
            }

            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for HookDebugEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("AGIRA_HOOK_DEBUG", value),
                    None => std::env::remove_var("AGIRA_HOOK_DEBUG"),
                }
            }
        }
    }
}
