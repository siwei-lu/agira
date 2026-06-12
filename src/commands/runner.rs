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
    global_config::ClaudeRunnerConfig,
    orchestrator::{
        DEFAULT_ORCHESTRATOR_KICKOFF, DEFAULT_ORCHESTRATOR_TEMPLATE, assemble_orchestrator_prompt,
        load_template_override,
    },
    project::Project,
    runner::{Runner, RunnerStore, RunnerStoreError, is_lease_expired},
};

const DEFAULT_RUNNER_TYPE: &str = "claude-tmux";
const LOG_FILE_NAME: &str = "runner.log";
const CLAUDE_RUNNER_SETTINGS_FILE: &str = "claude-runner-settings.json";
const HEARTBEAT_STALENESS_THRESHOLD: Duration = Duration::minutes(10);
const TUI_READY_MAX_ATTEMPTS: usize = 60;
const TUI_READY_BACKOFF: StdDuration = StdDuration::from_millis(500);
#[cfg(not(test))]
const KICKOFF_SUBMIT_MAX_ATTEMPTS: usize = 3;
#[cfg(test)]
const KICKOFF_SUBMIT_MAX_ATTEMPTS: usize = 3;
#[cfg(not(test))]
const KICKOFF_SUBMIT_BACKOFF: StdDuration = StdDuration::from_millis(500);
#[cfg(test)]
const KICKOFF_SUBMIT_BACKOFF: StdDuration = StdDuration::from_millis(0);
#[cfg(not(test))]
const HOOK_READY_MAX_ATTEMPTS: usize = 60;
#[cfg(test)]
const HOOK_READY_MAX_ATTEMPTS: usize = 1;
#[cfg(not(test))]
const HOOK_READY_BACKOFF: StdDuration = StdDuration::from_millis(500);
#[cfg(test)]
const HOOK_READY_BACKOFF: StdDuration = StdDuration::from_millis(0);

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

    #[error("runner kickoff was not submitted")]
    KickoffNotSubmitted,

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

    #[error("failed to parse {path} as JSON")]
    SettingsJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerEventKind {
    Ready,
    Idle,
    Heartbeat,
}

impl RunnerEventKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "idle" => Some(Self::Idle),
            "heartbeat" => Some(Self::Heartbeat),
            _ => None,
        }
    }
}

pub trait Tmux {
    fn has_session(&mut self, session_name: &str) -> Result<bool, RunnerCommandError>;
    fn pane_alive(&mut self, session_name: &str) -> Result<bool, RunnerCommandError>;
    fn pane_is_claude(&mut self, session_name: &str) -> Result<bool, RunnerCommandError>;
    fn pane_process_group(&mut self, session_name: &str)
    -> Result<Option<i32>, RunnerCommandError>;
    fn new_session(
        &mut self,
        session_name: &str,
        launch_command: &str,
    ) -> Result<(), RunnerCommandError>;
    fn pipe_pane(&mut self, session_name: &str, log_path: &Path) -> Result<(), RunnerCommandError>;
    fn wait_for_ready(&mut self, session_name: &str) -> Result<(), RunnerCommandError>;
    fn capture_pane(&mut self, session_name: &str) -> Result<String, RunnerCommandError>;
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

    fn pane_is_claude(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
        let output = tmux_command(["capture-pane", "-p", "-t", session_name], "capture-pane")?;
        if !output.status.success() {
            return Ok(false);
        }
        let pane = String::from_utf8_lossy(&output.stdout);
        Ok(claude_tui_input_ready(&pane))
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
            let pane = self.capture_pane(session_name)?;
            if claude_tui_input_ready(&pane) {
                return Ok(());
            }
            if attempt + 1 < TUI_READY_MAX_ATTEMPTS {
                thread::sleep(TUI_READY_BACKOFF);
            }
        }

