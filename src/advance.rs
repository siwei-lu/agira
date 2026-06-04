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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_prompt_contains_task_id_and_title() {
        let prompt = commit_prompt("task-042", "Add login endpoint", None);

        assert!(prompt.contains("task-042"));
        assert!(prompt.contains("Add login endpoint"));
        assert!(prompt.contains("# Commit"));
        assert!(prompt.contains("feat(task-042): Add login endpoint"));
    }

    #[test]
    fn commit_prompt_with_convention_shows_log() {
        let log = "abc1234 feat(cli): add status filter\ndef5678 fix(tasks): handle empty store";
        let prompt = commit_prompt("task-001", "My feature", Some(log));

        assert!(prompt.contains("abc1234 feat(cli): add status filter"));
        assert!(prompt.contains("def5678 fix(tasks): handle empty store"));
        assert!(!prompt.contains("No recent commits found"));
    }

    #[test]
    fn commit_prompt_without_convention_shows_fallback() {
        let prompt = commit_prompt("task-001", "My feature", None);

        assert!(prompt.contains("No recent commits found"));
    }
}
