use std::{
    fs, io,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use chrono::DateTime;
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

fn setup_repo(name: &str) -> (TempDir, TempDir, std::path::PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join(name);
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    run_ok(agira(home.path(), &repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching:sonnet,done:sonnet",
        "--verification-commands",
        "none",
        "--acceptance-testing",
        "cli",
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

#[test]
fn todo_artifact_dispatches_hook_with_env_vars() {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join("Hook Repo");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    let agira_root = home.path().join(".agira");
    fs::create_dir(&agira_root).unwrap();
    let hook_output = home.path().join("hook-output.txt");
    let hook_command = format!(
        "printf '%s\\n' \"$AGIRA_TASK_ID|$AGIRA_TASK_TITLE|$AGIRA_PROJECT_SLUG|$AGIRA_FROM_PHASE|$AGIRA_TO_PHASE|$AGIRA_ARTIFACT\" > {}",
        shell_quote(&hook_output)
    );
    fs::write(
        agira_root.join("config.toml"),
        format!(
            r#"default_max_retries = 3

[[hooks]]
on = "done"
run = {hook_command:?}
"#
        ),
    )
    .unwrap();

    run_ok(agira(home.path(), &repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching:sonnet,done:sonnet",
        "--verification-commands",
        "none",
        "--acceptance-testing",
        "cli",
    ]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "Env hook task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "artifact value"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_millis(500))
        .expect("hook did not write output within 500ms");
    let fields: Vec<&str> = contents.trim_end().split('|').collect();

    assert_eq!(
        fields,
        vec![
            "task-001",
            "Env hook task",
            "hook-repo",
            "enriching",
            "done",
            "artifact value"
        ]
    );
}

#[test]
fn global_hook_registered_by_cli_fires_on_matching_task_transition() {
    let (home, _workspace, repo) = setup_repo("Global Hook Repo");
    let hook_output = home.path().join("cli-global-hook-output.txt");
    let hook_path = shell_quote(&hook_output);

    run_ok(agira(home.path(), &repo).args([
        "hook",
        "add",
        "--global",
        "done",
        "printf",
        "global-hook-fired",
        ">",
        &hook_path,
    ]));
    run_ok(agira(home.path(), &repo).args(["task", "add", "CLI global hook task"]));
    run_ok(agira(home.path(), &repo).args(["task", "todo", "--artifact", "complete"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("global hook registered by CLI did not write output within 2s");

    assert_eq!(contents, "global-hook-fired");
}

#[test]
fn task_added_hook_receives_new_task_env_vars() {
    let (home, _workspace, repo) = setup_repo("Task Added Hook Repo");
    let hooks_path = home
        .path()
        .join(".agira")
        .join("task-added-hook-repo")
        .join("hooks.toml");
    let hook_output = home.path().join("task-added-hook-output.txt");
    let hook_command = format!(
        "printf '%s\\n' \"$AGIRA_TASK_ID|$AGIRA_TASK_TITLE|$AGIRA_TASK_DESCRIPTION|$AGIRA_TASK_STATE|$AGIRA_TASK_PRD_MODULE_ID|$AGIRA_TASK_DEPENDENCIES|$AGIRA_TASK_RETRY_COUNT|$AGIRA_TASK_MAX_RETRIES|$AGIRA_TASK_CREATED_AT|$AGIRA_PROJECT_SLUG|$AGIRA_FROM_PHASE|$AGIRA_TO_PHASE|$AGIRA_ARTIFACT\" > {}",
        shell_quote(&hook_output)
    );
    fs::write(
        hooks_path,
        format!(
            r#"[[hooks]]
on = "task_added"
run = {hook_command:?}
"#
        ),
    )
    .unwrap();

    run_ok(agira(home.path(), &repo).args([
        "task",
        "add",
        "Task added hook task",
        "--description",
        "description value",
        "--prd",
        "FM-012",
    ]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("task_added hook did not write output within 2s");
    let fields: Vec<&str> = contents.trim_end().split('|').collect();

    assert_eq!(fields.len(), 13);
    assert_eq!(fields[0], "task-001");
    assert_eq!(fields[1], "Task added hook task");
    assert_eq!(fields[2], "description value");
    assert_eq!(fields[3], "enriching");
    assert_eq!(fields[4], "FM-012");
    assert_eq!(fields[5], "");
    assert_eq!(fields[6], "0");
    assert_eq!(fields[7], "3");
    DateTime::parse_from_rfc3339(fields[8]).unwrap();
    assert_eq!(fields[9], "task-added-hook-repo");
    assert_eq!(fields[10], "");
    assert_eq!(fields[11], "enriching");
    assert_eq!(fields[12], "");
}

#[test]
fn wildcard_hook_fires_when_task_is_added() {
    let (home, _workspace, repo) = setup_repo("Wildcard Task Added Repo");
    let hooks_path = home
        .path()
        .join(".agira")
        .join("wildcard-task-added-repo")
        .join("hooks.toml");
    let hook_output = home.path().join("wildcard-task-added-output.txt");
    let hook_command = format!("printf wildcard > {}", shell_quote(&hook_output));
    fs::write(
        hooks_path,
        format!(
            r#"[[hooks]]
on = "*"
run = {hook_command:?}
"#
        ),
    )
    .unwrap();

    run_ok(agira(home.path(), &repo).args(["task", "add", "Wildcard task added hook"]));

    let contents = non_empty_file_contents_within(&hook_output, Duration::from_secs(2))
        .expect("wildcard hook did not write output for task_added within 2s");

    assert_eq!(contents, "wildcard");
}
