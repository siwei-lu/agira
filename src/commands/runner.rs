use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::{
    project::Project,
    runner::{Runner, RunnerStore, RunnerStoreError},
};

const DEFAULT_RUNNER_TYPE: &str = "claude-tmux";
const LOG_FILE_NAME: &str = "runner.log";

#[derive(Debug, Error)]
pub enum RunnerCommandError {
    #[error("runner already exists without a registry entry")]
    UnregisteredLiveSession,

    #[error("no runner is registered")]
    NoRunnerRegistered,

    #[error("runner session is not alive")]
    SessionNotAlive,

    #[error("runner log file not found: {path}")]
    LogFileNotFound { path: PathBuf },

    #[error("tmux {action} failed: {message}")]
    TmuxFailed {
        action: &'static str,
        message: String,
    },

    #[error("failed to run tmux {action}")]
    TmuxIo {
        action: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    RunnerStore(#[from] RunnerStoreError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerStartOutput {
    pub runner_id: String,
    pub session_name: String,
    pub already_running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerStatusOutput {
    pub runner_id: Option<String>,
    pub runner_type: Option<String>,
    pub current_task: Option<String>,
    pub liveness: String,
    pub heartbeat_age: Option<String>,
}

pub trait Tmux {
    fn has_session(&mut self, session_name: &str) -> Result<bool, RunnerCommandError>;
    fn new_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
    fn pipe_pane(&mut self, session_name: &str, log_path: &Path) -> Result<(), RunnerCommandError>;
    fn kill_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
    fn attach(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
}

pub struct ProcessTmux;

impl Tmux for ProcessTmux {
    fn has_session(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
        let output = tmux_command(["has-session", "-t", session_name], "has-session")?;
        Ok(output.status.success())
    }

    fn new_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(["new-session", "-d", "-s", session_name], "new-session")?,
            "new-session",
        )
    }

    fn pipe_pane(&mut self, session_name: &str, log_path: &Path) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(
                [
                    "pipe-pane",
                    "-t",
                    session_name,
                    "-o",
                    &format!("cat >> {}", shell_quote_path(log_path)),
                ],
                "pipe-pane",
            )?,
            "pipe-pane",
        )
    }

    fn kill_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(["kill-session", "-t", session_name], "kill-session")?,
            "kill-session",
        )
    }

    fn attach(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(["attach", "-t", session_name], "attach")?,
            "attach",
        )
    }
}

pub fn run_runner_start(
    project: &Project,
    runner_type: Option<&str>,
) -> Result<(), RunnerCommandError> {
    let mut tmux = ProcessTmux;
    let output = start_runner(project, runner_type, &mut tmux, Utc::now())?;
    print_runner_output(&format!(
        "runner {}\nsession {}{}",
        output.runner_id,
        output.session_name,
        if output.already_running {
            "\nalready running"
        } else {
            ""
        }
    ));
    Ok(())
}

pub fn run_runner_stop(project: &Project) -> Result<(), RunnerCommandError> {
    let mut tmux = ProcessTmux;
    stop_runner(project, &mut tmux)?;
    print_runner_output("runner stopped");
    Ok(())
}

pub fn run_runner_status(project: &Project) -> Result<(), RunnerCommandError> {
    let mut tmux = ProcessTmux;
    let status = status_runner(project, &mut tmux, Utc::now())?;
    print_runner_output(&format_status_output(&status));
    Ok(())
}

pub fn run_runner_attach(project: &Project) -> Result<(), RunnerCommandError> {
    let mut tmux = ProcessTmux;
    attach_runner(project, &mut tmux)
}

pub fn run_runner_logs(project: &Project, follow: bool) -> Result<(), RunnerCommandError> {
    let log_path = runner_log_path(project);
    if !log_path.is_file() {
        return Err(RunnerCommandError::LogFileNotFound { path: log_path });
    }

    if follow {
        return tail_follow(&log_path);
    }

    let contents = fs::read_to_string(&log_path).map_err(|source| RunnerCommandError::Read {
        path: log_path.clone(),
        source,
    })?;
    write_runner_output(&contents, &log_path)
}

