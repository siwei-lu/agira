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

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn setup_repo() -> (TempDir, TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let repo = workspace.path().join("Repo");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();
    (home, workspace, repo)
}

fn state_dir(home: &Path) -> PathBuf {
    home.join(".agira").join("repo")
}

fn init_project(home: &Path, repo: &Path) {
    let output = run(agira(home, repo).args([
        "init",
        "--stack",
        "rust",
        "--phases",
        "enriching:opus,in_progress:sonnet,verifying:haiku",
    ]));
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn init_writes_normalized_palette_and_workflow_refs() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    let contents = fs::read_to_string(state_dir(home.path()).join("config.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&contents).unwrap();

    assert!(value["phases"].is_object());
    assert_eq!(
        value["workflows"]["default"],
        serde_json::json!(["pending", "enriching", "in_progress", "verifying", "done"])
    );
    assert!(contents.find("\"done\"").unwrap() < contents.find("\"enriching\"").unwrap());
}

#[test]
fn task_add_rejects_removed_phase_snapshot_flags() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    let output =
        run(agira(home.path(), &repo).args(["task", "add", "custom", "--phases", "pending,done"]));

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--phases'"));
}

#[test]
fn phase_duty_update_changes_prompt_for_existing_task() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "add", "live duty"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "todo", "--artifact", "task accepted"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args([
            "phase",
            "update",
            "enriching",
            "--set-duty",
            "fresh duty from palette",
        ]))
        .status
        .code(),
        Some(0)
    );

    let output = run(agira(home.path(), &repo).args(["task", "todo"]));

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("fresh duty from palette"));
}

#[test]
fn workflow_add_update_set_default_and_remove_are_persisted() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    assert_eq!(
        run(agira(home.path(), &repo).args(["phase", "add", "review", "--model", "codex"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args([
            "workflow",
            "add",
            "fast",
            "--phases",
            "in_progress,review",
        ]))
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["workflow", "set-default", "fast"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["workflow", "update", "fast", "--remove", "review"]))
            .status
            .code(),
        Some(0)
    );

    let output = run(agira(home.path(), &repo).args(["workflow", "list"]));

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fast (default)"));
    assert!(!stdout.contains("review:codex"));
}
