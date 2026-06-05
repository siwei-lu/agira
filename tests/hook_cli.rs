use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

fn agira(home: &Path, repo: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agira"));
    command.current_dir(repo).env("HOME", home);
    command
}

fn agira_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agira"))
}

fn run_ok(command: &mut Command) -> Output {
    let output = command.output().unwrap();

    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    output
}

fn run_err(command: &mut Command) -> Output {
    let output = command.output().unwrap();

    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    output
}

fn setup_repo() -> (TempDir, TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join("Hook CLI Repo");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();

    run_ok(agira(home.path(), &repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching:sonnet,in_progress:sonnet,verifying:haiku,done:haiku",
        "--verification-commands",
        "none",
        "--acceptance-testing",
        "cli",
    ]));

    (home, workspace, repo)
}

fn project_hooks_path(home: &Path) -> PathBuf {
    home.join(".agira").join("hook-cli-repo").join("hooks.toml")
}

fn global_config_path(home: &Path) -> PathBuf {
    home.join(".agira").join("config.toml")
}

#[test]
fn hook_add_creates_project_hooks_file_and_list_shows_project_hook() {
    let (home, _workspace, repo) = setup_repo();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "done", "printf", "ok"]));

    let hooks_path = project_hooks_path(home.path());
    let contents = fs::read_to_string(&hooks_path).unwrap();
    assert!(contents.contains("on = \"done\""));
    assert!(contents.contains("run = \"printf ok\""));

    let output = run_ok(agira(home.path(), &repo).args(["hook", "list"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("source  event  command"));
    assert!(stdout.contains("project  done  printf ok"));
}

#[test]
fn hook_add_accepts_task_added_event() {
    let (home, _workspace, repo) = setup_repo();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "task_added", "printf", "created"]));

    let contents = fs::read_to_string(project_hooks_path(home.path())).unwrap();
    assert!(contents.contains("on = \"task_added\""));
    assert!(contents.contains("run = \"printf created\""));
}

#[test]
fn hook_add_global_writes_user_config_and_list_shows_global_hook() {
    let (home, _workspace, repo) = setup_repo();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "--global", "done", "printf", "global"]));

    let global_config = fs::read_to_string(global_config_path(home.path())).unwrap();
    assert!(global_config.contains("default_max_retries = 3"));
    assert!(global_config.contains("on = \"done\""));
    assert!(global_config.contains("run = \"printf global\""));
    assert!(!project_hooks_path(home.path()).exists());

    let output = run_ok(agira(home.path(), &repo).args(["hook", "list"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("source  event  command"));
    assert!(stdout.contains("global  done  printf global"));
    assert!(!stdout.contains("project  done  printf global"));
}

#[test]
fn hook_add_global_preserves_existing_user_config_values() {
    let (home, _workspace, repo) = setup_repo();
    fs::write(global_config_path(home.path()), "default_max_retries = 9\n").unwrap();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "--global", "failed", "printf", "fail"]));

    let global_config = fs::read_to_string(global_config_path(home.path())).unwrap();
    assert!(global_config.contains("default_max_retries = 9"));
    assert!(global_config.contains("on = \"failed\""));
    assert!(global_config.contains("run = \"printf fail\""));
}

#[test]
fn hook_remove_prevents_project_hook_from_appearing_in_list() {
    let (home, _workspace, repo) = setup_repo();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "done", "printf", "ok"]));
    run_ok(agira(home.path(), &repo).args(["hook", "remove", "done"]));

    let output = run_ok(agira(home.path(), &repo).args(["hook", "list"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "no hooks configured\n");
}

#[test]
fn hook_remove_global_removes_only_user_hooks_for_event() {
    let (home, _workspace, repo) = setup_repo();

    run_ok(agira(home.path(), &repo).args(["hook", "add", "--global", "done", "printf", "global"]));
    run_ok(agira(home.path(), &repo).args(["hook", "add", "done", "printf", "project"]));
    run_ok(agira(home.path(), &repo).args(["hook", "remove", "--global", "done"]));

    let global_config = fs::read_to_string(global_config_path(home.path())).unwrap();
    assert!(global_config.contains("default_max_retries = 3"));
    assert!(!global_config.contains("[[hooks]]"));
    assert!(project_hooks_path(home.path()).exists());

    let output = run_ok(agira(home.path(), &repo).args(["hook", "list"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("source  event  command"));
    assert!(stdout.contains("project  done  printf project"));
    assert!(!stdout.contains("global  done  printf global"));
}

#[test]
fn hook_add_unknown_phase_exits_nonzero_with_unknown_hook_event() {
    let (home, _workspace, repo) = setup_repo();

    let output = run_err(agira(home.path(), &repo).args(["hook", "add", "review", "echo", "x"]));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("unknown hook event: review"));
}

#[test]
fn hook_add_help_documents_injected_env_vars() {
    let output = agira_bin()
        .args(["hook", "add", "--help"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "hook add --help exited with non-zero status"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("AGIRA_TASK_ID"),
        "expected AGIRA_TASK_ID in hook add --help output"
    );
    assert!(
        stdout.contains("task_added"),
        "expected task_added in hook add --help output"
    );
    assert!(
        stdout.contains("AGIRA_TASK_DESCRIPTION"),
        "expected AGIRA_TASK_DESCRIPTION in hook add --help output"
    );
    assert!(
        stdout.contains("AGIRA_PROJECT_PATH"),
        "expected AGIRA_PROJECT_PATH in hook add --help output"
    );
    assert!(
        stdout.contains("AGIRA_TO_PHASE"),
        "expected AGIRA_TO_PHASE in hook add --help output"
    );
}

#[test]
fn hook_help_documents_injected_env_vars() {
    let output = agira_bin().args(["hook", "--help"]).output().unwrap();

    assert!(
        output.status.success(),
        "hook --help exited with non-zero status"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("AGIRA_TASK_ID"),
        "expected AGIRA_TASK_ID in hook --help output"
    );
    assert!(
        stdout.contains("task_added"),
        "expected task_added in hook --help output"
    );
    assert!(
        stdout.contains("AGIRA_TASK_DESCRIPTION"),
        "expected AGIRA_TASK_DESCRIPTION in hook --help output"
    );
    assert!(
        stdout.contains("AGIRA_PROJECT_PATH"),
        "expected AGIRA_PROJECT_PATH in hook --help output"
    );
    assert!(
        stdout.contains("AGIRA_TO_PHASE"),
        "expected AGIRA_TO_PHASE in hook --help output"
    );
}
