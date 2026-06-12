use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    orchestrator::{
        DEFAULT_ORCHESTRATOR_KICKOFF, DEFAULT_ORCHESTRATOR_TEMPLATE, assemble_orchestrator_prompt,
        load_template_override,
    },
    project::Project,
    runner::{Runner, RunnerStore, RunnerStoreError, is_lease_expired},
};

const DEFAULT_RUNNER_TYPE: &str = "claude-tmux";
const LOG_FILE_NAME: &str = "runner.log";
const HEARTBEAT_STALENESS_THRESHOLD: Duration = Duration::minutes(10);
const TUI_READY_MAX_ATTEMPTS: usize = 60;
const TUI_READY_BACKOFF: StdDuration = StdDuration::from_millis(500);

#[derive(Debug, Error)]
pub enum RunnerCommandError {
    #[error("runner already exists without a registry entry")]
    UnregisteredLiveSession,

    #[error("no runner is registered")]
    NoRunnerRegistered,

    #[error("runner session is not alive")]
    SessionNotAlive,

    #[error("runner session did not become ready for kickoff")]
    RunnerNotReady,

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
    Config(#[from] ConfigError),

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
    fn pane_alive(&mut self, session_name: &str) -> Result<bool, RunnerCommandError>;
    fn pane_process_group(&mut self, session_name: &str)
    -> Result<Option<i32>, RunnerCommandError>;
    fn new_session(
        &mut self,
        session_name: &str,
        launch_command: &str,
    ) -> Result<(), RunnerCommandError>;
    fn pipe_pane(&mut self, session_name: &str, log_path: &Path) -> Result<(), RunnerCommandError>;
    fn wait_for_ready(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
    fn send_keys(
        &mut self,
        session_name: &str,
        keys: &str,
        enter: bool,
    ) -> Result<(), RunnerCommandError>;
    fn kill_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
    fn kill_process_group(&mut self, pgid: i32) -> Result<(), RunnerCommandError>;
    fn attach(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
}

pub struct ProcessTmux;

impl Tmux for ProcessTmux {
    fn has_session(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
        let output = tmux_command(["has-session", "-t", session_name], "has-session")?;
        Ok(output.status.success())
    }

    fn pane_alive(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
        let Some(pane_pid) = self.pane_pid(session_name)? else {
            return Ok(true);
        };
        Ok(process_alive(pane_pid))
    }

    fn pane_process_group(
        &mut self,
        session_name: &str,
    ) -> Result<Option<i32>, RunnerCommandError> {
        let Some(pane_pid) = self.pane_pid(session_name)? else {
            return Ok(None);
        };
        Ok(process_group_for(pane_pid))
    }

    fn new_session(
        &mut self,
        session_name: &str,
        launch_command: &str,
    ) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command_with_args(
                ["new-session", "-d", "-s", session_name, launch_command],
                "new-session",
            )?,
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

    fn wait_for_ready(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        for attempt in 0..TUI_READY_MAX_ATTEMPTS {
            let output = tmux_command(["capture-pane", "-p", "-t", session_name], "capture-pane")?;
            if !output.status.success() {
                return Err(RunnerCommandError::TmuxFailed {
                    action: "capture-pane",
                    message: stderr_message(&output.stderr),
                });
            }
            let pane = String::from_utf8_lossy(&output.stdout);
            if claude_tui_input_ready(&pane) {
                return Ok(());
            }
            if attempt + 1 < TUI_READY_MAX_ATTEMPTS {
                thread::sleep(TUI_READY_BACKOFF);
            }
        }

        Err(RunnerCommandError::RunnerNotReady)
    }

    fn send_keys(
        &mut self,
        session_name: &str,
        keys: &str,
        enter: bool,
    ) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(["send-keys", "-t", session_name, keys], "send-keys")?,
            "send-keys",
        )?;
        if enter {
            ensure_tmux_success(
                tmux_command(["send-keys", "-t", session_name, "Enter"], "send-keys")?,
                "send-keys",
            )?;
        }
        Ok(())
    }

    fn kill_session(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        ensure_tmux_success(
            tmux_command(["kill-session", "-t", session_name], "kill-session")?,
            "kill-session",
        )
    }

    fn kill_process_group(&mut self, pgid: i32) -> Result<(), RunnerCommandError> {
        if kill_process_group(pgid) {
            Ok(())
        } else {
            Err(RunnerCommandError::TmuxFailed {
                action: "killpg",
                message: "failed to kill process group".to_owned(),
            })
        }
    }

    fn attach(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
        let status = Command::new("tmux")
            .arg("attach")
            .arg("-t")
            .arg(session_name)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|source| RunnerCommandError::TmuxIo {
                action: "attach",
                source,
            })?;

        if status.success() {
            Ok(())
        } else {
            Err(RunnerCommandError::TmuxFailed {
                action: "attach",
                message: "tmux attach exited non-zero".to_owned(),
            })
        }
    }
}