        Err(RunnerCommandError::RunnerNotReady)
    }

    fn capture_pane(&mut self, session_name: &str) -> Result<String, RunnerCommandError> {
        let output = tmux_command(["capture-pane", "-p", "-t", session_name], "capture-pane")?;
        if !output.status.success() {
            return Err(RunnerCommandError::TmuxFailed {
                action: "capture-pane",
                message: stderr_message(&output.stderr),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

pub fn run_runner_event(
    project: &Project,
    kind: RunnerEventKind,
    runner_id: Option<&str>,
) -> Result<(), RunnerCommandError> {
    runner_event(project, kind, runner_id, Utc::now())?;
    Ok(())
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

            // Pane is alive — check whether it is the claude TUI or a bare shell.
            if !tmux.pane_is_claude(&session_name)? {
                // Shell-prompt pane: treat as zombie and rebuild.
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

            // Idle runner (claude TUI alive, no current task): re-kick.
            let fresh_runner = store
                .get_runner(&runner.id)
                .cloned()
                .unwrap_or(runner.clone());
            if fresh_runner.current_task.is_none() {
                wait_for_hook_ready_or_tui_fallback(project, &fresh_runner.id, tmux)?;
                send_kickoff_and_verify(tmux, &session_name, DEFAULT_ORCHESTRATOR_KICKOFF)?;
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
    let hooks_settings = runner_hooks_settings();
    let settings_arg = claude_runner_settings_argument(
        &project.global_config.runner.claude,
        &hooks_settings,
        &runner_dir,
    )?;
    let launch_command = claude_launch_command(
        &project.global_config.runner.claude,
        &runner_id,
        &prompt,
        settings_arg.as_deref(),
        DEFAULT_ORCHESTRATOR_KICKOFF,
    );

    tmux.new_session(&session_name, &launch_command)?;
    tmux.pipe_pane(&session_name, &log_path)?;

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

fn runner_event(
    project: &Project,
    kind: RunnerEventKind,
    runner_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Option<Runner>, RunnerCommandError> {
    let Some(runner_id) = runner_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let mut store = RunnerStore::new(&project.state_dir)?;
    let runner = match kind {
        RunnerEventKind::Ready => store.record_ready(runner_id, now)?,
        RunnerEventKind::Idle => store.record_idle(runner_id, now)?,
        RunnerEventKind::Heartbeat => store.record_heartbeat(runner_id, now)?,
    };
    Ok(runner)
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
    } else if !tmux.pane_alive(&session_name)? || !tmux.pane_is_claude(&session_name)? {
        "zombie"
    } else if runner.current_task.is_none() {
        "idle"
    } else if is_lease_expired(runner.lease_expires_at.as_deref(), now)
        || is_heartbeat_stale(runner.last_heartbeat.as_deref(), now)
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

fn wait_for_hook_ready_or_tui_fallback<T: Tmux>(
    project: &Project,
    runner_id: &str,
    tmux: &mut T,
) -> Result<(), RunnerCommandError> {
    for attempt in 0..HOOK_READY_MAX_ATTEMPTS {
        let store = RunnerStore::new(&project.state_dir)?;
        if store
            .get_runner(runner_id)
            .map(runner_ready_from_hook_state)
            .unwrap_or(false)
        {
            return Ok(());
        }
        if attempt + 1 < HOOK_READY_MAX_ATTEMPTS {
            thread::sleep(HOOK_READY_BACKOFF);
        }
    }

    append_runner_log(
        project,
        "primary hook-based readiness path did not signal in time; falling back to tmux capture-pane readiness\n",
    )?;
    tmux.wait_for_ready(&session_name(project))
}

fn send_kickoff_and_verify<T: Tmux>(
    tmux: &mut T,
    session_name: &str,
    kickoff: &str,
) -> Result<(), RunnerCommandError> {
    tmux.send_keys(session_name, kickoff, true)?;

    for attempt in 0..KICKOFF_SUBMIT_MAX_ATTEMPTS {
        let pane = tmux.capture_pane(session_name)?;
        if claude_tui_input_cleared(&pane) {
            return Ok(());
        }
        if attempt + 1 < KICKOFF_SUBMIT_MAX_ATTEMPTS {
            thread::sleep(KICKOFF_SUBMIT_BACKOFF);
            tmux.send_keys(session_name, "Enter", false)?;
        }
    }

    Err(RunnerCommandError::KickoffNotSubmitted)
}

fn runner_ready_from_hook_state(runner: &Runner) -> bool {
    runner.current_task.is_none()
        && (runner.idle_since.is_some()
            || (runner.last_heartbeat.is_some() && runner.idle_since.is_none()))
}

fn append_runner_log(project: &Project, message: &str) -> Result<(), RunnerCommandError> {
    let log_path = runner_log_path(project);
    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| RunnerCommandError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|source| RunnerCommandError::Write {
            path: log_path.clone(),
            source,
        })?;
    file.write_all(message.as_bytes())
        .map_err(|source| RunnerCommandError::Write {
            path: log_path,
            source,
        })
}

fn claude_launch_command(
    config: &ClaudeRunnerConfig,
    runner_id: &str,
    prompt: &str,
    settings_arg: Option<&str>,
    kickoff: &str,
) -> String {
    let mut tokens = Vec::new();
    for (key, value) in &config.env {
        tokens.push(format!("{key}={}", shell_quote_string(value)));
    }
    tokens.push(format!("AGIRA_RUNNER_ID={}", shell_quote_string(runner_id)));
    tokens.push(shell_quote_string(&config.command));
    tokens.push("--model".to_owned());
    tokens.push(shell_quote_string(&config.model));
    tokens.push("--permission-mode".to_owned());
    tokens.push(shell_quote_string(&config.permission_mode));
    if let Some(settings_arg) = settings_arg {
        tokens.push("--settings".to_owned());
        tokens.push(shell_quote_string(settings_arg));
    }
    tokens.extend(config.extra_args.iter().map(|arg| shell_quote_string(arg)));
    tokens.push("--append-system-prompt".to_owned());
    tokens.push(shell_quote_string(prompt));
    tokens.push(shell_quote_string(kickoff));
    tokens.join(" ")
}

fn claude_runner_settings_argument(
    config: &ClaudeRunnerConfig,
    hooks_settings: &str,
    runner_dir: &Path,
) -> Result<Option<String>, RunnerCommandError> {
    let Some(settings_path) = config
        .settings_path
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
    else {
        return Ok(Some(hooks_settings.to_owned()));
    };

    // Claude Code treats --settings as one scalar file-or-JSON value. When the
    // user supplies a settings file, Agira writes one merged overlay so the
    // user's settings and the runner lifecycle hooks are both active.
    let user_settings =
        fs::read_to_string(settings_path).map_err(|source| RunnerCommandError::Read {
            path: settings_path.clone(),
            source,
        })?;
    let user_settings = serde_json::from_str(&user_settings).map_err(|source| {
        RunnerCommandError::SettingsJson {
            path: settings_path.clone(),
            source,
        }
    })?;
    let hooks_settings = serde_json::from_str(hooks_settings).map_err(|source| {
        RunnerCommandError::SettingsJson {
            path: PathBuf::from("<agira runner hooks>"),
            source,
        }
    })?;
    let merged = merge_runner_hooks_settings(user_settings, hooks_settings);

    fs::create_dir_all(runner_dir).map_err(|source| RunnerCommandError::Write {
        path: runner_dir.to_path_buf(),
        source,
    })?;
    let overlay_path = runner_dir.join(CLAUDE_RUNNER_SETTINGS_FILE);
    let overlay_json = serde_json::to_string_pretty(&merged).expect("settings json serializes");
    fs::write(&overlay_path, overlay_json).map_err(|source| RunnerCommandError::Write {
        path: overlay_path.clone(),
        source,
    })?;

    Ok(Some(overlay_path.to_string_lossy().into_owned()))
}

fn merge_runner_hooks_settings(
    mut user_settings: serde_json::Value,
    hooks_settings: serde_json::Value,
) -> serde_json::Value {
    let Some(user_object) = user_settings.as_object_mut() else {
        return hooks_settings;
    };
    let Some(hooks_object) = hooks_settings
        .get("hooks")
        .and_then(serde_json::Value::as_object)
    else {
        return user_settings;
    };

    let user_hooks = user_object
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(user_hooks_object) = user_hooks.as_object_mut() else {
        *user_hooks = serde_json::Value::Object(serde_json::Map::new());
        let Some(user_hooks_object) = user_hooks.as_object_mut() else {
            return user_settings;
        };
        for (event, hooks) in hooks_object {
            user_hooks_object.insert(event.clone(), hooks.clone());
        }
        return user_settings;
    };

    for (event, hooks) in hooks_object {
        match (user_hooks_object.get_mut(event), hooks.as_array()) {
            (Some(serde_json::Value::Array(existing)), Some(agira_hooks)) => {
                existing.extend(agira_hooks.iter().cloned());
            }
            _ => {
                user_hooks_object.insert(event.clone(), hooks.clone());
            }
        }
    }

    user_settings
}

fn runner_hooks_settings() -> String {
    serde_json::json!({
        "hooks": {
            "SessionStart": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": "agira runner event ready --runner \"$AGIRA_RUNNER_ID\""
                        }
                    ]
                }
            ],
            "Stop": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": "agira runner event idle --runner \"$AGIRA_RUNNER_ID\""
                        }
                    ]
                }
            ],
            "PostToolUse": [
                {
                    "matcher": "*",
                    "hooks": [
                        {
                            "type": "command",
                            "command": "agira runner event heartbeat --runner \"$AGIRA_RUNNER_ID\""
                        }
                    ]
                }
            ]
        }
    })
    .to_string()
}

