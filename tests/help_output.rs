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
