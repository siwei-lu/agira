use std::cmp::{Ordering, Reverse};

use chrono::{DateTime, FixedOffset};

use crate::core::{
    config::Config,
    tasks::{Task, TaskPhase},
};

const NO_TASKS_MESSAGE: &str = "No tasks found. Add tasks with `agira task add \"<title>\"` or provide requirements with `agira task work --prd <path>`";

pub(crate) fn format_pick_output(
    config: &Config,
    tasks: &[Task],
    prd_content: Option<&str>,
    just_done: Option<(&str, &str)>,
) -> String {
    if tasks.is_empty() {
        return match prd_content {
            Some(prd_content) => format_decomposition_prompt(prd_content),
            None => NO_TASKS_MESSAGE.to_owned(),
        };
    }

    if is_all_done(tasks, config) {
        return format_completion_summary(tasks);
    }

    if let Some(task) = select_next_task(tasks, config) {
        return format_task_prompt(task, config, just_done);
    }

    format_non_actionable_summary(tasks)
}

pub(crate) fn select_next_task<'a>(all_tasks: &'a [Task], config: &Config) -> Option<&'a Task> {
    let terminal_phase = config.terminal_phase()?;

    all_tasks
        .iter()
        .filter(|task| {
            is_actionable(task, config) && deps_satisfied(task, all_tasks, terminal_phase)
        })
        .max_by_key(|task| {
            (
                phase_index(&task.state, config).unwrap_or(0),
                Reverse(task_id_number(&task.id)),
            )
        })
}

fn phase_index(phase: &str, config: &Config) -> Option<usize> {
    config.phases.iter().position(|p| p.name == phase)
}

fn is_actionable(task: &Task, config: &Config) -> bool {
    config.terminal_phase().is_some_and(|terminal| {
        task.state != terminal && phase_index(&task.state, config).is_some()
    })
}

fn deps_satisfied(task: &Task, all_tasks: &[Task], terminal_phase: &str) -> bool {
    task.dependencies.iter().all(|dep_id| {
        all_tasks
            .iter()
            .find(|candidate| &candidate.id == dep_id)
            .is_some_and(|dependency| dependency.state == terminal_phase)
    })
}

fn is_all_done(tasks: &[Task], config: &Config) -> bool {
    !tasks.is_empty()
        && config
            .terminal_phase()
            .is_some_and(|terminal| tasks.iter().all(|task| task.state == terminal))
}

fn format_decomposition_prompt(prd_content: &str) -> String {
    format!(
        "# Agira PRD Decomposition\n\n## Role\nYou are the planner for this Agira project.\n\n## Objective\nBreak the requirements into small, actionable Agira tasks. Add each task with agira task add.\n\n## Commands\nFor each task, run:\n`agira task add \"<title>\" --description \"<description>\"`\n\n## Requirements Context\n{prd_content}"
    )
}