fn claude_tui_input_ready(pane: &str) -> bool {
    // Runner readiness is content-based on the Claude TUI prompt, not on the
    // pane process name. A configured wrapper command is supported as long as it
    // ultimately renders the Claude Code TUI prompt.
    pane.lines()
        .any(|line| prompt_line_after_glyph(line).is_some())
}

fn claude_tui_input_cleared(pane: &str) -> bool {
    pane.lines()
        .any(|line| prompt_line_after_glyph(line).is_some_and(|input| input.trim().is_empty()))
}

fn prompt_line_after_glyph(line: &str) -> Option<&str> {
    let line = line
        .trim_start()
        .trim_start_matches(|ch: char| !ch.is_ascii() && ch != '❯')
        .trim_start();
    line.strip_prefix('>').or_else(|| line.strip_prefix('❯'))
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
    use std::{collections::BTreeMap, collections::VecDeque, fs, path::Path, path::PathBuf};

    use chrono::{DateTime, Duration, Utc};

    use crate::core::{
        config::{Config, ConfigError, PhaseDef},
        global_config::{ClaudeRunnerConfig, GlobalConfig},
        hooks::HookConfig,
        orchestrator::DEFAULT_ORCHESTRATOR_KICKOFF,
        project::Project,
        runner::RunnerStore,
    };

    use super::{
        CLAUDE_RUNNER_SETTINGS_FILE, OUTPUT_CAPTURE, RunnerCommandError, RunnerEventKind, Tmux,
        attach_runner, claude_launch_command, claude_runner_settings_argument,
        claude_tui_input_cleared, claude_tui_input_ready, format_status_output, run_runner_logs,
        runner_event, runner_hooks_settings, start_runner, status_runner, stop_runner,
    };

    struct RecordingTmux {
        live: bool,
        pane_alive: bool,
        pane_is_claude: bool,
        pane_pgid: Option<i32>,
        ready: bool,
        pane_snapshots: VecDeque<String>,
        calls: Vec<Vec<String>>,
    }

    impl Default for RecordingTmux {
        fn default() -> Self {
            Self {
                live: false,
                pane_alive: false,
                pane_is_claude: true,
                pane_pgid: Some(4242),
                ready: true,
                pane_snapshots: VecDeque::new(),
                calls: Vec::new(),
            }
        }
    }

    impl RecordingTmux {
        fn live() -> Self {
            Self {
                live: true,
                pane_alive: true,
                pane_is_claude: true,
                pane_pgid: Some(4242),
                ready: true,
                pane_snapshots: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn zombie() -> Self {
            Self {
                live: true,
                pane_alive: false,
                pane_is_claude: true,
                pane_pgid: Some(4242),
                ready: true,
                pane_snapshots: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn shell_pane() -> Self {
            Self {
                live: true,
                pane_alive: true,
                pane_is_claude: false,
                pane_pgid: Some(4242),
                ready: true,
                pane_snapshots: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn never_ready() -> Self {
            Self {
                live: false,
                pane_alive: false,
                pane_is_claude: true,
                pane_pgid: Some(4242),
                ready: false,
                pane_snapshots: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn with_pane_snapshots(mut self, snapshots: &[&str]) -> Self {
            self.pane_snapshots = snapshots
                .iter()
                .map(|snapshot| (*snapshot).to_owned())
                .collect();
            self
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

        fn pane_is_claude(&mut self, session_name: &str) -> Result<bool, RunnerCommandError> {
            self.calls.push(vec![
                "pane-is-claude".into(),
                "-t".into(),
                session_name.into(),
            ]);
            Ok(self.pane_is_claude)
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

        fn capture_pane(&mut self, session_name: &str) -> Result<String, RunnerCommandError> {
            self.calls.push(vec![
                "capture-pane".into(),
                "-p".into(),
                "-t".into(),
                session_name.into(),
            ]);
            Ok(self
                .pane_snapshots
                .pop_front()
                .unwrap_or_else(|| "│ > \n".to_owned()))
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
        assert!(tmux.calls[1][4].contains("' 'claude' --model 'sonnet' --permission-mode 'auto'"));
        assert!(tmux.calls[1][4].contains("agira-orchestrator-template-v2"));
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
        assert!(tmux.calls[1][4].contains(" --settings '"));
        assert!(tmux.calls[1][4].contains("agira runner event ready --runner"));
        assert!(tmux.calls[1][4].contains("agira runner event idle --runner"));
        assert!(tmux.calls[1][4].contains("agira runner event heartbeat --runner"));
        assert!(tmux.calls[1][4].contains("agira task todo --runner \"$AGIRA_RUNNER_ID\""));
        assert!(!tmux.calls.iter().any(|call| call[0] == "wait-for-ready"));
        assert!(!tmux.calls.iter().any(|call| call[0] == "send-keys"));

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
        assert!(!launch_command.contains("agira-orchestrator-template-v2"));
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
        assert!(launch_command.contains("agira-orchestrator-template-v2"));
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
    fn start_does_not_wait_or_cleanup_when_runner_tui_is_not_ready_on_cold_start() {
        let (_dir, project) = project();
        let mut tmux = RecordingTmux::never_ready();

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

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
                vec!["pane-pgid", "-t", "agira-runner-repo"],
            ]
        );
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner(&output.runner_id).is_some());
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
            &tmux.calls[..5],
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
        // Runner has a fresh lease (current_task = Some), so it is classified "busy"
        // and must not trigger a re-kick send-keys.
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
        store
            .acquire_lease(
                "runner-existing",
                "task-busy",
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
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert_eq!(output.runner_id, "runner-existing");
        assert!(output.already_running);
        assert!(!tmux.calls.iter().any(|call| call[0] == "send-keys"));
        assert_eq!(
            tmux.calls,
            vec![
                vec!["has-session", "-t", "agira-runner-repo"],
                vec!["pane-alive", "-t", "agira-runner-repo"],
                vec!["pane-is-claude", "-t", "agira-runner-repo"],
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
    fn runner_hooks_settings_wires_expected_lifecycle_events() {
        let settings = runner_hooks_settings();
        let parsed: serde_json::Value =
            serde_json::from_str(&settings).expect("settings are valid json");

        assert_eq!(
            parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "agira runner event ready --runner \"$AGIRA_RUNNER_ID\""
        );
        assert!(parsed["hooks"]["SessionStart"][0].get("matcher").is_none());
        assert_eq!(
            parsed["hooks"]["Stop"][0]["hooks"][0]["command"],
            "agira runner event idle --runner \"$AGIRA_RUNNER_ID\""
        );
        assert!(parsed["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(parsed["hooks"]["PostToolUse"][0]["matcher"], "*");
        assert_eq!(
            parsed["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "agira runner event heartbeat --runner \"$AGIRA_RUNNER_ID\""
        );
    }

    #[test]
    fn claude_launch_command_uses_default_config_shape() {
        let hooks_settings = runner_hooks_settings();
        let command = claude_launch_command(
            &ClaudeRunnerConfig::default(),
            "runner-123",
            "system prompt",
            Some(&hooks_settings),
            "kickoff now",
        );

        assert!(command.starts_with(
            "AGIRA_RUNNER_ID='runner-123' 'claude' --model 'sonnet' --permission-mode 'auto'"
        ));
        assert!(!command.contains("--settings ''"));
        assert!(command.contains(" --settings '{\"hooks\":"));
        assert!(command.contains(" --append-system-prompt 'system prompt' 'kickoff now'"));
    }

    #[test]
    fn claude_launch_command_uses_full_config_in_token_order() {
        let mut env = BTreeMap::new();
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            "https://example.test".to_owned(),
        );
        env.insert("HTTPS_PROXY".to_owned(), "http://127.0.0.1:8080".to_owned());
        let config = ClaudeRunnerConfig {
            command: "/opt/bin/claude-wrapper".to_owned(),
            model: "opus".to_owned(),
            permission_mode: "dontAsk".to_owned(),
            settings_path: Some(PathBuf::from("/tmp/claude runner/settings.json")),
            extra_args: vec!["--debug".to_owned(), "category='hooks'".to_owned()],
            env,
        };

        let command = claude_launch_command(
            &config,
            "runner-xyz",
            "prompt with ' quote",
            Some("/tmp/agira-runner/claude-runner-settings.json"),
            "claim next",
        );

        assert!(command.starts_with(
            "ANTHROPIC_BASE_URL='https://example.test' HTTPS_PROXY='http://127.0.0.1:8080' AGIRA_RUNNER_ID='runner-xyz' '/opt/bin/claude-wrapper' --model 'opus' --permission-mode 'dontAsk' --settings '/tmp/agira-runner/claude-runner-settings.json' '--debug' 'category='\\''hooks'\\'''"
        ));
        assert_eq!(command.matches("--settings").count(), 1);
        assert!(
            command.ends_with(" --append-system-prompt 'prompt with '\\'' quote' 'claim next'")
        );
    }

    #[test]
    fn claude_launch_command_omits_user_settings_path_when_unset_or_empty() {
        let hooks_settings = runner_hooks_settings();
        let command = claude_launch_command(
            &ClaudeRunnerConfig {
                settings_path: Some(PathBuf::new()),
                ..ClaudeRunnerConfig::default()
            },
            "runner-123",
            "prompt",
            Some(&hooks_settings),
            "kickoff",
        );

        assert_eq!(command.matches("--settings").count(), 1);
        assert!(command.contains(" --settings '{\"hooks\":"));
    }

    #[test]
    fn claude_launch_command_preserves_extra_arg_order_before_hooks_overlay() {
        let config = ClaudeRunnerConfig {
            extra_args: vec!["--first".to_owned(), "--second=value".to_owned()],
            ..ClaudeRunnerConfig::default()
        };

        let command = claude_launch_command(
            &config,
            "runner-123",
            "prompt",
            Some("{\"hooks\":{}}"),
            "kickoff",
        );

        assert!(command.contains(
            "--permission-mode 'auto' --settings '{\"hooks\":{}}' '--first' '--second=value'"
        ));
    }

    #[test]
    fn claude_launch_command_sorts_env_and_runner_id_wins() {
        let mut env = BTreeMap::new();
        env.insert("ZZZ".to_owned(), "z value".to_owned());
        env.insert("AAA".to_owned(), "a'value $(rm -rf /)".to_owned());
        env.insert("AGIRA_RUNNER_ID".to_owned(), "user-runner".to_owned());

        let command = claude_launch_command(
            &ClaudeRunnerConfig {
                env,
                ..ClaudeRunnerConfig::default()
            },
            "real-runner",
            "prompt",
            Some(&runner_hooks_settings()),
            "kickoff",
        );

        assert!(command.starts_with(
            "AAA='a'\\''value $(rm -rf /)' AGIRA_RUNNER_ID='user-runner' ZZZ='z value' AGIRA_RUNNER_ID='real-runner'"
        ));
    }

    #[test]
    fn claude_runner_settings_argument_merges_hooks_with_user_settings_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user_settings_path = dir.path().join("user-settings.json");
        fs::write(
            &user_settings_path,
            r#"{
  "allowedTools": ["Bash(git *)"],
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo user stop"
          }
        ]
      }
    ]
  }
}"#,
        )
        .expect("write user settings");
        let runner_dir = dir.path().join("runner");

        let settings_arg = claude_runner_settings_argument(
            &ClaudeRunnerConfig {
                settings_path: Some(user_settings_path.clone()),
                ..ClaudeRunnerConfig::default()
            },
            &runner_hooks_settings(),
            &runner_dir,
        )
        .expect("settings arg")
        .expect("settings arg present");

        let overlay_path = runner_dir.join(CLAUDE_RUNNER_SETTINGS_FILE);
        assert_eq!(settings_arg, overlay_path.to_string_lossy());
        let overlay: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&overlay_path).expect("read overlay settings"),
        )
        .expect("overlay json");

        assert_eq!(overlay["allowedTools"][0], "Bash(git *)");
        assert_eq!(
            overlay["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo user stop"
        );
        assert_eq!(
            overlay["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "agira runner event ready --runner \"$AGIRA_RUNNER_ID\""
        );
        assert_eq!(
            overlay["hooks"]["Stop"][1]["hooks"][0]["command"],
            "agira runner event idle --runner \"$AGIRA_RUNNER_ID\""
        );
        assert_eq!(
            overlay["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "agira runner event heartbeat --runner \"$AGIRA_RUNNER_ID\""
        );
    }

    #[test]
    fn runner_event_command_records_ready_idle_and_heartbeat() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-event",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);

        runner_event(
            &project,
            RunnerEventKind::Idle,
            Some("runner-event"),
            fixed_now(),
        )
        .expect("record idle");
        runner_event(
            &project,
            RunnerEventKind::Ready,
            Some("runner-event"),
            fixed_now() + Duration::seconds(1),
        )
        .expect("record ready");
        runner_event(
            &project,
            RunnerEventKind::Heartbeat,
            Some("runner-event"),
            fixed_now() + Duration::seconds(2),
        )
        .expect("record heartbeat");

        let store = RunnerStore::new(&project.state_dir).expect("open store");
        let runner = store.get_runner("runner-event").expect("runner exists");
        assert!(runner.idle_since.is_none());
        assert_eq!(
            runner.last_heartbeat.as_deref(),
            Some("2026-06-11T12:00:02+00:00")
        );
    }

    #[test]
    fn runner_event_without_resolved_runner_is_noop() {
        let (_dir, project) = project();

        let result = runner_event(&project, RunnerEventKind::Ready, None, fixed_now())
            .expect("missing runner is non-fatal");

        assert!(result.is_none());
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.registry().runners.is_empty());
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
    fn tui_input_ready_is_independent_of_binary_path() {
        assert!(claude_tui_input_ready("/opt/bin/claude-wrapper\n│ > \n"));
    }

    #[test]
    fn tui_input_cleared_requires_empty_prompt_after_glyph() {
        assert!(claude_tui_input_cleared("/opt/bin/claude-wrapper\n│ > \n"));
        assert!(claude_tui_input_cleared("prefix\n❯ \n"));
        assert!(!claude_tui_input_cleared("prefix\n│ > kickoff now\n"));
        assert!(!claude_tui_input_cleared("prefix\n❯ kickoff now\n"));
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

    // ── task-127: idle-runner re-kick and non-claude pane detection ────────────

    #[test]
    fn start_rekicks_idle_live_claude_session() {
        // Runner registered, claude pane alive, no current_task → re-kick path.
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        // No lease acquired → current_task is None.
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.runner_id, "runner-idle");
        assert!(output.already_running);

        // Must have called wait-for-ready followed by two send-keys (text + Enter).
        let wait_idx = tmux
            .calls
            .iter()
            .position(|c| c[0] == "wait-for-ready")
            .expect("wait-for-ready not called");
        assert_eq!(tmux.calls[wait_idx + 1][0], "send-keys");
        assert_eq!(tmux.calls[wait_idx + 2][0], "send-keys");
        assert_eq!(tmux.calls[wait_idx + 2][3], "Enter");

        // Store must still hold the same runner_id — no deregister/cold-start.
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-idle").is_some());
    }

    #[test]
    fn start_rekick_retries_standalone_enter_when_initial_enter_is_swallowed() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle-swallowed-enter",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::live()
            .with_pane_snapshots(&[&format!("│ > {DEFAULT_ORCHESTRATOR_KICKOFF}\n"), "│ > \n"]);

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.runner_id, "runner-idle-swallowed-enter");
        assert!(output.already_running);
        let send_keys: Vec<&Vec<String>> = tmux
            .calls
            .iter()
            .filter(|call| call[0] == "send-keys")
            .collect();
        assert_eq!(send_keys.len(), 3);
        assert_eq!(send_keys[0][3], DEFAULT_ORCHESTRATOR_KICKOFF);
        assert_eq!(send_keys[1][3], "Enter");
        assert_eq!(send_keys[2][3], "Enter");
        let capture_count = tmux
            .calls
            .iter()
            .filter(|call| call[0] == "capture-pane")
            .count();
        assert_eq!(capture_count, 2);
    }

    #[test]
    fn start_rekick_does_not_retry_when_initial_submit_clears_input() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle-first-submit",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::live().with_pane_snapshots(&["│ > \n"]);

        let output = start_runner(&project, None, &mut tmux, fixed_now()).expect("start runner");

        assert_eq!(output.runner_id, "runner-idle-first-submit");
        assert!(output.already_running);
        let send_keys: Vec<&Vec<String>> = tmux
            .calls
            .iter()
            .filter(|call| call[0] == "send-keys")
            .collect();
        assert_eq!(send_keys.len(), 2);
        assert_eq!(send_keys[0][3], DEFAULT_ORCHESTRATOR_KICKOFF);
        assert_eq!(send_keys[1][3], "Enter");
    }

    #[test]
    fn start_rekick_returns_error_when_kickoff_submission_never_clears() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle-never-submits",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::live().with_pane_snapshots(&[
            &format!("│ > {DEFAULT_ORCHESTRATOR_KICKOFF}\n"),
            &format!("│ > {DEFAULT_ORCHESTRATOR_KICKOFF}\n"),
            &format!("│ > {DEFAULT_ORCHESTRATOR_KICKOFF}\n"),
        ]);

        let result = start_runner(&project, None, &mut tmux, fixed_now());

        assert!(matches!(
            result,
            Err(RunnerCommandError::KickoffNotSubmitted)
        ));
        let send_keys: Vec<&Vec<String>> = tmux
            .calls
            .iter()
            .filter(|call| call[0] == "send-keys")
            .collect();
        assert_eq!(send_keys.len(), 4);
        assert_eq!(send_keys[0][3], DEFAULT_ORCHESTRATOR_KICKOFF);
        assert_eq!(send_keys[1][3], "Enter");
        assert_eq!(send_keys[2][3], "Enter");
        assert_eq!(send_keys[3][3], "Enter");
    }

    #[test]
    fn start_rekick_failure_does_not_deregister_idle_session() {
        // If wait_for_ready returns RunnerNotReady the error propagates but
        // the runner stays registered and the session is NOT killed.
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle-fail",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux {
            live: true,
            pane_alive: true,
            pane_is_claude: true,
            ready: false,
            ..Default::default()
        };

        let result = start_runner(&project, None, &mut tmux, fixed_now());

        assert!(matches!(result, Err(RunnerCommandError::RunnerNotReady)));
        assert!(!tmux.calls.iter().any(|c| c[0] == "kill-session"));
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-idle-fail").is_some());
    }

    #[test]
    fn start_busy_live_claude_session_triggers_no_send_keys() {
        // Runner has a fresh lease (current_task = Some) → must not send-keys.
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-busy",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-busy",
                "task-active",
                Duration::minutes(10),
                fixed_now(),
            )
            .expect("acquire lease");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let output = start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert_eq!(output.runner_id, "runner-busy");
        assert!(output.already_running);
        assert!(!tmux.calls.iter().any(|c| c[0] == "send-keys"));
        assert!(!tmux.calls.iter().any(|c| c[0] == "wait-for-ready"));
    }

    #[test]
    fn start_rebuilds_shell_pane_zombie_and_cold_starts() {
        // Session alive, pane alive, but pane is a shell (not claude TUI).
        // Expected: kill → reap → deregister → cold-start → new runner_id.
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-shell",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::shell_pane();

        let output = start_runner(
            &project,
            None,
            &mut tmux,
            fixed_now() + Duration::minutes(1),
        )
        .expect("start runner");

        assert_ne!(output.runner_id, "runner-shell");
        assert!(!output.already_running);
        assert!(tmux.calls.iter().any(|c| c[0] == "kill-session"));

        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-shell").is_none());
        assert!(store.get_runner(&output.runner_id).is_some());
    }

    #[test]
    fn status_reports_idle_for_live_claude_pane_with_no_current_task() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-idle-status",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        // No lease → current_task is None.
        drop(store);
        let mut tmux = RecordingTmux::live();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(status.liveness, "idle");
        let output = format_status_output(&status);
        assert!(output.contains("liveness: idle"));
    }

    #[test]
    fn status_reports_zombie_for_live_session_with_shell_pane() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-shell-status",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        drop(store);
        let mut tmux = RecordingTmux::shell_pane();

        let status = status_runner(&project, &mut tmux, fixed_now()).expect("status runner");

        assert_eq!(status.liveness, "zombie");
        let output = format_status_output(&status);
        assert!(output.contains("liveness: zombie"));
        // Must NOT mutate registry or kill session.
        assert!(!tmux.calls.iter().any(|c| c[0] == "kill-session"));
        let store = RunnerStore::new(&project.state_dir).expect("open store");
        assert!(store.get_runner("runner-shell-status").is_some());
    }

    #[test]
    fn status_reports_live_for_busy_fresh_runner_with_claude_pane() {
        let (_dir, project) = project();
        let mut store = RunnerStore::new(&project.state_dir).expect("open store");
        store
            .register_at(
                "runner-live-status",
                "claude-tmux",
                "agira-runner-repo",
                fixed_now(),
            )
            .expect("register runner");
        store
            .acquire_lease(
                "runner-live-status",
                "task-live",
                Duration::minutes(10),
                fixed_now(),
            )
            .expect("acquire lease");
        drop(store);
        let mut tmux = RecordingTmux::live();

        let status =
            status_runner(&project, &mut tmux, fixed_now() + Duration::minutes(1)).expect("status");

        assert_eq!(status.liveness, "live");
        assert!(format_status_output(&status).contains("liveness: live"));
    }
}