impl ProcessTmux {
    fn pane_pid(&mut self, session_name: &str) -> Result<Option<i32>, RunnerCommandError> {
        let output = tmux_command(
            ["list-panes", "-t", session_name, "-F", "#{pane_pid}"],
            "list-panes",
        )?;
        if !output.status.success() {
            return Ok(None);
        }

        let pid = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<i32>().ok());
        Ok(pid)
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

/// Idempotent ensure-runner: starts a runner if none is live, otherwise returns the
/// existing healthy runner.  Callers that need to handle failure non-fatally should
/// call this and `eprintln!` any returned error; task creation continues regardless.
pub(crate) fn ensure_runner_with_tmux<T: Tmux>(
    project: &Project,
    runner_type: &str,
    tmux: &mut T,
) -> Result<RunnerStartOutput, RunnerCommandError> {
    start_runner(project, Some(runner_type), tmux, Utc::now())
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
        if let Some(runner) = matching_runner(&store, &session_name).cloned() {
            if !tmux.pane_alive(&session_name)? {
                tmux.kill_session(&session_name)?;
                reap_recorded_process_group(tmux, &runner)?;
                store.deregister(&runner.id)?;
                return cold_start_runner(project, runner_type, tmux, &mut store, now);
            }

            if runner.current_task.is_some()
                && (is_lease_expired(runner.lease_expires_at.as_deref(), now)
                    || is_heartbeat_stale(runner.last_heartbeat.as_deref(), now))
            {
                store.release_lease(&runner.id)?;
            }

            return Ok(RunnerStartOutput {
                runner_id: runner.id.clone(),
                session_name,
                already_running: true,
            });
        }
        return Err(RunnerCommandError::UnregisteredLiveSession);
    }

    if let Some(runner) = matching_runner(&store, &session_name).cloned() {
        reap_recorded_process_group(tmux, &runner)?;
        store.deregister(&runner.id)?;
    }

    cold_start_runner(project, runner_type, tmux, &mut store, now)
}

