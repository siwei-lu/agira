use std::process::Command;

fn agira() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agira"))
}

fn help_stdout(args: &[&str]) -> String {
    let mut cmd = agira();
    cmd.args(args).arg("--help");
    let output = cmd.output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn has_uppercase_placeholder_start(out: &str) -> bool {
    out.as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'<' && pair[1].is_ascii_uppercase())
}

#[test]
fn task_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["task"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira task --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira task --help'"
    );
}

#[test]
fn task_help_hides_legacy_lock_commands() {
    let out = help_stdout(&["task"]);

    assert!(
        !out.contains(" lock"),
        "did not expect legacy lock command in 'agira task --help', got:\n{out}"
    );
    assert!(
        !out.contains(" unlock"),
        "did not expect legacy unlock command in 'agira task --help', got:\n{out}"
    );
}

#[test]
fn task_add_help_describes_workflow_without_state_machine_wording() {
    let out = help_stdout(&["task", "add"]);
    assert!(
        out.contains("Use a named workflow from the project config to execute this task"),
        "expected 'agira task add --help' to describe --workflow execution, got:\n{out}"
    );
    assert!(
        !out.contains("state machine"),
        "found stale state machine wording in 'agira task add --help'"
    );
}

#[test]
fn phase_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["phase"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira phase --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira phase --help'"
    );
}

#[test]
fn hook_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["hook"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira hook --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira hook --help'"
    );
}

#[test]
fn hook_update_help_uses_lowercase_event_placeholder() {
    let out = help_stdout(&["hook", "update"]);
    assert!(
        out.contains("<event>"),
        "expected lowercase <event> in 'agira hook update --help', got:\n{out}"
    );
    assert!(
        !has_uppercase_placeholder_start(&out),
        "found uppercase placeholder matching '<[A-Z]' in 'agira hook update --help', got:\n{out}"
    );
}

#[test]
fn project_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["project"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira project --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira project --help'"
    );
}

#[test]
fn skill_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["skill"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira skill --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira skill --help'"
    );
}

#[test]
fn runner_help_uses_lowercase_command_placeholder() {
    let out = help_stdout(&["runner"]);
    assert!(
        out.contains("<command>"),
        "expected lowercase <command> in 'agira runner --help', got:\n{out}"
    );
    assert!(
        !out.contains("<COMMAND>"),
        "found uppercase <COMMAND> in 'agira runner --help'"
    );
}

#[test]
fn runner_start_help_lists_type_flag() {
    let out = help_stdout(&["runner", "start"]);
    assert!(
        out.contains("--type <type>"),
        "expected --type <type> in 'agira runner start --help', got:\n{out}"
    );
}

#[test]
fn config_help_lists_valid_keys() {
    let out = help_stdout(&["config"]);
    assert!(
        out.contains("default-max-retries"),
        "expected 'agira config --help' to list default-max-retries, got:\n{out}"
    );
    assert!(
        out.contains("hook-debug"),
        "expected 'agira config --help' to list hook-debug, got:\n{out}"
    );
}

#[test]
fn config_get_help_lists_displayed_keys() {
    let out = help_stdout(&["config", "get"]);
    assert!(
        out.contains("default-max-retries"),
        "expected 'agira config get --help' to list default-max-retries, got:\n{out}"
    );
    assert!(
        out.contains("hook-debug"),
        "expected 'agira config get --help' to list hook-debug, got:\n{out}"
    );
}

#[test]
fn config_set_help_lists_settable_key() {
    let out = help_stdout(&["config", "set"]);
    assert!(
        out.contains("hook-debug"),
        "expected 'agira config set --help' to list hook-debug, got:\n{out}"
    );
    assert!(
        out.contains("<key>"),
        "expected lowercase <key> in 'agira config set --help', got:\n{out}"
    );
    assert!(
        out.contains("<value>"),
        "expected lowercase <value> in 'agira config set --help', got:\n{out}"
    );
    assert!(
        !out.contains("<KEY>"),
        "found uppercase <KEY> in 'agira config set --help'"
    );
    assert!(
        !out.contains("<VALUE>"),
        "found uppercase <VALUE> in 'agira config set --help'"
    );
    assert!(
        !out.contains("default-max-retries"),
        "did not expect 'agira config set --help' to advertise default-max-retries as settable, got:\n{out}"
    );
}
