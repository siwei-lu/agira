use std::{path::Path, process::Command};

pub fn commit_prompt(task_id: &str, task_title: &str, commit_convention: Option<&str>) -> String {
    let convention_section = match commit_convention {
        Some(log) => {
            let indented = log
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("Recent commits for convention reference:\n{indented}")
        }
        None => "No recent commits found.".to_owned(),
    };

    format!(
        "# Commit\n\nTask {task_id} \"{task_title}\" is complete — please commit.\n\n{convention_section}\n\nSuggested commit message:\n  feat({task_id}): {task_title}"
    )
}

pub(crate) fn read_recent_commits(git_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(git_root)
        .output()
        .ok()?;

    if output.status.success() && !output.stdout.is_empty() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        None
    }
}