fn cold_start_runner<T: Tmux>(
    project: &Project,
    runner_type: Option<&str>,
    tmux: &mut T,
    store: &mut RunnerStore,
    now: DateTime<Utc>,
) -> Result<RunnerStartOutput, RunnerCommandError> {
    let session_name = session_name(project);
    let runner_dir = runner_dir(project);
    fs::create_dir_all(&runner_dir).map_err(|source| RunnerCommandError::Write {
        path: runner_dir.clone(),
        source,
    })?;
    let log_path = runner_log_path(project);
    let runner_id = generate_runner_id(project, &session_name, now);
    let config = load_project_config(
        &project.state_dir.join("config.json"),
        &project.global_config,
    )?;
    let template = match &project.global_config.runner.orchestrator_template_path {
        Some(path) => match load_template_override(path) {
            Ok(template) => template,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                DEFAULT_ORCHESTRATOR_TEMPLATE.to_owned()
            }
            Err(source) => {
                return Err(RunnerCommandError::Read {
                    path: path.clone(),
                    source,
                });
            }
        },
        None => DEFAULT_ORCHESTRATOR_TEMPLATE.to_owned(),
    };
    let prompt = assemble_orchestrator_prompt(&template, &config);
    let launch_command = claude_launch_command(&runner_id, &prompt);

    tmux.new_session(&session_name, &launch_command)?;
    tmux.pipe_pane(&session_name, &log_path)?;
    if let Err(error) = tmux
        .wait_for_ready(&session_name)
        .and_then(|_| tmux.send_keys(&session_name, DEFAULT_ORCHESTRATOR_KICKOFF, true))
    {
        let _ = tmux.kill_session(&session_name);
        return Err(error);
    }

    let runner = store.register_at(
        &runner_id,
        runner_type
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_RUNNER_TYPE),
        &session_name,
        now,
    )?;
    let pgid = tmux.pane_process_group(&session_name).ok().flatten();
    store.record_process_group(&runner.id, pgid)?;

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

