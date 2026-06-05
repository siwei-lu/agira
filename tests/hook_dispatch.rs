use std::{
    fs, io,
    path::Path,
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
fn work_artifact_dispatches_hook_with_env_vars() {
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
    run_ok(agira(home.path(), &repo).args(["task", "work", "--artifact", "artifact value"]));

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
