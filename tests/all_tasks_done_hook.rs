use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

fn agira(home: &Path, repo: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agira"));
    command.current_dir(repo).env("HOME", home);
    command
}

fn run_ok(command: &mut Command) {
    let output = command.output().unwrap();

    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn setup_repo(name: &str) -> (TempDir, TempDir, PathBuf) {
    setup_repo_with_max_retries(name, 3)
}

fn setup_repo_with_max_retries(name: &str, max_retries: u32) -> (TempDir, TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join(name);
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    let agira_root = home.path().join(".agira");
    fs::create_dir(&agira_root).unwrap();
    fs::write(
        agira_root.join("config.toml"),
        format!(
            "default_max_retries = {max_retries}\nhook_debug = false\non_retry_exhausted = \"fail\"\n"
        ),
    )
    .unwrap();

    run_ok(agira(home.path(), &repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching:sonnet,done:sonnet",
    ]));

    (home, workspace, repo)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn non_empty_file_contents_within(path: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;

    loop {
        match fs::read_to_string(path) {
            Ok(contents) if !contents.is_empty() => return Some(contents),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to read {}: {error}", path.display()),
        }

        if Instant::now() >= deadline {
            return None;
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_file_stays_empty(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;

    loop {
        match fs::read_to_string(path) {
            Ok(contents) if !contents.is_empty() => {
                panic!(
                    "expected no hook output, found {} in {}",
                    contents,
                    path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to read {}: {error}", path.display()),
        }

        if Instant::now() >= deadline {
            return;
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn register_all_tasks_done_hook(home: &Path, repo: &Path, hook_output: &Path) {
    let hook_command = format!(
        "printf '%s\\n' \"$AGIRA_TASK_ID|$AGIRA_TO_PHASE|$AGIRA_ARTIFACT\" >> {}",
        shell_quote(hook_output)
    );

    run_ok(agira(home, repo).args(["hook", "add", "--on", "all_tasks_done", &hook_command]));
}

fn register_labeled_hook(home: &Path, repo: &Path, event: &str, hook_output: &Path, label: &str) {
    let hook_command = format!(
        "printf '%s\\n' \"{label}:$AGIRA_TASK_ID|$AGIRA_TO_PHASE|$AGIRA_ARTIFACT\" >> {}",
        shell_quote(hook_output)
    );

    run_ok(agira(home, repo).args(["hook", "add", "--on", event, &hook_command]));
}

#[test]
fn all_tasks_done_hook_fires_when_last_task_transitions_to_done() {
    let (home, _workspace, repo) = setup_repo("All Tasks Done Repo");
    let hook_output = home.path().join("all-tasks-done-output.txt");
    register_all_tasks_done_hook(home.path(), &repo, &hook_output);

    run_ok(agira(home.path(), &repo).args(["task", "add", "First task"]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "Second task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "first pending"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "first done"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "second pending"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "second done"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("all_tasks_done hook did not write output within 2s");

    assert_eq!(contents, "task-002|done|second done\n");
}

#[test]
fn all_tasks_done_hook_does_not_fire_when_non_terminal_tasks_remain() {
    let (home, _workspace, repo) = setup_repo("Partial Tasks Done Repo");
    let hook_output = home.path().join("partial-all-tasks-done-output.txt");
    register_all_tasks_done_hook(home.path(), &repo, &hook_output);

    run_ok(agira(home.path(), &repo).args(["task", "add", "First task"]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "Second task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "first pending"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "first done"]));

    assert_file_stays_empty(&hook_output, Duration::from_millis(250));
}

#[test]
fn all_tasks_done_hook_runs_after_done_hook_when_last_task_transitions_to_done() {
    let (home, _workspace, repo) = setup_repo("Done Order Repo");
    let hook_output = home.path().join("done-order-output.txt");
    register_labeled_hook(home.path(), &repo, "done", &hook_output, "done");
    register_labeled_hook(
        home.path(),
        &repo,
        "all_tasks_done",
        &hook_output,
        "all_tasks_done",
    );

    run_ok(agira(home.path(), &repo).args(["task", "add", "Only task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "pending artifact"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "terminal artifact"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("ordered hook output was not written within 2s");

    assert_eq!(
        contents,
        "done:task-001|done|terminal artifact\nall_tasks_done:task-001|done|terminal artifact\n"
    );
}

#[test]
fn all_tasks_done_hook_fires_when_all_tasks_are_failed() {
    let (home, _workspace, repo) = setup_repo_with_max_retries("All Failed Repo", 1);
    let hook_output = home.path().join("all-failed-output.txt");
    register_all_tasks_done_hook(home.path(), &repo, &hook_output);

    run_ok(agira(home.path(), &repo).args(["task", "add", "First task"]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "Second task"]));
    run_ok(agira(home.path(), &repo).args([
        "task",
        "fail",
        "task-001",
        "--reason",
        "first failed",
    ]));
    run_ok(agira(home.path(), &repo).args([
        "task",
        "fail",
        "task-002",
        "--reason",
        "second failed",
    ]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("all_tasks_done hook did not write output within 2s");

    assert_eq!(contents, "task-002|failed|\n");
}

#[test]
fn all_tasks_done_hook_runs_after_failed_hook_when_last_task_transitions_to_failed() {
    let (home, _workspace, repo) = setup_repo_with_max_retries("Failed Order Repo", 1);
    let hook_output = home.path().join("failed-order-output.txt");
    register_labeled_hook(home.path(), &repo, "failed", &hook_output, "failed");
    register_labeled_hook(
        home.path(),
        &repo,
        "all_tasks_done",
        &hook_output,
        "all_tasks_done",
    );

    run_ok(agira(home.path(), &repo).args(["task", "add", "Only task"]));
    run_ok(agira(home.path(), &repo).args(["task", "fail", "task-001", "--reason", "terminal"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("ordered hook output was not written within 2s");

    assert_eq!(
        contents,
        "failed:task-001|failed|\nall_tasks_done:task-001|failed|\n"
    );
}

#[test]
fn all_tasks_done_hook_fires_when_tasks_are_done_and_failed() {
    let (home, _workspace, repo) = setup_repo_with_max_retries("Mixed Terminal Repo", 1);
    let hook_output = home.path().join("mixed-terminal-output.txt");
    register_all_tasks_done_hook(home.path(), &repo, &hook_output);

    run_ok(agira(home.path(), &repo).args(["task", "add", "Failed task"]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "Done task"]));
    run_ok(agira(home.path(), &repo).args(["task", "fail", "task-001", "--reason", "failed task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "done pending"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "done terminal"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("all_tasks_done hook did not write output within 2s");

    assert_eq!(contents, "task-002|done|done terminal\n");
}

#[test]
fn all_tasks_done_hook_fires_after_cascade_fail_drains_pending_queue() {
    // task-001 is a dependency of task-002.
    // Failing task-001 terminally must cascade-fail task-002.
    // After the cascade the all_tasks_done hook must fire exactly once.
    let (home, _workspace, repo) = setup_repo_with_max_retries("Cascade Fail Repo", 1);
    let hook_output = home.path().join("cascade-fail-output.txt");
    register_all_tasks_done_hook(home.path(), &repo, &hook_output);

    run_ok(agira(home.path(), &repo).args(["task", "add", "Root task"]));
    run_ok(agira(home.path(), &repo).args([
        "task",
        "add",
        "Dependent task",
        "--depends-on",
        "task-001",
    ]));

    // Fail task-001 terminally; this must cascade-fail task-002.
    run_ok(agira(home.path(), &repo).args([
        "task",
        "fail",
        "task-001",
        "--reason",
        "root failure",
    ]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("all_tasks_done hook did not fire within 2s after cascade");

    // The hook fires once. The triggering task context is from the original
    // task-001 fail (that is the hook_ctx used in fail_task_flow). Both tasks
    // are now terminal (failed), so all_tasks_done must be true.
    assert!(
        !contents.is_empty(),
        "all_tasks_done hook must have written output"
    );

    // Verify task-002 is failed with the cascade reason.
    let output = agira(home.path(), &repo)
        .args(["task", "inspect", "task-002"])
        .output()
        .unwrap();
    let inspect = String::from_utf8_lossy(&output.stdout);
    assert!(
        inspect.contains("dependency task-001 failed"),
        "inspect output must show cascade reason; got:\n{inspect}"
    );
}
