use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
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

fn run_with_stdin(command: &mut Command, stdin: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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
fn runner_subcommands_require_initialized_project() {
    let cases: &[&[&str]] = &[
        &["runner", "start"],
        &["runner", "stop"],
        &["runner", "status"],
        &["runner", "attach"],
        &["runner", "logs"],
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

#[test]
fn task_add_batch_file_creates_alias_wired_dag() {
    let (home, _workspace, repo) = setup_initialized_repo("Batch Add Repo");
    let manifest = repo.join("tasks.toml");
    fs::write(
        &manifest,
        r#"
[[task]]
id = "root"
title = "root task"
description = "first"

[[task]]
id = "child"
title = "child task"
depends_on = ["root"]

[[task]]
title = "leaf task"
depends_on = ["child"]
"#,
    )
    .unwrap();

    let output = run(agira(home.path(), &repo).args([
        "task",
        "add-batch",
        "--file",
        manifest.to_str().unwrap(),
    ]));

    assert!(
        output.status.success(),
        "add-batch failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("root -> task-001"));
    assert!(stdout.contains("child -> task-002"));
    assert!(stdout.contains("added task-003: leaf task"));

    let tasks_json =
        fs::read_to_string(project_state_dir_for(home.path(), "batch-add-repo").join("tasks.json"))
            .unwrap();
    assert!(tasks_json.contains("\"id\": \"task-001\""));
    assert!(tasks_json.contains("\"dependencies\": [\n        \"task-001\"\n      ]"));
    assert!(tasks_json.contains("\"dependencies\": [\n        \"task-002\"\n      ]"));
}

#[test]
fn task_add_batch_stdin_dry_run_json_writes_nothing() {
    let (home, _workspace, repo) = setup_initialized_repo("Batch Dry Run Repo");
    let manifest = r#"
[[task]]
id = "a"
title = "dry root"

[[task]]
id = "b"
title = "dry child"
depends_on = ["a"]
"#;

    let output = run_with_stdin(
        agira(home.path(), &repo).args(["task", "add-batch", "--stdin", "--dry-run", "--json"]),
        manifest,
    );

    assert!(
        output.status.success(),
        "dry-run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["tasks"][0]["alias"], "a");
    assert_eq!(value["tasks"][1]["id"], "task-002");
    assert!(
        !project_state_dir_for(home.path(), "batch-dry-run-repo")
            .join("tasks.json")
            .exists()
    );
}

#[test]
fn task_add_batch_reports_all_preflight_errors_and_writes_nothing() {
    let (home, _workspace, repo) = setup_initialized_repo("Batch Errors Repo");
    run(agira(home.path(), &repo).args(["task", "add", "existing title"]));
    run(agira(home.path(), &repo).args(["workflow", "add", "alt", "--phases", "implementing"]));

    let manifest = repo.join("bad.toml");
    fs::write(
        &manifest,
        r#"
[[task]]
id = "dup"
title = "existing title"
depends_on = ["missing"]
phase = "not-a-phase"
workflow = "missing-workflow"

[[task]]
id = "dup"
title = "Local Duplicate"

[[task]]
id = "cycle-a"
title = "local duplicate"
depends_on = ["cycle-b"]

[[task]]
id = "cycle-b"
title = "cycle b"
depends_on = ["cycle-a"]
"#,
    )
    .unwrap();

    let before = fs::read_to_string(
        project_state_dir_for(home.path(), "batch-errors-repo").join("tasks.json"),
    )
    .unwrap();
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add-batch",
        "--file",
        manifest.to_str().unwrap(),
    ]));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "unknown dependency reference: missing",
        "duplicate local id alias: dup",
        "duplicate title in manifest: local duplicate",
        "a task with this title already exists: task-001 \"existing title\"",
        "unknown workflow 'missing-workflow'",
        "unknown phase: not-a-phase",
        "dependency cycle among manifest tasks",
    ] {
        assert!(
            stderr.contains(expected),
            "missing {expected:?} in:\n{stderr}"
        );
    }
    let after = fs::read_to_string(
        project_state_dir_for(home.path(), "batch-errors-repo").join("tasks.json"),
    )
    .unwrap();
    assert_eq!(after, before);
}

#[test]
fn task_add_batch_rejects_file_and_stdin_combinations_with_exit_1() {
    let (home, _workspace, repo) = setup_initialized_repo("Batch Args Repo");
    let manifest = repo.join("tasks.toml");
    fs::write(&manifest, "[[task]]\ntitle = \"x\"\n").unwrap();

    let neither = run(agira(home.path(), &repo).args(["task", "add-batch"]));
    assert_eq!(neither.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&neither.stderr)
            .contains("supply exactly one of --file or --stdin")
    );

    let both = run(agira(home.path(), &repo).args([
        "task",
        "add-batch",
        "--file",
        manifest.to_str().unwrap(),
        "--stdin",
    ]));
    assert_eq!(both.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("supply exactly one of --file or --stdin")
    );
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
    assert!(
        stdout.contains("--runner"),
        "expected --runner in 'agira task todo --help', got:\n{stdout}"
    );
}

