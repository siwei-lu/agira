use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use chrono::{Duration, Utc};
use tempfile::TempDir;

const NOT_INITIALIZED_ERROR: &str =
    "error: project not initialized — run \"agira init\" to set up this repository\n";

fn agira(home: &Path, repo: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agira"));
    command.current_dir(repo).env("HOME", home);
    command
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn setup_uninitialized_repo() -> (TempDir, TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join("Task CLI Repo");

    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    (home, workspace, repo)
}

fn project_state_dir(home: &Path) -> PathBuf {
    home.join(".agira").join("task-cli-repo")
}

fn project_state_dir_for(home: &Path, slug: &str) -> PathBuf {
    home.join(".agira").join(slug)
}

#[test]
fn task_subcommands_require_initialized_project() {
    let cases: &[&[&str]] = &[
        &["task", "list"],
        &["task", "todo"],
        &["task", "todo", "--artifact", "done"],
        &["task", "fail", "task-001", "--reason", "failed"],
        &["task", "block", "task-001", "--reason", "blocked"],
        &["task", "unblock", "task-001"],
        &["task", "add", "new task"],
        &["task", "update", "task-001", "--title", "renamed task"],
    ];

    for args in cases {
        let (home, _workspace, repo) = setup_uninitialized_repo();

        let output = run(agira(home.path(), &repo).args(*args));

        assert_eq!(output.status.code(), Some(1), "args: {args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "",
            "args: {args:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            NOT_INITIALIZED_ERROR,
            "args: {args:?}"
        );
        assert!(!project_state_dir(home.path()).exists(), "args: {args:?}");
    }
}

#[test]
fn task_status_is_unrecognized_subcommand() {
    let (home, _workspace, repo) = setup_uninitialized_repo();

    let output = run(agira(home.path(), &repo).args(["task", "status"]));

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand 'status'"));
}

fn setup_initialized_repo(name: &str) -> (TempDir, TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join(name);
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    let output = run(agira(home.path(), &repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching,implementing,reviewing,done",
    ]));
    assert!(
        output.status.success(),
        "init failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    (home, workspace, repo)
}

/// Full roundtrip: add a task, read the prompt, parse the `## Completion` command,
/// run it verbatim, and verify the task advanced to the next phase.
#[test]
fn todo_completion_receipt_command_advances_task_to_next_phase() {
    let (home, _workspace, repo) = setup_initialized_repo("CAS Roundtrip Repo");

    // Add a task that starts in the "enriching" phase
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "roundtrip task",
        "--description",
        "A task for the CAS roundtrip integration test",
        "--phase",
        "enriching",
    ]));
    assert!(output.status.success(), "task add failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Extract the task ID from the output (format: "added task-001: <title>")
    let task_id = stdout
        .split_whitespace()
        .nth(1)
        .expect("task id in add output")
        .trim_end_matches(':')
        .to_owned();

    // Run `agira task todo` to obtain the prompt
    let output = run(agira(home.path(), &repo).args(["task", "todo", "--task", &task_id]));
    assert!(
        output.status.success(),
        "task todo failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let prompt = String::from_utf8_lossy(&output.stdout);

    // Parse the `## Completion` section to verify it contains the expected receipt command.
    // The format is: `agira task todo --task <id> --from <phase> --artifact "<evidence>"`
    let completion_line = prompt
        .lines()
        .find(|line| line.contains("agira task todo --task") && line.contains("--from"))
        .unwrap_or_else(|| panic!("## Completion receipt command not found in prompt:\n{prompt}"));

    // Verify the completion line embeds the correct task ID and current phase
    assert!(
        completion_line.contains(&task_id),
        "completion line must embed task ID {task_id}; got:\n{completion_line}"
    );
    assert!(
        completion_line.contains("--from enriching"),
        "completion line must embed --from enriching; got:\n{completion_line}"
    );

    // Run the actual receipt command by constructing it from parsed values
    // (avoids shell-quoting complexity when the artifact text has spaces)
    let output = run(agira(home.path(), &repo).args([
        "task",
        "todo",
        "--task",
        &task_id,
        "--from",
        "enriching",
        "--artifact",
        "roundtrip complete",
    ]));
    assert!(
        output.status.success(),
        "receipt command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the task is now in the next phase (implementing)
    let output = run(agira(home.path(), &repo).args(["task", "list", "--json", &task_id]));
    assert!(output.status.success(), "task list failed");
    let json_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        json_str.contains("\"implementing\""),
        "task should be in implementing phase after roundtrip; json:\n{json_str}"
    );
}

/// `agira task todo --help` must list the new --task and --from flags.
#[test]
fn task_todo_help_lists_task_and_from_flags() {
    let output = run(Command::new(env!("CARGO_BIN_EXE_agira")).args(["task", "todo", "--help"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--task"),
        "expected --task in 'agira task todo --help', got:\n{stdout}"
    );
    assert!(
        stdout.contains("--from"),
        "expected --from in 'agira task todo --help', got:\n{stdout}"
    );
    assert!(
        stdout.contains("<id>"),
        "expected <id> placeholder in 'agira task todo --help', got:\n{stdout}"
    );
    assert!(
        stdout.contains("<phase>"),
        "expected <phase> placeholder in 'agira task todo --help', got:\n{stdout}"
    );
}

/// Legacy path (--artifact without --task) emits deprecation warning on stderr.
#[test]
fn legacy_path_emits_deprecation_warning_on_stderr() {
    let (home, _workspace, repo) = setup_initialized_repo("Legacy Warning Repo");

    // Add a task in the enriching phase
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "legacy task",
        "--description",
        "A task for legacy deprecation warning test",
        "--phase",
        "enriching",
    ]));
    assert!(output.status.success(), "task add failed");

    // Run the legacy path
    let output = run(agira(home.path(), &repo).args(["task", "todo", "--artifact", "legacy done"]));
    assert!(
        output.status.success(),
        "legacy task todo failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--artifact without --task is deprecated"),
        "expected deprecation warning in stderr, got:\n{stderr}"
    );

    // stdout must not contain the warning
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("--artifact without --task is deprecated"),
        "deprecation warning must not appear in stdout, got:\n{stdout}"
    );
}