fn format_task_prompt(task: &Task, config: &Config, just_done: Option<(&str, &str)>) -> String {
    let model = config
        .phases
        .iter()
        .find(|p| p.name == task.state)
        .map(|p| p.model.as_str())
        .unwrap_or("sonnet");

    let mut subagent = format!(
        "# Agira Task Prompt\n\n## Task\n- ID: {}\n- Title: {}\n- Current phase: {}\n- Agent role: {}\n\n## Description\n{}",
        task.id,
        task.title,
        task.state,
        model,
        if task.description.is_empty() {
            "No description provided."
        } else {
            task.description.as_str()
        }
    );

    if !task.phases.is_empty() {
        subagent.push_str("\n\n## Acceptance Criteria");
        for (phase_name, phase) in prior_phases(task, config) {
            let artifact = if phase.artifact.is_empty() {
                "<empty>"
            } else {
                phase.artifact.as_str()
            };
            subagent.push_str(&format!(
                "\n- Phase: {phase_name}\n  Completed at: {}\n  Artifact: {artifact}",
                phase.completed_at
            ));
        }
    }

    if is_verification_phase(&task.state, config) {
        subagent.push_str("\n\n## Verification Commands");
        if config.verification.commands.is_empty() {
            subagent.push_str("\nNo verification commands configured.");
        } else {
            for command in &config.verification.commands {
                subagent.push_str(&format!("\n- `{command}`"));
            }
        }
    }

    subagent.push_str(&format!(
        "\n\n## Advance State\nWhen this phase is complete, run:\n`agira task work --artifact \"<artifact>\"`\n\nIf this phase cannot be completed, run:\n`agira task fail {} --reason \"<reason>\"`",
        task.id
    ));

    let steps = if let Some((done_id, done_title)) = just_done {
        format!(
            "1. Commit all changes from {done_id} \"{done_title}\" with a descriptive commit message.\n2. Spawn a subagent using model `{model}`.\nThe configured model is `{model}` — escalate to a higher-tier model if the task complexity warrants it.\n3. Pass the content between the delimiters below as the subagent's prompt.\n4. Once the subagent finishes, call `agira task work --artifact \"<subagent summary>\"` with a concise summary of what it did."
        )
    } else {
        format!(
            "1. Spawn a subagent using model `{model}`.\nThe configured model is `{model}` — escalate to a higher-tier model if the task complexity warrants it.\n2. Pass the content between the delimiters below as the subagent's prompt.\n3. Once the subagent finishes, call `agira task work --artifact \"<subagent summary>\"` with a concise summary of what it did."
        )
    };

    format!(
        "# Agira Orchestrator Instructions\n\nYou are the **orchestrator** for this task. Do NOT perform the work yourself.\n\n{steps}\n\n--- SUBAGENT PROMPT ---\n{subagent}\n--- END SUBAGENT PROMPT ---"
    )
}

fn prior_phases<'a>(task: &'a Task, config: &'a Config) -> Vec<(&'a str, &'a TaskPhase)> {
    let Some(current_index) = phase_index(&task.state, config) else {
        return Vec::new();
    };

    config
        .phases
        .iter()
        .take(current_index)
        .filter_map(|p| {
            task.phases
                .get(&p.name)
                .map(|phase| (p.name.as_str(), phase))
        })
        .collect()
}

fn is_verification_phase(phase: &str, config: &Config) -> bool {
    phase.to_ascii_lowercase().contains("verif") || !config.verification.commands.is_empty()
}

fn format_completion_summary(tasks: &[Task]) -> String {
    let mut completed_tasks: Vec<&Task> = tasks.iter().collect();
    completed_tasks.sort_by_key(|task| task_id_number(&task.id));

    let mut output = "# Agira Completion Summary\n\nAll tasks are in the terminal done phase.\n\n## Completed Tasks".to_owned();
    for task in completed_tasks {
        output.push_str(&format!(
            "\n- {}: {}\n  Final artifact: {}",
            task.id,
            task.title,
            latest_artifact(task)
        ));
    }

    output
}

fn latest_artifact(task: &Task) -> &str {
    task.phases
        .values()
        .max_by(|left, right| compare_completed_at(&left.completed_at, &right.completed_at))
        .map(|phase| {
            if phase.artifact.is_empty() {
                "<none>"
            } else {
                phase.artifact.as_str()
            }
        })
        .unwrap_or("<none>")
}