fn start_runner<T: Tmux>(
    project: &Project,
    runner_type: Option<&str>,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> Result<RunnerStartOutput, RunnerCommandError> {
    let session_name = session_name(project);
    let mut store = RunnerStore::new(&project.state_dir)?;
    let live = tmux.has_session(&session_name)?;

    if live {
        if let Some(runner) = matching_runner(&store, &session_name) {
            return Ok(RunnerStartOutput {
                runner_id: runner.id.clone(),
                session_name,
                already_running: true,
            });
        }
        return Err(RunnerCommandError::UnregisteredLiveSession);
    }

    let runner_dir = runner_dir(project);
    fs::create_dir_all(&runner_dir).map_err(|source| RunnerCommandError::Write {
        path: runner_dir.clone(),
        source,
    })?;
    let log_path = runner_log_path(project);

    tmux.new_session(&session_name)?;
    tmux.pipe_pane(&session_name, &log_path)?;

    let runner_id = generate_runner_id(project, &session_name, now);
    store.register_at(
        &runner_id,
        runner_type
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_RUNNER_TYPE),
        &session_name,
        now,
    )?;

    Ok(RunnerStartOutput {
        runner_id,
        session_name,
        already_running: false,
    })
}

fn stop_runner<T: Tmux>(project: &Project, tmux: &mut T) -> Result<(), RunnerCommandError> {
    let session_name = session_name(project);
    let mut store = RunnerStore::new(&project.state_dir)?;
    let runner_id = matching_runner(&store, &session_name)
        .map(|runner| runner.id.clone())
        .ok_or(RunnerCommandError::NoRunnerRegistered)?;

    if !tmux.has_session(&session_name)? {
        return Err(RunnerCommandError::SessionNotAlive);
    }

    tmux.kill_session(&session_name)?;
    store.release_lease(&runner_id)?;
    store.deregister(&runner_id)?;
    Ok(())
}

fn status_runner<T: Tmux>(
    project: &Project,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> Result<RunnerStatusOutput, RunnerCommandError> {
    let session_name = session_name(project);
    let store = RunnerStore::new(&project.state_dir)?;
    let Some(runner) = matching_runner(&store, &session_name) else {
        return Ok(RunnerStatusOutput {
            runner_id: None,
            runner_type: None,
            current_task: None,
            liveness: "no runner registered".to_owned(),
            heartbeat_age: None,
        });
    };

    let live = tmux.has_session(&session_name)?;
    Ok(RunnerStatusOutput {
        runner_id: Some(runner.id.clone()),
        runner_type: Some(runner.runner_type.clone()),
        current_task: runner.current_task.clone(),
        liveness: if live {
            "running".to_owned()
        } else {
            "registered but session not alive".to_owned()
        },
        heartbeat_age: Some(format_heartbeat_age(runner.last_heartbeat.as_deref(), now)),
    })
}

fn attach_runner<T: Tmux>(project: &Project, tmux: &mut T) -> Result<(), RunnerCommandError> {
    let session_name = session_name(project);
    if !tmux.has_session(&session_name)? {
        return Err(RunnerCommandError::SessionNotAlive);
    }

    tmux.attach(&session_name)
}

fn matching_runner<'a>(store: &'a RunnerStore, session_name: &str) -> Option<&'a Runner> {
    store
        .registry()
        .runners
        .values()
        .find(|runner| runner.tmux_session == session_name)
}

fn session_name(project: &Project) -> String {
    format!("agira-{}", project.slug)
}

fn runner_dir(project: &Project) -> PathBuf {
    project.state_dir.join("runner")
}

fn runner_log_path(project: &Project) -> PathBuf {
    runner_dir(project).join(LOG_FILE_NAME)
}

fn generate_runner_id(project: &Project, session_name: &str, now: DateTime<Utc>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project.slug.as_bytes());
    hasher.update(session_name.as_bytes());
    hasher.update(now.to_rfc3339().as_bytes());
    hasher.update(std::process::id().to_string().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("runner-{}", &digest[..12])
}