#[test]
fn task_todo_runner_flag_claims_selected_task() {
    let (home, _workspace, repo) = setup_initialized_repo("Runner Flag Repo");
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "runner flag task",
        "--description",
        "A task claimed by the runner flag",
        "--phase",
        "implementing",
    ]));
    assert!(output.status.success(), "task add failed");

    let output = run(agira(home.path(), &repo).args(["task", "todo", "--runner", "runner-cli"]));
    assert!(
        output.status.success(),
        "todo failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let runners_json = fs::read_to_string(
        project_state_dir_for(home.path(), "runner-flag-repo")
            .join("runner")
            .join("runners.json"),
    )
    .expect("read runners.json");
    assert!(runners_json.contains("\"runner-cli\""));
    assert!(runners_json.contains("\"current_task\": \"task-001\""));
    assert!(runners_json.contains("\"lease_expires_at\""));
    assert!(runners_json.contains("\"last_heartbeat\""));
}

#[test]
fn task_todo_runner_env_claims_selected_task() {
    let (home, _workspace, repo) = setup_initialized_repo("Runner Env Repo");
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "runner env task",
        "--description",
        "A task claimed by the runner env var",
        "--phase",
        "implementing",
    ]));
    assert!(output.status.success(), "task add failed");

    let output = run(agira(home.path(), &repo)
        .env("AGIRA_RUNNER_ID", "runner-env")
        .args(["task", "todo"]));
    assert!(
        output.status.success(),
        "todo failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let runners_json = fs::read_to_string(
        project_state_dir_for(home.path(), "runner-env-repo")
            .join("runner")
            .join("runners.json"),
    )
    .expect("read runners.json");
    assert!(runners_json.contains("\"runner-env\""));
    assert!(runners_json.contains("\"current_task\": \"task-001\""));
}

