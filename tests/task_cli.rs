use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

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

#[test]
fn task_subcommands_require_initialized_project() {
    let cases: &[&[&str]] = &[
        &["task", "status"],
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