pub(crate) fn status_runner<T: Tmux>(
    project: &Project,
    tmux: &mut T,
    now: DateTime<Utc>,
) -> Result<RunnerStatusOutput, RunnerCommandError> {
    let session_name = session_name(project);
    let store = RunnerStore::new(&project.state_dir)?;
    let live = tmux.has_session(&session_name)?;
    let Some(runner) = matching_runner(&store, &session_name) else {
        return Ok(RunnerStatusOutput {
            runner_id: None,
            runner_type: None,
            current_task: None,
            liveness: if live {
                "session running but no runner registered".to_owned()
            } else {
                "no runner registered".to_owned()
            },
            heartbeat_age: None,
        });
    };

    let liveness = if !live {
        "stale"
    } else if !tmux.pane_alive(&session_name)? {
        "zombie"
    } else if runner.current_task.is_some()
        && (is_lease_expired(runner.lease_expires_at.as_deref(), now)
            || is_heartbeat_stale(runner.last_heartbeat.as_deref(), now))
    {
        "stale"
    } else {
        "live"
    };

    Ok(RunnerStatusOutput {
        runner_id: Some(runner.id.clone()),
        runner_type: Some(runner.runner_type.clone()),
        current_task: runner.current_task.clone(),
        liveness: liveness.to_owned(),
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

fn reap_recorded_process_group<T: Tmux>(
    tmux: &mut T,
    runner: &Runner,
) -> Result<(), RunnerCommandError> {
    if let Some(pgid) = runner.pgid {
        tmux.kill_process_group(pgid)?;
    }
    Ok(())
}

fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }

    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn process_group_for(pid: i32) -> Option<i32> {
    if pid <= 0 {
        return None;
    }

    let pgid = unsafe { libc::getpgid(pid) };
    if pgid > 0 { Some(pgid) } else { None }
}

fn kill_process_group(pgid: i32) -> bool {
    if pgid <= 0 {
        return true;
    }

    if unsafe { libc::killpg(pgid, libc::SIGTERM) } == 0 {
        return true;
    }

    io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
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

fn claude_launch_command(runner_id: &str, prompt: &str) -> String {
    format!(
        "AGIRA_RUNNER_ID={} claude --append-system-prompt {}",
        shell_quote_string(runner_id),
        shell_quote_string(prompt)
    )
}

fn claude_tui_input_ready(pane: &str) -> bool {
    pane.lines().any(|line| {
        let line = line
            .trim_start()
            .trim_start_matches(|ch: char| !ch.is_ascii() && ch != '❯')
            .trim_start();
        line.starts_with('>') || line.starts_with('❯')
    })
}

pub(crate) fn is_heartbeat_stale(last_heartbeat: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(hb) = last_heartbeat else {
        return false; // fail-safe: missing heartbeat → treat as live
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(hb) else {
        return false; // fail-safe: unparseable → treat as live
    };
    now - parsed.with_timezone(&Utc) > HEARTBEAT_STALENESS_THRESHOLD
}

pub(crate) fn format_heartbeat_age(last_heartbeat: Option<&str>, now: DateTime<Utc>) -> String {
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
    tmux_command_with_args(args, action)
}

fn tmux_command_with_args<const N: usize>(
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

fn shell_quote_string(text: &str) -> String {
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
        config::{Config, ConfigError, PhaseDef},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
        runner::RunnerStore,
    };

    use super::{
        OUTPUT_CAPTURE, RunnerCommandError, Tmux, attach_runner, claude_tui_input_ready,
        format_status_output, run_runner_logs, start_runner, status_runner, stop_runner,
    };

    struct RecordingTmux {
        live: bool,
        pane_alive: bool,
        pane_pgid: Option<i32>,
        ready: bool,
        calls: Vec<Vec<String>>,
    }

    impl Default for RecordingTmux {
        fn default() -> Self {
            Self {
                live: false,
                pane_alive: false,
                pane_pgid: Some(4242),
                ready: true,
                calls: Vec::new(),
            }
        }
    }

    impl RecordingTmux {
        fn live() -> Self {
            Self {
                live: true,
                pane_alive: true,
                pane_pgid: Some(4242),
                ready: true,
                calls: Vec::new(),
            }
        }

        fn zombie() -> Self {
            Self {
                live: true,
                pane_alive: false,
                pane_pgid: Some(4242),
                ready: true,
                calls: Vec::new(),
            }
        }

        fn never_ready() -> Self {
            Self {
                live: false,
                pane_alive: false,
                pane_pgid: Some(4242),
                ready: false,
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

        fn pane_alive(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
            self.calls
                .push(vec!["pane-alive".into(), "-t".into(), session_name.into()]);
            Ok(self.live && self.pane_alive)
        }

        fn pane_process_group(
            &mut self,
            session_name: &str,
        ) -> Result<Option<i32>, RunnerCommandError> {
            self.calls
                .push(vec!["pane-pgid".into(), "-t".into(), session_name.into()]);
            Ok(self.pane_pgid)
        }

        fn new_session(
            &mut self,
            session_name: &str,
            launch_command: &str,
        ) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                session_name.into(),
                launch_command.into(),
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

        fn wait_for_ready(&mut self, session_name: &str) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "wait-for-ready".into(),
                "-t".into(),
                session_name.into(),
            ]);
            if self.ready {
                Ok(())
            } else {
                Err(RunnerCommandError::RunnerNotReady)
            }
        }

        fn send_keys(
            &mut self,
            session_name: &str,
            keys: &str,
            enter: bool,
        ) -> Result<(), RunnerCommandError> {
            self.calls.push(vec![
                "send-keys".into(),
                "-t".into(),
                session_name.into(),
                keys.into(),
            ]);
            if enter {
                self.calls.push(vec![
                    "send-keys".into(),
                    "-t".into(),
                    session_name.into(),
                    "Enter".into(),
                ]);
            }
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

        fn kill_process_group(&mut self, pgid: i32) -> Result<(), RunnerCommandError> {
            self.calls.push(vec!["killpg".into(), pgid.to_string()]);
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

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![
                (
                    "implementing".to_owned(),
                    PhaseDef {
                        model: Some("dispatch exec -a codex".to_owned()),
                        duty: Some("write tests first".to_owned()),
                        gate: None,
                    },
                ),
                (
                    "verifying".to_owned(),
                    PhaseDef {
                        model: Some("sonnet".to_owned()),
                        duty: Some("run cargo test".to_owned()),
                        gate: None,
                    },
                ),
            ],
            3,
        )
    }

    fn write_project_config(project: &Project) {
        fs::write(
            project.state_dir.join("config.json"),
            serde_json::to_string_pretty(&test_config()).expect("serialize config"),
        )
        .expect("write config");
    }

    fn project_without_config() -> (tempfile::TempDir, Project) {
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

    fn project() -> (tempfile::TempDir, Project) {
        let (dir, project) = project_without_config();
        write_project_config(&project);
        (dir, project)
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
            tmux.calls[0],
            vec!["has-session", "-t", "agira-runner-repo"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &tmux.calls[1][..4],
            ["new-session", "-d", "-s", "agira-runner-repo"]
        );
        assert!(tmux.calls[1][4].starts_with("AGIRA_RUNNER_ID='runner-"));
        assert!(tmux.calls[1][4].contains("' claude --append-system-prompt '"));
        assert!(tmux.calls[1][4].contains("agira-orchestrator-template-v1"));
        assert!(
            tmux.calls[1][4]
                .contains("| implementing | dispatch exec -a codex | write tests first |")
        );

        assert_eq!(
            &tmux.calls[2][..3],
            ["pipe-pane", "-t", "agira-runner-repo"]
        );
        assert_eq!(tmux.calls[2][3], "-o");
        let pipe_command = &tmux.calls[2][4];
        assert!(pipe_command.ends_with("/runner/runner.log'"));
        assert_eq!(
            tmux.calls[3],
            vec!["wait-for-ready", "-t", "agira-runner-repo"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &tmux.calls[4][..3],
            ["send-keys", "-t", "agira-runner-repo"]
        );
        assert!(tmux.calls[4][3].contains("agira task todo --runner \"$AGIRA_RUNNER_ID\""));
        assert_eq!(
            tmux.calls[5],
            vec!["send-keys", "-t", "agira-runner-repo", "Enter"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );

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
    fn start_injects_override_template_on_cold_start() {
        let (dir, mut project) = project();
        let template_path = dir.path().join("override-template.md");
        fs::write(
            &template_path,
            "custom static marker\n\nThin-orchestrator rule: delegate only",
        )
        .expect("write override");
        project.global_config.runner.orchestrator_template_path = Some(template_path);
        let mut tmux = RecordingTmux::default();

        start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        let launch_command = &tmux.calls[1][4];
        assert!(launch_command.contains("custom static marker"));
        assert!(!launch_command.contains("agira-orchestrator-template-v1"));
        assert!(launch_command.contains("| verifying | sonnet | run cargo test |"));
    }

    #[test]
    fn start_uses_embedded_template_when_override_template_is_missing() {
        let (dir, mut project) = project();
        project.global_config.runner.orchestrator_template_path =
            Some(dir.path().join("missing-template.md"));
        let mut tmux = RecordingTmux::default();

        start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        let launch_command = &tmux.calls[1][4];
        assert!(launch_command.contains("agira-orchestrator-template-v1"));
        assert!(launch_command.contains("| verifying | sonnet | run cargo test |"));
    }

    #[test]
    fn start_errors_before_creating_session_when_config_is_missing() {
        let (_dir, project) = project_without_config();
        let mut tmux = RecordingTmux::default();

        let result = start_runner(&project, None, &mut tmux, fixed_now());

        assert!(matches!(
            result,
            Err(RunnerCommandError::Config(ConfigError::NotFound { .. }))
        ));
        assert_eq!(
            tmux.calls,
            vec![vec!["has-session", "-t", "agira-runner-repo"]]
                .into_iter()
                .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn start_errors_and_cleans_up_when_runner_tui_is_not_ready() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::never_ready();

        let result = start_runner(&project, None, &mut tmux, fixed_now());

        assert!(matches!(result, Err(RunnerCommandError::RunnerNotReady)));
        assert_eq!(
            tmux.calls,
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec![
                    "new-session",
                    "-d",
                    "-s",
                    "agira-runner-repo",
                    &tmux.calls[1][4],
                ],
                vec![
                    "pipe-pane",
                    "-t",
                    "agira-runner-repo",
                    "-o",
                    &tmux.calls[2][4],
                ],
                vec!["wait-for-ready", "-t", "agira-runner-repo"],
                vec!["kill-session", "-t", "agira-runner-repo"],
            ]
        );
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.registry().runners.is_empty());
    }

    #[test]
    fn start_rebuilds_zombie_session_and_removes_stale_registry_entry() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-zombie",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::zombie();

        let output = start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert_ne!(output.runner_id, "runner-zombie");
        assert!(!output.already_running);
        assert_eq!(
            &tmux.calls[..8],
            [
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["pane-alive", "-t", "agira-runner-repo"],
                vec!["kill-session", "-t", "agira-runner-repo"],
                vec![
                    "new-session",
                    "-d",
                    "-s",
                    "agira-runner-repo",
                    &tmux.calls[3][4]
                ],
                vec![
                    "pipe-pane",
                    "-t",
                    "agira-runner-repo",
                    "-o",
                    &tmux.calls[4][4]
                ],
                vec!["wait-for-ready", "-t", "agira-runner-repo"],
                vec!["send-keys", "-t", "agira-runner-repo", &tmux.calls[6][3]],
                vec!["send-keys", "-t", "agira-runner-repo", "Enter"],
            ]
        );
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-zombie").is_none());
        assert!(store.get_runner(&output.runner_id).is_some());
    }

    #[test]
    fn start_releases_stale_lease_and_takes_over_live_session() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-stale",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-stale",
                "task-122",
                Duration::minutes(5),
                fixed_now(),
            )
            .expect("acquire lease");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(6),
        )
        .expect("start runner");

        assert_eq!(output.runner_id, "runner-stale");
        assert!(output.already_running);
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store.get_runner("runner-stale").expect("runner exists");
        assert!(runner.current_task.is_none());
        assert!(runner.lease_expires_at.is_none());
        assert!(runner.last_heartbeat.is_none());
    }

    #[test]
    fn start_releases_stale_heartbeat_with_fresh_lease() {
        // Lease is still valid (expires in 4 minutes), but last_heartbeat is older
        // than HEARTBEAT_STALENESS_THRESHOLD (10 minutes). Runner should be reclaimed.
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-stale-hb",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-stale-hb",
                "task-122",
                Duration::minutes(15),
                fixed_now(),
            )
            .expect("acquire lease");
        // Manually set a stale last_heartbeat (11 minutes old relative to "now")
        let mut registry = store.registry().clone();
        let runner = registry.runners.get_mut("runner-stale-hb").unwrap();
        let stale_hb = (fixed_now() - Duration::minutes(11)).to_rfc3339();
        runner.last_heartbeat = Some(stale_hb);
        store.save(registry).expect("save registry");
        drop(store);

        let mut tmux = RecordingTmux::live();
        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.runner_id, "runner-stale-hb");
        assert!(output.already_running);
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store.get_runner("runner-stale-hb").expect("runner exists");
        assert!(runner.current_task.is_none(), "lease should be released");
        assert!(runner.lease_expires_at.is_none());
        assert!(runner.last_heartbeat.is_none());
    }

    #[test]
    fn start_keeps_fresh_live_session_idempotent() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-fresh",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-fresh",
                "task-122",
                Duration::minutes(5),
                fixed_now(),
            )
            .expect("acquire lease");
        let before = store.registry().clone();
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert_eq!(output.runner_id, "runner-fresh");
        assert!(output.already_running);
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert_eq!(store.registry(), &before);
        let runner = store.get_runner("runner-fresh").expect("runner exists");
        assert_eq!(runner.current_task.as_deref(), Some("task-122"));
    }

    #[test]
    fn start_treats_unparseable_lease_as_live_fail_safe() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-ambiguous",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        let mut registry = store.registry().clone();
        let runner = registry
            .runners
            .get_mut("runner-ambiguous")
            .expect("runner exists");
        runner.current_task = Some("task-122".to_owned());
        runner.lease_expires_at = Some("not-a-timestamp".to_owned());
        runner.last_heartbeat = Some("not-a-timestamp".to_owned());
        store.save(registry).expect("save registry");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(&project, None, &mut tmux, fixed_now() + Duration::days(1))
            .expect("start runner");

        assert_eq!(output.runner_id, "runner-ambiguous");
        assert!(output.already_running);
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store.get_runner("runner-ambiguous").expect("runner exists");
        assert_eq!(runner.current_task.as_deref(), Some("task-122"));
        assert_eq!(runner.lease_expires_at.as_deref(), Some("not-a-timestamp"));
    }

    #[test]
    fn start_reaps_recorded_process_group_when_session_is_gone() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-gone",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        let mut registry = store.registry().clone();
        registry
            .runners
            .get_mut("runner-gone")
            .expect("runner exists")
            .pgid = Some(4242);
        store.save(registry).expect("save registry");
        drop(store);
        let mut tmux = RecordingTmux::default();

        start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert!(tmux.calls.contains(&vec!["killpg".into(), "4242".into()]));
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
        assert!(!tmux.calls.iter().any(|call| call[0] == "send-keys"));
        assert_eq!(
            tmux.calls,
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["pane-alive", "-t", "agira-runner-repo"],
            ]
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
        assert!(output.contains("liveness: live"));
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

        assert_eq!(status.liveness, "stale");
    }

    #[test]
    fn status_reports_zombie_without_mutating_registry_or_session() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-zombie-status",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        let before = store.registry().clone();
        drop(store);
        let mut tmux = RecordingTmux::zombie();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(status.liveness, "zombie");
        assert!(!tmux.calls.iter().any(|call| call[0] == "kill-session"));
        assert!(!tmux.calls.iter().any(|call| call[0] == "killpg"));
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert_eq!(store.registry(), &before);
    }

    #[test]
    fn status_reports_stale_lease_without_releasing_it() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-stale-status",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-stale-status",
                "task-122",
                Duration::minutes(5),
                fixed_now(),
            )
            .expect("acquire lease");
        let before = store.registry().clone();
        drop(store);
        let mut tmux = RecordingTmux::live();

        let status = status_runner(&project, &mut tmux, fixed_now() + Duration::minutes(6))
            .expect("status runner");

        assert_eq!(status.liveness, "stale");
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert_eq!(store.registry(), &before);
    }

    #[test]
    fn status_without_registered_runner_exits_successfully_with_message() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::default();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(format_status_output(&status), "no runner registered");
        assert_eq!(
            tmux.calls,
            vec![vec!["has-session", "-t", "agira-runner-repo"]]
                .into_iter()
                .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn status_reports_orphaned_live_session_without_registry_entry() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::live();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(
            format_status_output(&status),
            "session running but no runner registered"
        );
        assert_eq!(
            tmux.calls,
            vec![vec!["has-session", "-t", "agira-runner-repo"]]
                .into_iter()
                .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
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
    fn tui_input_ready_detects_ascii_prompt() {
        assert!(claude_tui_input_ready("│ > \n"));
        assert!(claude_tui_input_ready("  > type here\n"));
    }

    #[test]
    fn tui_input_ready_detects_heavy_angle_prompt() {
        // Claude Code v2.1+ renders the input prompt as ❯ (U+276F), not >.
        assert!(claude_tui_input_ready("❯ \n"));
        assert!(claude_tui_input_ready("  ❯ \n"));
        assert!(claude_tui_input_ready("│ ❯ \n"));
    }

    #[test]
    fn tui_input_ready_rejects_pane_without_prompt() {
        assert!(!claude_tui_input_ready("Loading…\n"));
        assert!(!claude_tui_input_ready("│ ▐▛███▜▌ Claude Code v2.1.175\n"));
        assert!(!claude_tui_input_ready(""));
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
