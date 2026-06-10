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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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

fn config_path(home: &Path) -> PathBuf {
    state_dir(home).join("config.json")
}

fn read_config(home: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(config_path(home)).unwrap()).unwrap()
}

fn write_config(home: &Path, config: &serde_json::Value) {
    fs::write(
        config_path(home),
        serde_json::to_string_pretty(config).unwrap(),
    )
    .unwrap();
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

#[test]
fn read_only_task_load_rewrites_legacy_workflow_and_state_machine_snapshot() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    let tasks_path = state_dir(home.path()).join("tasks.json");
    fs::write(
        &tasks_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "tasks": [
                {
                    "id": "task-001",
                    "title": "legacy task",
                    "description": "",
                    "state": "pending",
                    "dependencies": [],
                    "retry_count": 0,
                    "max_retries": 3,
                    "phases": {},
                    "history": [
                        {
                            "from": null,
                            "to": "pending",
                            "timestamp": "2026-06-10T00:00:00Z",
                            "reason": "task created"
                        }
                    ],
                    "created_at": "2026-06-10T00:00:00Z",
                    "workflow": null,
                    "state_machine": [
                        { "name": "pending" },
                        { "name": "done" }
                    ]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run(agira(home.path(), &repo).args(["task", "list"]));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rewritten: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&tasks_path).unwrap()).unwrap();
    let task = &rewritten["tasks"][0];
    assert_eq!(task["workflow"], "default");
    assert!(task.get("state_machine").is_none());
}

#[test]
fn config_validation_rejects_dangling_duplicate_and_missing_default_workflows() {
    let cases = [
        (
            "dangling",
            serde_json::json!({
                "default": ["pending", "missing_phase", "done"]
            }),
            "references unknown phase 'missing_phase'",
            Some("default"),
        ),
        (
            "duplicate",
            serde_json::json!({
                "default": ["pending", "enriching", "enriching", "done"]
            }),
            "references duplicate phase 'enriching'",
            Some("default"),
        ),
        (
            "missing_default",
            serde_json::json!({
                "default": ["pending", "enriching", "done"]
            }),
            "default_workflow 'missing' not found in workflows",
            Some("missing"),
        ),
    ];

    for (_name, workflows, expected, default_workflow) in cases {
        let (home, _workspace, repo) = setup_repo();
        init_project(home.path(), &repo);
        let mut config = read_config(home.path());
        config["workflows"] = workflows;
        if let Some(default_workflow) = default_workflow {
            config["default_workflow"] = serde_json::Value::String(default_workflow.to_owned());
        }
        write_config(home.path(), &config);

        let output = run(agira(home.path(), &repo).args(["workflow", "list"]));

        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(
            stderr(&output).contains(expected),
            "missing {expected:?} in {}",
            stderr(&output)
        );
    }
}

#[test]
fn legacy_config_shapes_no_longer_parse() {
    let cases: [(&str, fn(&mut serde_json::Value)); 6] = [
        ("default_model", |config: &mut serde_json::Value| {
            config["default_model"] = serde_json::Value::String("opus".to_owned());
        }),
        ("phases_array", |config: &mut serde_json::Value| {
            config["phases"] = serde_json::json!([
                { "name": "in_progress", "model": "opus" }
            ]);
        }),
        ("denormalized_workflow", |config: &mut serde_json::Value| {
            config["workflows"]["default"] = serde_json::json!({
                "phases": [
                    { "name": "pending" },
                    { "name": "done" }
                ]
            });
        }),
        ("models", |config: &mut serde_json::Value| {
            config["models"] = serde_json::json!({ "in_progress": "opus" });
        }),
        ("acceptance_testing", |config: &mut serde_json::Value| {
            config["acceptance_testing"] = serde_json::json!({ "enabled": true });
        }),
        ("state_machine", |config: &mut serde_json::Value| {
            config["state_machine"] = serde_json::json!(["pending", "done"]);
        }),
    ];

    for (_name, mutate) in cases {
        let (home, _workspace, repo) = setup_repo();
        init_project(home.path(), &repo);
        let mut config = read_config(home.path());
        mutate(&mut config);
        write_config(home.path(), &config);

        let output = run(agira(home.path(), &repo).args(["workflow", "list"]));

        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("failed to load config"),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
fn phase_crud_rejects_reserved_and_referenced_phase_removal() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    let reserved = run(agira(home.path(), &repo).args(["phase", "add", "pending"]));
    assert_eq!(reserved.status.code(), Some(1), "{}", stderr(&reserved));
    assert!(stderr(&reserved).contains("reserved phase name: pending"));

    let referenced = run(agira(home.path(), &repo).args(["phase", "remove", "enriching"]));
    assert_eq!(referenced.status.code(), Some(1), "{}", stderr(&referenced));
    assert!(stderr(&referenced).contains("workflows still reference it: default"));

    assert_eq!(
        run(agira(home.path(), &repo).args(["phase", "add", "review", "--model", "codex"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["phase", "update", "review", "--clear-model"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["phase", "remove", "review"]))
            .status
            .code(),
        Some(0)
    );

    let output = run(agira(home.path(), &repo).args(["phase", "list"]));
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("review"));
}

#[test]
fn workflow_remove_and_update_reject_in_flight_task_references() {
    let (home, _workspace, repo) = setup_repo();
    init_project(home.path(), &repo);

    assert_eq!(
        run(agira(home.path(), &repo).args(["workflow", "remove", "default"]))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "add", "default task"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "todo", "--artifact", "accepted"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "todo", "--artifact", "enriched"]))
            .status
            .code(),
        Some(0)
    );

    let remove_phase = run(agira(home.path(), &repo).args([
        "workflow",
        "update",
        "default",
        "--remove",
        "in_progress",
    ]));
    assert_eq!(
        remove_phase.status.code(),
        Some(1),
        "{}",
        stderr(&remove_phase)
    );
    assert!(stderr(&remove_phase).contains("task task-001 is in that phase"));

    assert_eq!(
        run(agira(home.path(), &repo).args(["workflow", "add", "fast", "--phases", "in_progress"]))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(agira(home.path(), &repo).args(["task", "add", "fast task", "--workflow", "fast"]))
            .status
            .code(),
        Some(0)
    );

    let remove_workflow = run(agira(home.path(), &repo).args(["workflow", "remove", "fast"]));
    assert_eq!(
        remove_workflow.status.code(),
        Some(1),
        "{}",
        stderr(&remove_workflow)
    );
    assert!(stderr(&remove_workflow).contains("task task-002 is not terminal"));
}
