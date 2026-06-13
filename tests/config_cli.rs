use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

fn agira(home: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agira"));
    command.current_dir(cwd).env("HOME", home);
    command
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

#[test]
fn config_get_and_set_work_without_git_repo() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();

    let output = run_ok(agira(home.path(), cwd.path()).args(["config", "get"]));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hook_debug = false\ndefault_max_retries = 3\non_retry_exhausted = block\nrunner.auto_start = false\nrunner.lease_ttl = 5m\nrunner.type = claude-tmux\n"
    );

    let output =
        run_ok(agira(home.path(), cwd.path()).args(["config", "set", "hook_debug", "true"]));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hook_debug = true\n"
    );

    let output =
        run_ok(agira(home.path(), cwd.path()).args(["config", "set", "default-max-retries", "5"]));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "default_max_retries = 5\n"
    );

    let contents = fs::read_to_string(home.path().join(".agira").join("config.toml")).unwrap();
    assert!(contents.contains("default_max_retries = 5"));
    assert!(contents.contains("hook_debug = true"));
}