/// CAS mismatch via CLI: --task + --from where task is already in a different phase
/// must exit 1 with a clear error and must not advance the task.
#[test]
fn cli_cas_mismatch_exits_1_without_mutating_state() {
    let (home, _workspace, repo) = setup_initialized_repo("CAS Mismatch CLI Repo");

    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "cas task",
        "--description",
        "CAS mismatch test task",
        "--phase",
        "implementing",
    ]));
    assert!(output.status.success(), "task add failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "added task-001: cas task"
    let task_id = stdout
        .split_whitespace()
        .nth(1)
        .expect("task id")
        .trim_end_matches(':')
        .to_owned();

    // Submit with --from enriching but task is in implementing → CAS mismatch
    let output = run(agira(home.path(), &repo).args([
        "task",
        "todo",
        "--task",
        &task_id,
        "--from",
        "enriching",
        "--artifact",
        "done",
    ]));
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit code 1 for CAS mismatch"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already advanced past"),
        "expected 'already advanced past' in stderr, got:\n{stderr}"
    );

    // Task must still be in implementing
    let output = run(agira(home.path(), &repo).args(["task", "list", "--json", &task_id]));
    let json_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        json_str.contains("\"implementing\""),
        "task state must not change on CAS mismatch; json:\n{json_str}"
    );
}

#[test]
fn task_list_warns_about_recent_hook_failures_for_table_output() {
    let repo_name = "Recent Hook Failure Repo";
    let (home, _workspace, repo) = setup_initialized_repo(repo_name);
    let state_dir = project_state_dir_for(home.path(), "recent-hook-failure-repo");

    run(agira(home.path(), &repo).args(["task", "add", "recent hook failure task"]));
    let hooks_log = state_dir.join("hooks.log");
    fs::write(
        &hooks_log,
        format!(
            "{}\ttask_added\ttask-001\tbad-command\tspawn_error: command not found\n",
            Utc::now().to_rfc3339()
        ),
    )
    .unwrap();

    let output = run(agira(home.path(), &repo).args(["task", "list"]));

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "warning: recent hook failures in {}\n",
            hooks_log.canonicalize().unwrap().display()
        )
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("task-001"));
}

#[test]
fn task_list_json_warns_about_recent_hook_failures_only_on_stderr() {
    let repo_name = "Recent Hook Failure Json Repo";
    let (home, _workspace, repo) = setup_initialized_repo(repo_name);
    let state_dir = project_state_dir_for(home.path(), "recent-hook-failure-json-repo");

    run(agira(home.path(), &repo).args(["task", "add", "recent json hook failure task"]));
    let hooks_log = state_dir.join("hooks.log");
    fs::write(
        &hooks_log,
        format!(
            "{}\ttask_added\ttask-001\tbad-command\tspawn_error: command not found\n",
            Utc::now().to_rfc3339()
        ),
    )
    .unwrap();

    let output = run(agira(home.path(), &repo).args(["task", "list", "--json"]));

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "warning: recent hook failures in {}\n",
            hooks_log.canonicalize().unwrap().display()
        )
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("warning:"));
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap();
}

#[test]
fn task_list_does_not_warn_when_hook_failures_are_old() {
    let repo_name = "Old Hook Failure Repo";
    let (home, _workspace, repo) = setup_initialized_repo(repo_name);
    let state_dir = project_state_dir_for(home.path(), "old-hook-failure-repo");

    run(agira(home.path(), &repo).args(["task", "add", "old hook failure task"]));
    fs::write(
        state_dir.join("hooks.log"),
        format!(
            "{}\ttask_added\ttask-001\tbad-command\tspawn_error: command not found\n",
            (Utc::now() - Duration::hours(25)).to_rfc3339()
        ),
    )
    .unwrap();

    let output = run(agira(home.path(), &repo).args(["task", "list"]));

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert!(String::from_utf8_lossy(&output.stdout).contains("task-001"));
}

#[test]
fn task_list_does_not_warn_when_hook_failure_log_is_absent() {
    let (home, _workspace, repo) = setup_initialized_repo("Absent Hook Failure Repo");

    run(agira(home.path(), &repo).args(["task", "add", "absent hook failure task"]));

    let output = run(agira(home.path(), &repo).args(["task", "list"]));

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert!(String::from_utf8_lossy(&output.stdout).contains("task-001"));
}
