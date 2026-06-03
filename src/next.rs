use std::{
    cmp::Ordering,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, FixedOffset};
use thiserror::Error;

use crate::{
    config::Config,
    project::Project,
    tasks::{StoreError, Task, TaskPhase, TaskStore},
};

const NO_TASKS_MESSAGE: &str = "No tasks found. Add tasks with `agira add \"<title>\"` or provide requirements with `agira next --prd <path>`";

#[derive(Debug, Error)]
pub enum NextError {
    #[error("prd file not found: {path}")]
    PrdNotFound { path: PathBuf },

    #[error("failed to read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to load config {path}: {source}")]
    ConfigLoad {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    StoreError(#[from] StoreError),
}

pub fn run_next(project: &Project, prd_path: Option<&Path>) -> Result<(), NextError> {
    let config_path = project.state_dir.join("config.json");
    let config = load_config(&config_path)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let prd_content = prd_path.map(read_prd).transpose()?;
    let output = format_next_output(&config, store.all_tasks(), prd_content.as_deref());

    println!("{output}");
    Ok(())
}

fn load_config(path: &Path) -> Result<Config, NextError> {
    let contents = fs::read_to_string(path).map_err(|source| NextError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    serde_json::from_str(&contents).map_err(|source| NextError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })
}

fn read_prd(path: &Path) -> Result<String, NextError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(NextError::PrdNotFound {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(NextError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn format_next_output(config: &Config, tasks: &[Task], prd_content: Option<&str>) -> String {
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
        return format_task_prompt(task, config);
    }

    format_non_actionable_summary(tasks)
}

fn select_next_task<'a>(tasks: &'a [Task], config: &Config) -> Option<&'a Task> {
    tasks
        .iter()
        .filter(|task| is_actionable(task, config))
        .min_by_key(|task| {
            (
                phase_index(&task.state, config).unwrap_or(usize::MAX),
                task_id_number(&task.id),
            )
        })
}

fn phase_index(phase: &str, config: &Config) -> Option<usize> {
    config
        .state_machine
        .iter()
        .position(|candidate| candidate == phase)
}

fn is_actionable(task: &Task, config: &Config) -> bool {
    config.state_machine.last().is_some_and(|terminal_phase| {
        task.state != *terminal_phase && phase_index(&task.state, config).is_some()
    })
}

fn is_all_done(tasks: &[Task], config: &Config) -> bool {
    !tasks.is_empty()
        && config
            .state_machine
            .last()
            .is_some_and(|terminal_phase| tasks.iter().all(|task| task.state == *terminal_phase))
}

fn format_decomposition_prompt(prd_content: &str) -> String {
    format!(
        "# Agira PRD Decomposition\n\n## Role\nYou are the planner for this Agira project.\n\n## Objective\nBreak the requirements into small, actionable Agira tasks. Add each task with agira add.\n\n## Commands\nFor each task, run:\n`agira add \"<title>\" --description \"<description>\"`\n\n## Requirements Context\n{prd_content}"
    )
}

fn format_task_prompt(task: &Task, config: &Config) -> String {
    let role = config.models.get(&task.state).unwrap_or(&task.state);
    let description = if task.description.is_empty() {
        "No description provided."
    } else {
        task.description.as_str()
    };
    let mut output = format!(
        "# Agira Task Prompt\n\n## Task\n- ID: {}\n- Title: {}\n- Current phase: {}\n- Agent role: {}\n\n## Description\n{}",
        task.id, task.title, task.state, role, description
    );

    if !task.phases.is_empty() {
        output.push_str("\n\n## Acceptance Criteria");
        for (phase_name, phase) in prior_phases(task, config) {
            let artifact = if phase.artifact.is_empty() {
                "<empty>"
            } else {
                phase.artifact.as_str()
            };
            output.push_str(&format!(
                "\n- Phase: {phase_name}\n  Completed at: {}\n  Artifact: {artifact}",
                phase.completed_at
            ));
        }
    }

    if is_verification_phase(&task.state, config) {
        output.push_str("\n\n## Verification Commands");
        if config.verification.commands.is_empty() {
            output.push_str("\nNo verification commands configured.");
        } else {
            for command in &config.verification.commands {
                output.push_str(&format!("\n- `{command}`"));
            }
        }
    }

    output.push_str(&format!(
        "\n\n## Advance State\nWhen this phase is complete, run:\n`agira done {} --artifact \"<artifact>\"`\n\nIf this phase cannot be completed, run:\n`agira fail {} --reason \"<reason>\"`",
        task.id, task.id
    ));

    output
}

fn prior_phases<'a>(task: &'a Task, config: &'a Config) -> Vec<(&'a str, &'a TaskPhase)> {
    let Some(current_index) = phase_index(&task.state, config) else {
        return Vec::new();
    };

    config
        .state_machine
        .iter()
        .take(current_index)
        .filter_map(|phase_name| {
            task.phases
                .get(phase_name)
                .map(|phase| (phase_name.as_str(), phase))
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
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::config::VerificationConfig;

    fn test_config() -> Config {
        let mut models = BTreeMap::new();
        models.insert("enriching".to_owned(), "enricher".to_owned());
        models.insert("in_progress".to_owned(), "implementer".to_owned());
        models.insert("verifying".to_owned(), "verifier".to_owned());

        Config {
            stack: "rust".to_owned(),
            state_machine: vec![
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "verifying".to_owned(),
                "done".to_owned(),
            ],
            models,
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

        let output = format_next_output(&config, store.all_tasks(), None);

        assert_eq!(output, NO_TASKS_MESSAGE);
    }

    #[test]
    fn no_tasks_with_prd_decomposition() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let store = test_store(&temp_dir, &config);
        let prd_text = "Build the next command from these requirements.";

        let output = format_next_output(&config, store.all_tasks(), Some(prd_text));

        assert!(output.contains("agira add"));
        assert!(output.contains(prd_text));
    }

    #[test]
    fn select_earliest_phase_wins() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Later phase", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.add_task("Earlier phase", "", None, vec![]).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-002");
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
    fn task_prompt_contains_role() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement next", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config);

        assert!(prompt.contains("- Agent role: implementer"));
    }

    #[test]
    fn task_prompt_contains_done_command() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement next", "", None, vec![]).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config);

        assert!(prompt.contains("agira done task-001 --artifact"));
    }

    #[test]
    fn task_prompt_no_ansi() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement next", "", None, vec![]).unwrap();

        let output = format_next_output(&config, store.all_tasks(), None);

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

        let output = format_next_output(&config, store.all_tasks(), None);

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
