use std::process::Command;

fn agira() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agira"))
}

#[test]
fn skill_install_prints_task_add_skill_prompt() {
    let output = agira().args(["skill", "install"]).output().unwrap();

    assert!(
        output.status.success(),
        "expected 'agira skill install' to exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("agira-task-add"),
        "install prompt should name the skill, got:\n{stdout}"
    );
    assert!(
        stdout.contains("agira project list"),
        "install prompt should discover projects, got:\n{stdout}"
    );
    assert!(
        stdout.contains("agira task add"),
        "install prompt should run 'agira task add', got:\n{stdout}"
    );
}

#[test]
fn skill_uninstall_prints_scoped_deletion_prompt() {
    let output = agira().args(["skill", "uninstall"]).output().unwrap();

    assert!(
        output.status.success(),
        "expected 'agira skill uninstall' to exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("agira-task-add"),
        "uninstall prompt should name the skill, got:\n{stdout}"
    );
    assert!(
        stdout.contains("only"),
        "uninstall prompt should scope deletion to only the managed skill, got:\n{stdout}"
    );
}

#[test]
fn skill_install_works_outside_an_initialized_project() {
    // The install prompt teaches discovery, so the command itself must not require
    // being inside an initialized agira repo. Run from a bare temp dir.
    let dir = tempfile::TempDir::new().unwrap();
    let output = agira()
        .args(["skill", "install"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "expected 'agira skill install' to succeed outside a project, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