fn format_heartbeat_age(last_heartbeat: Option<&str>, now: DateTime<Utc>) -> String {
    let Some(last_heartbeat) = last_heartbeat else {
        return "none".to_owned();
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(last_heartbeat) else {
        return "unknown".to_owned();
    };

    let seconds = (now - parsed.with_timezone(&Utc)).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn format_status_output(status: &RunnerStatusOutput) -> String {
    let Some(runner_id) = &status.runner_id else {
        return status.liveness.clone();
    };

    format!(
        "runner: {}\ntype: {}\ncurrent task: {}\nliveness: {}\nheartbeat: {}",
        runner_id,
        status.runner_type.as_deref().unwrap_or("unknown"),
        status.current_task.as_deref().unwrap_or("none"),
        status.liveness,
        status.heartbeat_age.as_deref().unwrap_or("none")
    )
}

fn tail_follow(path: &Path) -> Result<(), RunnerCommandError> {
    let status = Command::new("tail")
        .args(["-f"])
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|source| RunnerCommandError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(RunnerCommandError::Read {
            path: path.to_path_buf(),
            source: io::Error::other("tail failed"),
        })
    }
}

fn tmux_command<const N: usize>(
    args: [&str; N],
    action: &'static str,
) -> Result<std::process::Output, RunnerCommandError> {
    Command::new("tmux")
        .args(args)
        .output()
        .map_err(|source| RunnerCommandError::TmuxIo { action, source })
}

fn ensure_tmux_success(
    output: std::process::Output,
    action: &'static str,
) -> Result<(), RunnerCommandError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(RunnerCommandError::TmuxFailed {
            action,
            message: stderr_message(&output.stderr),
        })
    }
}

fn stderr_message(stderr: &[u8]) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_owned();
    if message.is_empty() {
        "tmux command exited non-zero".to_owned()
    } else {
        message
    }
}

fn shell_quote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn print_runner_output(message: &str) {
    #[cfg(test)]
    let mut captured = false;

    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(message);
            output.push('\n');
            captured = true;
        }
    });

    #[cfg(test)]
    if captured {
        return;
    }

    println!("{message}");
}

