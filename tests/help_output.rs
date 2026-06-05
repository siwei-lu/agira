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