fn compare_completed_at(left: &str, right: &str) -> Ordering {
    match (
        DateTime::<FixedOffset>::parse_from_rfc3339(left),
        DateTime::<FixedOffset>::parse_from_rfc3339(right),
    ) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn format_non_actionable_summary(tasks: &[Task]) -> String {
    let mut output =
        "# Agira Task Summary\n\nNo actionable tasks found.\n\n## Non-actionable Tasks".to_owned();
    for task in tasks {
        output.push_str(&format!("\n- {}: {} ({})", task.id, task.title, task.state));
    }

    output
}

fn task_id_number(id: &str) -> u32 {
    id.rsplit_once('-')
        .and_then(|(_, suffix)| suffix.parse::<u32>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::core::config::{PhaseConfig, VerificationConfig};
    use crate::core::tasks::TaskStore;

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: "opus".to_owned(),
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: "sonnet".to_owned(),
                },
                PhaseConfig {
                    name: "verifying".to_owned(),
                    model: "haiku".to_owned(),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: "haiku".to_owned(),
                },
            ],
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
        }
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    #[test]
    fn no_tasks_no_prd_message() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let store = test_store(&temp_dir, &config);

        let output = format_pick_output(&config, store.all_tasks(), None, None);

        assert_eq!(output, NO_TASKS_MESSAGE);
    }

    #[test]
    fn no_tasks_with_prd_decomposition() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let store = test_store(&temp_dir, &config);
        let prd_text = "Build the pick command from these requirements.";

        let output = format_pick_output(&config, store.all_tasks(), Some(prd_text), None);

        assert!(output.contains("agira task add"));
        assert!(output.contains(prd_text));
    }

    #[test]
    fn select_most_advanced_phase_wins() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Later phase", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.add_task("Earlier phase", "", None, vec![]).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn select_lowest_id_within_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("First", "", None, vec![]).unwrap();
        store.add_task("Second", "", None, vec![]).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn test_blocked_task_not_selected() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Blocking task", "", None, vec![]).unwrap();
        store
            .add_task("Blocked task", "", None, vec!["task-001".to_owned()])
            .unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn task_prompt_has_orchestrator_preamble_and_delimiters() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement pick", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("# Agira Orchestrator Instructions"));
        assert!(prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(prompt.contains("--- END SUBAGENT PROMPT ---"));
        assert!(prompt.contains("# Agira Task Prompt"));
        assert!(prompt.contains("- Agent role: sonnet"));
        assert!(prompt.contains("The configured model is `sonnet`"));
    }

    #[test]
    fn task_prompt_uses_sonnet_fallback_for_unknown_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.phases.retain(|p| p.name != "in_progress");
        let mut store = test_store(&temp_dir, &test_config());

        store.add_task("Implement pick", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("- Agent role: sonnet"));
    }

    #[test]
    fn task_prompt_contains_work_command() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement work", "", None, vec![]).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("agira task work --artifact"));
    }

    #[test]
    fn task_prompt_without_just_done_has_three_steps() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Next task", "", None, vec![]).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("1. Spawn a subagent"));
        assert!(prompt.contains("The configured model is `opus`"));
        assert!(prompt.contains("2. Pass the content"));
        assert!(prompt.contains("3. Once the subagent finishes"));
        assert!(!prompt.contains("Commit all changes"));
    }

    #[test]
    fn task_prompt_with_just_done_includes_commit_step() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Next task", "", None, vec![]).unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            Some(("task-000", "Previous Task")),
        );

        assert!(prompt.contains("1. Commit all changes from task-000 \"Previous Task\""));
        assert!(prompt.contains("2. Spawn a subagent"));
        assert!(prompt.contains("The configured model is `opus`"));
        assert!(prompt.contains("3. Pass the content"));
        assert!(prompt.contains("4. Once the subagent finishes"));
    }

    #[test]
    fn task_prompt_no_ansi() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement pick", "", None, vec![]).unwrap();

        let output = format_pick_output(&config, store.all_tasks(), None, None);

        assert!(!output.as_bytes().contains(&0x1B));
    }

    #[test]
    fn completion_summary_all_done() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("First done task", "", None, vec![]).unwrap();
        store
            .add_task("Second done task", "", None, vec![])
            .unwrap();
        for id in ["task-001", "task-002"] {
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
        }

        let output = format_pick_output(&config, store.all_tasks(), None, None);

        assert!(output.contains("# Agira Completion Summary"));
        assert!(output.contains("task-001"));
        assert!(output.contains("First done task"));
        assert!(output.contains("task-002"));
        assert!(output.contains("Second done task"));
    }

    #[test]
    fn verification_phase_detection() {
        let config = test_config();
        let mut config_with_commands = test_config();
        config_with_commands.verification.commands = vec!["cargo test".to_owned()];

        assert!(is_verification_phase("verifying", &config));
        assert!(is_verification_phase("enriching", &config_with_commands));
        assert!(!is_verification_phase("enriching", &config));
    }
}