#[test]
fn hidden_task_lock_and_unlock_still_operate() {
    let (home, _workspace, repo) = setup_initialized_repo("Hidden Lock Repo");
    let output = run(agira(home.path(), &repo).args([
        "task",
        "add",
        "hidden lock task",
        "--description",
        "A task used to verify hidden lock commands still work",
    ]));
    assert!(output.status.success(), "task add failed");

    let output = run(agira(home.path(), &repo).args(["task", "lock", "task-001"]));
    assert!(
        output.status.success(),
        "task lock failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let tasks_json = fs::read_to_string(
        project_state_dir_for(home.path(), "hidden-lock-repo").join("tasks.json"),
    )
    .expect("read tasks.json after lock");
    assert!(tasks_json.contains("\"locked_at\": \"20"));

    let output = run(agira(home.path(), &repo).args(["task", "unlock", "task-001"]));
    assert!(
        output.status.success(),
        "task unlock failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let tasks_json = fs::read_to_string(
        project_state_dir_for(home.path(), "hidden-lock-repo").join("tasks.json"),
    )
    .expect("read tasks.json after unlock");
    assert!(!tasks_json.contains("\"locked_at\""));
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

/// `agira task list` (plain text) always shows a runner header line above the
/// task table.  When no runner is registered and no tmux session is live, the
/// header must read "no runner".
#[test]
fn task_list_plain_shows_runner_header_above_table() {
    let (home, _workspace, repo) = setup_initialized_repo("Runner Header Plain Repo");

    run(agira(home.path(), &repo).args(["task", "add", "runner header task"]));

    let output = run(agira(home.path(), &repo).args(["task", "list"]));

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Without a tmux session, the header must say "no runner"
    assert!(
        stdout.contains("no runner"),
        "expected 'no runner' header in plain task list\nstdout:\n{stdout}"
    );

    // Header must appear before the table-header line ("ID")
    let header_pos = stdout.find("no runner").expect("header not found");
    let table_pos = stdout.find("ID").expect("table header not found");
    assert!(
        header_pos < table_pos,
        "runner header must appear before table header\nstdout:\n{stdout}"
    );

    // There must be a blank line separating header from table
    assert!(
        stdout.contains("\n\n"),
        "expected blank line between runner header and table\nstdout:\n{stdout}"
    );
}

/// `agira task list --json` (no filter) must include a top-level `runner`
/// object alongside `tasks`, with the documented fields.
#[test]
fn task_list_json_includes_runner_object_with_required_fields() {
    let (home, _workspace, repo) = setup_initialized_repo("Runner Header Json Repo");

    run(agira(home.path(), &repo).args(["task", "add", "runner json task"]));

    let output = run(agira(home.path(), &repo).args(["task", "list", "--json"]));

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("task list --json must be valid JSON");

    // Existing tasks array must be present and unchanged
    assert!(
        value.get("tasks").is_some(),
        "JSON must contain 'tasks' array\nstdout:\n{stdout}"
    );
    assert!(
        value["tasks"].is_array(),
        "tasks must be an array\nstdout:\n{stdout}"
    );

    // New runner object must be present with all documented keys
    let runner = value
        .get("runner")
        .expect("JSON must contain top-level 'runner' key");
    assert!(
        runner.get("runner_id").is_some(),
        "runner must have 'runner_id'\nstdout:\n{stdout}"
    );
    assert!(
        runner.get("runner_type").is_some(),
        "runner must have 'runner_type'\nstdout:\n{stdout}"
    );
    assert!(
        runner.get("current_task").is_some(),
        "runner must have 'current_task'\nstdout:\n{stdout}"
    );
    assert!(
        runner.get("liveness").is_some(),
        "runner must have 'liveness'\nstdout:\n{stdout}"
    );
    assert!(
        runner.get("heartbeat_age").is_some(),
        "runner must have 'heartbeat_age'\nstdout:\n{stdout}"
    );

    // With no runner registered, liveness must be "none"
    assert_eq!(
        runner["liveness"], "none",
        "expected liveness 'none' with no registered runner\nstdout:\n{stdout}"
    );
}

/// `agira task list --json <id>` (single-task filter) must NOT include a
/// `runner` key, since this path returns a single-task object.
#[test]
fn task_list_json_with_single_task_filter_has_no_runner_key() {
    let (home, _workspace, repo) = setup_initialized_repo("Runner Header Json Filter Repo");

    run(agira(home.path(), &repo).args(["task", "add", "runner json filter task"]));

    let output = run(agira(home.path(), &repo).args(["task", "list", "--json", "task-001"]));

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("task list --json <id> must be valid JSON");

    assert!(
        value.get("runner").is_none(),
        "single-task --json must NOT include 'runner' key\nstdout:\n{stdout}"
    );
}