fn write_runner_output(contents: &str, path: &Path) -> Result<(), RunnerCommandError> {
    #[cfg(test)]
    let mut captured = false;

    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(contents);
            captured = true;
        }
    });

    #[cfg(test)]
    if captured {
        return Ok(());
    }

    io::stdout()
        .write_all(contents.as_bytes())
        .map_err(|source| RunnerCommandError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use chrono::{DateTime, Duration, Utc};

    use crate::core::{
        global_config::GlobalConfig, hooks::HookConfig, project::Project, runner::RunnerStore,
    };

    use super::{
        OUTPUT_CAPTURE, RunnerCommandError, Tmux, attach_runner, format_status_output,
        run_runner_logs, start_runner, status_runner, stop_runner,
    };

    #[derive(Default)]
    struct RecordingTmux {
        live: bool,
        calls: Vec<Vec<String>>,
    }

    impl RecordingTmux {
        fn live() -> Self {
            Self {
                live: true,
                calls: Vec::new(),
            }
        }
    }

    impl Tmux for RecordingTmux {
        fn has_session(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
            self.calls
                .push(vec!["has-session".into(), "-t".into(), session_name.into()]);
            Ok(self.live)
        }

        fn new_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                session_name.into(),
            ]);
            self.live = true;
            Ok(())
        }

        fn pipe_pane(
            &mut self,
            session_name: &str,
            log_path: &Path,
        ) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "pipe-pane".into(),
                "-t".into(),
                session_name.into(),
                "-o".into(),
                format!("cat >> '{}'", log_path.display()),
            ]);
            Ok(())
        }

        fn kill_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "kill-session".into(),
                "-t".into(),
                session_name.into(),
            ]);
            self.live = false;
            Ok(())
        }

        fn attach(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
            self.calls
                .push(vec!["attach".into(), "-t".into(), session_name.into()]);
            Ok(())
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
            .expect("parse fixed time")
            .with_timezone(&Utc)
    }

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let state_dir = dir.path().join(".agira").join("runner-repo");
        fs::create_dir_all(&state_dir).expect("state dir");

        (
            dir,
            Project {
                git_root: Path::new("/tmp/runner-repo").to_path_buf(),
                slug: "runner-repo".to_owned(),
                state_dir,
                global_config: GlobalConfig::default(),
                global_hooks: HookConfig::default(),
                project_hooks: HookConfig::default(),
            },
        )
    }

    fn capture_runner_output<F>(run: F) -> String
    where
        F: FnOnce(),
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });
        run();
        OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().expect("captured output"))
    }

    #[test]
    fn start_spawns_pipe_pane_and_registers_default_runner_type() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.session_name, "agira-runner-repo");
        assert!(!output.already_running);
        assert_eq!(
            &tmux.calls[..2],
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["new-session", "-d", "-s", "agira-runner-repo"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
        );

        assert_eq!(
            &tmux.calls[2][..3],
            ["pipe-pane", "-t", "agira-runner-repo"]
        );
        assert_eq!(tmux.calls[2][3], "-o");
        let pipe_command = &tmux.calls[2][4];
        assert!(pipe_command.ends_with("/runner/runner.log'"));

        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store
            .get_runner(&output.runner_id)
            .expect("runner registered");
        assert_eq!(runner.runner_type, "claude-tmux");
        assert_eq!(runner.tmux_session, "agira-runner-repo");
    }

    #[test]
    fn start_records_custom_runner_type() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let output =
            start_runner(&project, Some("custom"), &mut tmux, fixed_now()).expect("start runner");

        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store
            .get_runner(&output.runner_id)
            .expect("runner registered");
        assert_eq!(runner.runner_type, "custom");
    }

    #[test]
    fn start_is_idempotent_when_live_session_and_registry_entry_exist() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-existing",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.runner_id, "runner-existing");
        assert!(output.already_running);
        assert_eq!(
            tmux.calls,
            vec![vec!["has-session", "-t", "agira-runner-repo"]]
                .into_iter()
                .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stop_kills_deregisters_and_releases_lease() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-stop",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease("runner-stop", "task-120", Duration::minutes(5), fixed_now())
            .expect("acquire lease");
        drop(store);
        let mut tmux = RecordingTmux::live();

        stop_runner(&project, &mut tmux).expect("stop runner");

        assert_eq!(
            tmux.calls,
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["kill-session", "-t", "agira-runner-repo"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
        );
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-stop").is_none());
    }

    #[test]
    fn stop_without_runner_returns_user_error() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let result = stop_runner(&project, &mut tmux);

        assert!(matches!(
            result,
            Err(RunnerCommandError::NoRunnerRegistered)
        ));
    }

    #[test]
    fn status_reports_liveness_type_current_task_and_heartbeat_age() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at("runner-status", "custom", "agira-runner-repo", fixed_now())
            .expect("register runner");
        store
            .acquire_lease(
                "runner-status",
                "task-120",
                Duration::minutes(5),
                fixed_now(),
            )
            .expect("acquire lease");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let status = status_runner(&project, &mut tmux, fixed_now() + Duration::seconds(12))
            .expect("status runner");
        let output = format_status_output(&status);

        assert!(output.contains("type: custom"));
        assert!(output.contains("current task: task-120"));
        assert!(output.contains("liveness: running"));
        assert!(output.contains("heartbeat: 12s ago"));
    }

    #[test]
    fn status_distinguishes_registered_dead_session() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-dead",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::default();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(status.liveness, "registered but session not alive");
    }

    #[test]
    fn status_without_registered_runner_exits_successfully_with_message() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(format_status_output(&status), "no runner registered");
        assert!(tmux.calls.is_empty());
    }

    #[test]
    fn attach_invokes_tmux_attach_for_live_session() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::live();

        attach_runner(&project, &mut tmux).expect("attach runner");

        assert_eq!(
            tmux.calls,
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["attach", "-t", "agira-runner-repo"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn attach_errors_when_session_is_not_live() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let result = attach_runner(&project, &mut tmux);

        assert!(matches!(result, Err(RunnerCommandError::SessionNotAlive)));
    }

    #[test]
    fn logs_reads_pipe_pane_file_to_stdout() {
        let (_dir, project) = project();
        let log_path = project.state_dir.join("runner").join("runner.log");
        fs::create_dir_all(log_path.parent().unwrap()).expect("runner dir");
        fs::write(&log_path, "one\ntwo\n").expect("write log");

        let output = capture_runner_output(|| {
            run_runner_logs(&project, false).expect("read logs");
        });

        assert_eq!(output, "one\ntwo\n");
    }

    #[test]
    fn logs_missing_file_returns_error() {
        let (_dir, project) = project();

        let result = run_runner_logs(&project, false);

        assert!(matches!(
            result,
            Err(RunnerCommandError::LogFileNotFound { .. })
        ));
    }
}
