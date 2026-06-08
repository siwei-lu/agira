use std::cmp::{Ordering, Reverse};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};

use crate::core::{
    config::{Config, INITIAL_PHASE_NAME, TERMINAL_PHASE_NAME},
    tasks::{Task, TaskPhase},
};

/// Single source of truth for the advisory lock staleness threshold. A lock older than this is
/// treated as expired and the task becomes actionable again. Default: 1 hour.
const LOCK_STALE_AFTER_SECS: i64 = 3600;

const NO_TASKS_MESSAGE: &str = "No tasks found. Add tasks with `agira task add \"<title>\"`";
const BLOCKED_STATE: &str = "blocked";
const FAILED_STATE: &str = "failed";

pub(crate) fn format_pick_output(config: &Config, tasks: &[Task], state_dir: &Path) -> String {
    if tasks.is_empty() {
        return NO_TASKS_MESSAGE.to_owned();
    }

    if is_all_done(tasks, config) {
        return format_completion_summary(tasks);
    }

    if let Some(task) = select_next_task(tasks, config) {
        return format_task_prompt_output(task, config, state_dir);
    }

    format_non_actionable_summary(tasks)
}

pub(crate) fn select_next_task<'a>(all_tasks: &'a [Task], config: &Config) -> Option<&'a Task> {
    select_next_task_at(all_tasks, config, Utc::now())
}

fn select_next_task_at<'a>(
    all_tasks: &'a [Task],
    config: &Config,
    now: DateTime<Utc>,
) -> Option<&'a Task> {
    let terminal_phase = config.terminal_phase()?;

    all_tasks
        .iter()
        .filter(|task| {
            is_actionable(task, config)
                && !is_lock_live(task.locked_at.as_deref(), now)
                && deps_satisfied(task, all_tasks, terminal_phase)
        })
        .max_by_key(|task| {
            (
                task_phase_index(task, &task.state, config).unwrap_or(0),
                Reverse(task_id_number(&task.id)),
            )
        })
}

/// Returns true when the lock is present and NOT yet stale (i.e. the task should be skipped).
/// Fail-safe: an unparseable timestamp is treated as a live lock.
fn is_lock_live(locked_at: Option<&str>, now: DateTime<Utc>) -> bool {
    match locked_at {
        None => false,
        Some(ts) => match DateTime::parse_from_rfc3339(ts) {
            Err(_) => true,
            Ok(locked_time) => {
                let age_secs = (now - locked_time.with_timezone(&Utc)).num_seconds();
                age_secs < LOCK_STALE_AFTER_SECS
            }
        },
    }
}

fn phase_index(phase: &str, config: &Config) -> Option<usize> {
    config.phases.iter().position(|p| p.name == phase)
}

fn task_phase_names<'a>(task: &'a Task, config: &'a Config) -> Vec<&'a str> {
    task.state_machine
        .as_ref()
        .map(|machine| machine.iter().map(|phase| phase.name.as_str()).collect())
        .unwrap_or_else(|| {
            config
                .phases
                .iter()
                .map(|phase| phase.name.as_str())
                .collect()
        })
}

fn task_phase_index(task: &Task, phase: &str, config: &Config) -> Option<usize> {
    task_phase_names(task, config)
        .iter()
        .position(|candidate| *candidate == phase)
}

fn is_actionable(task: &Task, config: &Config) -> bool {
    let Some(terminal) = config.terminal_phase() else {
        return false;
    };

    !is_non_actionable_state(&task.state, terminal)
        && task_phase_index(task, &task.state, config).is_some()
}

fn is_non_actionable_state(state: &str, terminal_phase: &str) -> bool {
    state == BLOCKED_STATE || state == FAILED_STATE || state == terminal_phase
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

#[cfg(test)]
fn format_task_prompt(task: &Task, config: &Config, state_dir: &Path) -> String {
    format_task_prompt_output(task, config, state_dir)
}

fn format_task_prompt_output(task: &Task, config: &Config, state_dir: &Path) -> String {
    if task.state == INITIAL_PHASE_NAME {
        return "Run: agira task todo --artifact \"task accepted\" to advance this task to the next phase."
            .to_owned();
    }

    let model = effective_task_model(task, &task.state, config);
    let duty = effective_task_duty(task, &task.state, config);

    let mut subagent = format!(
        "# Agira Task Prompt\n\n## Task\n- ID: {}\n- Title: {}\n- Current phase: {}",
        task.id, task.title, task.state,
    );
    if let Some(m) = model {
        subagent.push_str(&format!("\n- Agent role: {m}"));
    }
    subagent.push_str(&format!(
        "\n\n## Description\n{}",
        if task.description.is_empty() {
            "No description provided."
        } else {
            task.description.as_str()
        }
    ));

    if description_warning_applies(task, config) {
        let description_len = task.description.chars().count();
        subagent.push_str(&format!(
            "\n\n## Description Quality Warning\nThis description is short ({description_len} chars). If the enriching phase did not produce a complete spec, update it before proceeding:\n  agira task update {} --description \"<complete spec>\"\nOr block for clarification:\n  agira task block {} --reason \"description too thin to implement\"",
            task.id, task.id
        ));
    }

    if let Some(duty) = duty {
        subagent.push_str(&format!("\n\n## Phase Duty\n{duty}"));
    }

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

    if task.state != INITIAL_PHASE_NAME && task.state != TERMINAL_PHASE_NAME {
        let attachments_path = state_dir.join("attachments").join(&task.id);
        subagent.push_str(&format!(
            "\n\n## Attachments\nSave evidence files (screenshots, recordings, test output) to:\n{}/\nCreate the directory if it does not exist. Reference saved files in your --artifact text.",
            attachments_path.display()
        ));
    }

    subagent.push_str(&format!(
        "\n\n## Checkpoints\nIf you are not confident about a decision and human input is required, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Run:\n`agira task block {} --reason \"<explanation>\"`",
        task.id
    ));

    subagent
}

fn description_warning_applies(task: &Task, config: &Config) -> bool {
    let short = task.description.chars().count() < 150;
    let post_enriching = match phase_index("enriching", config) {
        Some(enriching_index) => phase_index(&task.state, config)
            .is_some_and(|current_index| current_index > enriching_index),
        None => task.state != INITIAL_PHASE_NAME,
    };

    short && post_enriching
}

fn effective_phase_model<'a>(
    phase: &'a crate::core::config::PhaseConfig,
    config: &'a Config,
) -> Option<&'a str> {
    if phase.name == INITIAL_PHASE_NAME || phase.name == TERMINAL_PHASE_NAME {
        return None;
    }

    phase.model.as_deref().or(config.default_model.as_deref())
}

fn effective_task_model<'a>(task: &'a Task, phase: &str, config: &'a Config) -> Option<&'a str> {
    if let Some(machine) = task.state_machine.as_ref() {
        let phase_cfg = machine.iter().find(|candidate| candidate.name == phase)?;
        if phase_cfg.name == INITIAL_PHASE_NAME || phase_cfg.name == TERMINAL_PHASE_NAME {
            return None;
        }

        return phase_cfg
            .model
            .as_deref()
            .or(config.default_model.as_deref());
    }

    config
        .phases
        .iter()
        .find(|candidate| candidate.name == phase)
        .and_then(|phase_cfg| effective_phase_model(phase_cfg, config))
}

fn effective_task_duty<'a>(task: &'a Task, phase: &str, config: &'a Config) -> Option<&'a str> {
    if let Some(machine) = task.state_machine.as_ref() {
        if let Some(phase_cfg) = machine.iter().find(|p| p.name == phase) {
            if let Some(duty) = phase_cfg.duty.as_deref().filter(|d| !d.is_empty()) {
                return Some(duty);
            }
            // task-level phase has no duty; fall back to global config phase of same name
            return config
                .phases
                .iter()
                .find(|p| p.name == phase)
                .and_then(|p| p.duty.as_deref())
                .filter(|d| !d.is_empty());
        }
        return None;
    }

    config
        .phases
        .iter()
        .find(|p| p.name == phase)
        .and_then(|p| p.duty.as_deref())
        .filter(|d| !d.is_empty())
}

fn prior_phases<'a>(task: &'a Task, config: &'a Config) -> Vec<(&'a str, &'a TaskPhase)> {
    let Some(current_index) = task_phase_index(task, &task.state, config) else {
        return Vec::new();
    };

    task_phase_names(task, config)
        .iter()
        .take(current_index)
        .filter_map(|phase_name| {
            task.phases
                .get(*phase_name)
                .map(|phase| (*phase_name, phase))
        })
        .collect()
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
    let mut output = "# Agira Task Summary\n\nNo actionable tasks found. All remaining tasks are blocked, failed, or complete.\n\n## Non-actionable Tasks".to_owned();
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
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::core::config::PhaseConfig;
    use crate::core::tasks::{TaskPhaseConfig, TaskStore};

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "verifying".to_owned(),
                    model: Some("haiku".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            max_retries: 3,
        }
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    fn custom_state_machine() -> Vec<TaskPhaseConfig> {
        vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "security_review".to_owned(),
                model: Some("opus".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ]
    }

    #[test]
    fn no_tasks_message() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let store = test_store(&temp_dir, &config);

        let output = format_pick_output(&config, store.all_tasks(), temp_dir.path());

        assert_eq!(output, NO_TASKS_MESSAGE);
    }

    #[test]
    fn select_most_advanced_phase_wins() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Later phase", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store
            .add_task("Earlier phase", "", vec![], None, None)
            .unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn select_lowest_id_within_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("First", "", vec![], None, None).unwrap();
        store.add_task("Second", "", vec![], None, None).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn test_blocked_task_not_selected() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Blocking task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Blocked task", "", vec!["task-001".to_owned()], None, None)
            .unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn custom_machine_task_is_actionable() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task(
                "Security review",
                "",
                vec![],
                Some("security_review"),
                Some(custom_state_machine()),
            )
            .unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
        assert_eq!(selected.state, "security_review");
    }

    #[test]
    fn select_skips_blocked_state_even_if_configured_as_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.phases.insert(
            1,
            PhaseConfig {
                name: "blocked".to_owned(),
                model: Some("haiku".to_owned()),
                duty: None,
            },
        );
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Blocked current task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Next actionable task", "", vec![], None, None)
            .unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-002");
    }

    #[test]
    fn select_skips_failed_state_even_if_configured_as_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.phases.insert(
            1,
            PhaseConfig {
                name: "failed".to_owned(),
                model: Some("haiku".to_owned()),
                duty: None,
            },
        );
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Failed current task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Next actionable task", "", vec![], None, None)
            .unwrap();
        store.fail_task("task-001", "broken").unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-002");
    }

    #[test]
    fn non_actionable_summary_explains_remaining_tasks_are_blocked_failed_or_complete() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Blocked task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Failed task", "", vec![], None, None)
            .unwrap();
        store.block_task("task-001", "waiting").unwrap();
        store.fail_task("task-002", "broken").unwrap();

        let output = format_pick_output(&config, store.all_tasks(), temp_dir.path());

        assert!(output.contains("All remaining tasks are blocked, failed, or complete."));
        assert!(output.contains("task-001: Blocked task (blocked)"));
        assert!(output.contains("task-002: Failed task (failed)"));
    }

    #[test]
    fn task_prompt_returns_raw_task_data_without_orchestrator_wrapper() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.starts_with("# Agira Task Prompt"));
        assert!(prompt.contains("- Agent role: opus"));
        assert!(prompt.contains("## Checkpoints"));
        assert!(!prompt.contains("## Advance State"));
        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("--- END SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("Do NOT perform this work yourself"));
        assert!(!prompt.contains("Use the Agent tool"));
    }

    #[test]
    fn model_lookup_honors_task_machine() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task(
                "Security review",
                "",
                vec![],
                Some("security_review"),
                Some(custom_state_machine()),
            )
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("- Agent role: opus"));
        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("The configured model is `opus`"));
    }

    #[test]
    fn task_prompt_includes_phase_duty_when_current_phase_defines_one() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "enriching")
            .unwrap()
            .duty = Some("investigate and write a plan".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("investigate and write a plan"));
    }

    #[test]
    fn duty_falls_back_to_global() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "in_progress")
            .unwrap()
            .duty = Some("write the implementation".to_owned());
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "in_progress".to_owned(),
                model: Some("sonnet".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "security_review".to_owned(),
                model: Some("opus".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task(
                "Global duty",
                "",
                vec![],
                Some("in_progress"),
                Some(state_machine.clone()),
            )
            .unwrap();
        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("write the implementation"));

        store
            .add_task(
                "No global duty",
                "",
                vec![],
                Some("security_review"),
                Some(state_machine),
            )
            .unwrap();
        let prompt = format_task_prompt(
            store.get_task("task-002").unwrap(),
            &config,
            temp_dir.path(),
        );
        assert!(!prompt.contains("## Phase Duty"));
    }

    #[test]
    fn task_prompt_omits_phase_duty_when_current_phase_duty_is_none() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Phase Duty"));
    }

    #[test]
    fn task_prompt_omits_phase_duty_when_current_phase_duty_is_empty() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "enriching")
            .unwrap()
            .duty = Some(String::new());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Phase Duty"));
    }

    #[test]
    fn task_prompt_places_phase_duty_before_acceptance_criteria() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "enriching")
            .unwrap()
            .duty = Some("investigate and write a plan".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store
            .record_phase_artifact(
                "task-001",
                "accepted pending task",
                "2026-06-07T00:00:00+00:00".to_owned(),
            )
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let duty_index = prompt.find("## Phase Duty").unwrap();
        let acceptance_index = prompt.find("## Acceptance Criteria").unwrap();

        assert!(duty_index < acceptance_index);
    }

    #[test]
    fn active_task_prompt_includes_attachments_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let expected = temp_dir
            .path()
            .join("attachments")
            .join("task-001")
            .display()
            .to_string();

        assert!(prompt.contains("## Attachments"));
        assert!(prompt.contains(&expected));
    }

    #[test]
    fn pending_task_prompt_omits_attachments() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Pending task", "", vec![], None, None)
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Attachments"));
        assert_eq!(
            prompt,
            "Run: agira task todo --artifact \"task accepted\" to advance this task to the next phase."
        );
    }

    #[test]
    fn terminal_task_prompt_omits_attachments() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Done task", "", vec![], None, None).unwrap();
        for _ in 0..4 {
            store.next_phase("task-001").unwrap();
        }

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Attachments"));
    }

    #[test]
    fn task_prompt_places_attachments_after_acceptance_criteria() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "in_progress")
            .unwrap()
            .duty = Some("implement the task".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Ordered attachments", "", vec![], None, None)
            .unwrap();
        store
            .record_phase_artifact(
                "task-001",
                "accepted pending task",
                "2026-06-07T00:00:00+00:00".to_owned(),
            )
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let acceptance_index = prompt.find("## Acceptance Criteria").unwrap();
        let attachments_index = prompt.find("## Attachments").unwrap();

        assert!(acceptance_index < attachments_index);
    }

    #[test]
    fn warning_shown_for_thin_desc_post_enriching() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "thin description", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Description Quality Warning"));
        assert!(prompt.contains("chars)"));
        assert!(prompt.contains("agira task update task-001 --description"));
        assert!(
            prompt.contains(
                "agira task block task-001 --reason \"description too thin to implement\""
            )
        );
    }

    #[test]
    fn no_warning_for_long_desc_post_enriching() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let description = "x".repeat(150);

        store
            .add_task("Implement pick", &description, vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Description Quality Warning"));
    }

    #[test]
    fn description_warning_boundary_is_strictly_less_than_150_chars() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let description_149 = "x".repeat(149);
        let description_150 = "y".repeat(150);

        store
            .add_task("Short boundary", &description_149, vec![], None, None)
            .unwrap();
        store
            .add_task("Long boundary", &description_150, vec![], None, None)
            .unwrap();
        for id in ["task-001", "task-002"] {
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
        }

        let short_prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let long_prompt = format_task_prompt(
            store.get_task("task-002").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(short_prompt.contains("## Description Quality Warning"));
        assert!(!long_prompt.contains("## Description Quality Warning"));
    }

    #[test]
    fn no_warning_in_enriching_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Enrich task", "thin description", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Description Quality Warning"));
    }

    #[test]
    fn no_warning_in_pending_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Pending task", "thin description", vec![], None, None)
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Description Quality Warning"));
    }

    #[test]
    fn empty_desc_post_enriching_warns() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Empty description", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Description Quality Warning"));
        assert!(prompt.contains("(0 chars"));
    }

    #[test]
    fn warning_placed_after_description_before_phase_duty() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "in_progress")
            .unwrap()
            .duty = Some("implement the task".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Ordered sections", "thin description", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let description_index = prompt.find("## Description").unwrap();
        let warning_index = prompt.find("## Description Quality Warning").unwrap();
        let duty_index = prompt.find("## Phase Duty").unwrap();

        assert!(description_index < warning_index);
        assert!(warning_index < duty_index);
    }

    #[test]
    fn enriching_absent_branch_warns_after_pending_only() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: Some("sonnet".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            max_retries: 3,
        };
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("In progress task", "thin description", vec![], None, None)
            .unwrap();
        store
            .add_task("Pending task", "thin description", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let in_progress_prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );
        let pending_prompt = format_task_prompt(
            store.get_task("task-002").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(in_progress_prompt.contains("## Description Quality Warning"));
        assert!(!pending_prompt.contains("## Description Quality Warning"));
    }

    #[test]
    fn explicit_phase_model_wins_over_default_model() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.default_model = Some("codex".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("- Agent role: opus"));
        assert!(!prompt.contains("The configured model is `opus`"));
        assert!(!prompt.contains("- Agent role: codex"));
    }

    #[test]
    fn pending_phase_prompt_has_no_orchestrator_wrapper() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.default_model = Some("codex".to_owned());
        let mut store = test_store(&temp_dir, &config);

        // Task in pending phase — transition phase, no model, no orchestrator wrapper.
        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("--- END SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("# Agira Task Prompt"));
        assert!(!prompt.contains("- Agent role:"));
    }

    #[test]
    fn task_prompt_omits_agent_role_for_unknown_phase_with_default_model() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.default_model = Some("codex".to_owned());
        // Remove in_progress so the task lands in an unknown phase with no model.
        config.phases.retain(|p| p.name != "in_progress");
        let mut store = test_store(&temp_dir, &test_config());

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        // Task is in in_progress state but config has no in_progress phase: model is None.
        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("- Agent role:"));
        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
    }

    #[test]
    fn model_less_non_mandatory_phase_uses_configured_default_model() {
        let temp_dir = TempDir::new().unwrap();
        // Build a config with a model-less middle phase "triage".
        let config = Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "triage".to_owned(),
                    model: None, // non-mandatory, no model
                    duty: None,
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: Some("codex".to_owned()),
            max_retries: 3,
        };
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Triage work", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap(); // pending -> triage

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("# Agira Task Prompt"));
        assert!(prompt.contains("- Agent role: codex"));
        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("The configured model is `codex`"));
    }

    #[test]
    fn model_less_non_mandatory_phase_without_default_stays_model_less() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "triage".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            max_retries: 3,
        };
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Triage work", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("- Agent role:"));
    }

    #[test]
    fn task_prompt_contains_todo_command() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement work", "", vec![], None, None)
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("agira task todo --artifact"));
    }

    #[test]
    fn pending_task_prompt_tells_agent_to_accept_and_advance() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Accept work", "", vec![], None, None)
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert_eq!(
            prompt,
            "Run: agira task todo --artifact \"task accepted\" to advance this task to the next phase."
        );
        for section in [
            "# Agira Task Prompt",
            "# Agira Orchestrator Instructions",
            "--- SUBAGENT PROMPT ---",
            "--- END SUBAGENT PROMPT ---",
            "## Description",
            "## Phase Duty",
            "## Acceptance Criteria",
            "## Attachments",
            "## Pending Phase",
            "## Checkpoints",
            "## Advance State",
            "- Agent role:",
        ] {
            assert!(
                !prompt.contains(section),
                "pending prompt unexpectedly contained {section}"
            );
        }
    }

    #[test]
    fn non_pending_task_prompt_omits_pending_phase_instruction() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Continue work", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Pending Phase"));
        assert!(!prompt.contains("This task is currently in the pending phase."));
        assert!(
            !prompt
                .contains("You are expected to accept the task and advance it, not just read it.")
        );
        assert!(!prompt.contains("agira task todo --artifact \"<evidence>\""));
    }

    #[test]
    fn prior_phases_honor_task_machine() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "security_review".to_owned(),
                model: Some("opus".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "compliance_review".to_owned(),
                model: Some("haiku".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task("Custom order", "", vec![], None, Some(state_machine))
            .unwrap();
        store
            .record_phase_artifact(
                "task-001",
                "pending artifact",
                "2026-06-06T00:00:00Z".into(),
            )
            .unwrap();
        store.next_phase("task-001").unwrap();
        store
            .record_phase_artifact(
                "task-001",
                "security artifact",
                "2026-06-06T01:00:00Z".into(),
            )
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Acceptance Criteria"));
        let pending_index = prompt.find("Phase: pending").unwrap();
        let security_index = prompt.find("Phase: security_review").unwrap();
        assert!(pending_index < security_index);
        assert!(prompt.contains("Artifact: pending artifact"));
        assert!(prompt.contains("Artifact: security artifact"));
    }

    #[test]
    fn task_prompt_contains_checkpoint_block_instruction() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Clarify requirements", "", vec![], None, None)
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("not confident about a decision"));
        assert!(prompt.contains("human input is required"));
        assert!(prompt.contains("Blocking is the correct escalation path"));
        assert!(prompt.contains("not a last resort"));
        assert!(prompt.contains("agira task block task-001 --reason \"<explanation>\""));
    }

    #[test]
    fn task_prompt_model_variants_share_unified_raw_format() {
        let temp_dir = TempDir::new().unwrap();
        let baseline_config = test_config();
        let mut baseline_store = test_store(&temp_dir, &baseline_config);
        baseline_store
            .add_task(
                "Unified prompt",
                "implement unified raw output",
                vec![],
                None,
                None,
            )
            .unwrap();
        baseline_store.next_phase("task-001").unwrap();

        let variants = [
            Some("dispatch -a codex".to_owned()),
            Some("codex".to_owned()),
            None,
        ];
        let mut normalized_prompts = Vec::new();

        for model in variants {
            let mut config = test_config();
            config
                .phases
                .iter_mut()
                .find(|phase| phase.name == "enriching")
                .unwrap()
                .model = model;
            let store = test_store(&temp_dir, &config);

            let prompt = format_task_prompt(
                store.get_task("task-001").unwrap(),
                &config,
                temp_dir.path(),
            );

            assert!(prompt.starts_with("# Agira Task Prompt"));
            assert!(prompt.contains("## Checkpoints"));
            assert!(!prompt.contains("## Advance State"));
            assert!(!prompt.contains("# Agira Orchestrator Instructions"));
            assert!(!prompt.contains("--- SUBAGENT PROMPT ---"));
            assert!(!prompt.contains("--- END SUBAGENT PROMPT ---"));
            assert!(!prompt.contains("dispatch -a codex exec"));
            assert!(!prompt.contains("Use the Agent tool"));

            let normalized = prompt
                .lines()
                .filter(|line| !line.starts_with("- Agent role:"))
                .collect::<Vec<_>>()
                .join("\n");
            normalized_prompts.push(normalized);
        }

        assert_eq!(normalized_prompts[0], normalized_prompts[1]);
        assert_eq!(normalized_prompts[0], normalized_prompts[2]);
    }

    #[test]
    fn task_prompt_no_ansi() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Implement pick", "", vec![], None, None)
            .unwrap();

        let output = format_pick_output(&config, store.all_tasks(), temp_dir.path());

        assert!(!output.as_bytes().contains(&0x1B));
    }

    #[test]
    fn completion_summary_all_done() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("First done task", "", vec![], None, None)
            .unwrap();
        store
            .add_task("Second done task", "", vec![], None, None)
            .unwrap();
        for id in ["task-001", "task-002"] {
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
        }

        let output = format_pick_output(&config, store.all_tasks(), temp_dir.path());

        assert!(output.contains("# Agira Completion Summary"));
        assert!(output.contains("task-001"));
        assert!(output.contains("First done task"));
        assert!(output.contains("task-002"));
        assert!(output.contains("Second done task"));
    }

    #[test]
    fn verifying_task_prompt_does_not_include_verification_commands_section() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        let duty = "Run cargo fmt -- --check, cargo test, cargo clippy -- -D warnings. All must pass. Then advance.";
        config
            .phases
            .iter_mut()
            .find(|phase| phase.name == "verifying")
            .unwrap()
            .duty = Some(duty.to_owned());
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task(
                "Verify work",
                "Run final verification for the implemented change.",
                vec![],
                None,
                None,
            )
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains(duty));
        assert!(!prompt.contains("## Verification Commands"));
    }

    #[test]
    fn task_machine_duty_shown_when_stored_on_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "security_review".to_owned(),
                model: Some("opus".to_owned()),
                duty: Some("Check for SQL injection vulnerabilities".to_owned()),
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task(
                "Custom duty task",
                "",
                vec![],
                Some("security_review"),
                Some(state_machine),
            )
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("Check for SQL injection vulnerabilities"));
    }

    #[test]
    fn task_machine_duty_falls_back_to_global_when_phase_matches() {
        // A phase reused from global config carries duty: None in state_machine
        // but duty is still shown by falling back to the global config's phase duty.
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config
            .phases
            .iter_mut()
            .find(|p| p.name == "enriching")
            .unwrap()
            .duty = Some("global enriching duty".to_owned());
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "enriching".to_owned(),
                model: Some("opus".to_owned()),
                duty: None, // no task-level duty; should fall back to global
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task(
                "Fallback task",
                "",
                vec![],
                Some("enriching"),
                Some(state_machine),
            )
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("global enriching duty"));
    }

    #[test]
    fn task_machine_new_phase_with_no_global_duty_shows_task_duty() {
        // A new phase (not in global) with a duty stored on the task's state_machine
        // shows that duty and does NOT fall back to anything.
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "new_custom_phase".to_owned(),
                model: Some("opus".to_owned()),
                duty: Some("custom phase specific duty".to_owned()),
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task(
                "New phase task",
                "",
                vec![],
                Some("new_custom_phase"),
                Some(state_machine),
            )
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("custom phase specific duty"));
    }

    #[test]
    fn task_machine_new_phase_without_duty_omits_section() {
        // A new phase (not in global) with duty: None → no duty section.
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        let state_machine = vec![
            TaskPhaseConfig {
                name: "pending".to_owned(),
                model: None,
                duty: None,
            },
            TaskPhaseConfig {
                name: "new_custom_phase".to_owned(),
                model: Some("opus".to_owned()),
                duty: None,
            },
            TaskPhaseConfig {
                name: "done".to_owned(),
                model: None,
                duty: None,
            },
        ];

        store
            .add_task(
                "No duty task",
                "",
                vec![],
                Some("new_custom_phase"),
                Some(state_machine),
            )
            .unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            temp_dir.path(),
        );

        assert!(!prompt.contains("## Phase Duty"));
    }

    #[test]
    fn fresh_locked_task_is_skipped_by_select() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Only task", "", vec![], None, None).unwrap();
        store.lock_task("task-001").unwrap();

        let now = Utc::now();
        let result = select_next_task_at(store.all_tasks(), &config, now);
        assert!(result.is_none(), "fresh-locked task should not be selected");
    }

    #[test]
    fn fresh_locked_task_skipped_next_actionable_selected() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Locked task", "", vec![], None, None)
            .unwrap();
        store.add_task("Free task", "", vec![], None, None).unwrap();
        store.lock_task("task-001").unwrap();

        let now = Utc::now();
        let selected = select_next_task_at(store.all_tasks(), &config, now).unwrap();
        assert_eq!(selected.id, "task-002");
    }

    #[test]
    fn stale_locked_task_is_selected() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", vec![], None, None).unwrap();
        store.lock_task("task-001").unwrap();

        // Inject a `now` that is 2 hours after the lock was set — stale
        let now = Utc::now() + chrono::Duration::seconds(LOCK_STALE_AFTER_SECS + 1);
        let selected = select_next_task_at(store.all_tasks(), &config, now).unwrap();
        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn unparseable_locked_at_treated_as_live_lock() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", vec![], None, None).unwrap();

        // Write a corrupt locked_at directly
        let tasks_path = temp_dir.path().join("tasks.json");
        let json = fs::read_to_string(&tasks_path).unwrap_or_else(|_| {
            r#"{"tasks":[{"id":"task-001","title":"Task","description":"","state":"pending","dependencies":[],"retry_count":0,"max_retries":3,"phases":{},"history":[{"from":null,"to":"pending","timestamp":"2026-01-01T00:00:00Z","reason":"task created"}],"created_at":"2026-01-01T00:00:00Z"}]}"#.to_owned()
        });
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["tasks"][0]["locked_at"] = serde_json::Value::String("not-a-timestamp".to_owned());
        fs::write(&tasks_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let store2 = TaskStore::new(temp_dir.path(), &config).unwrap();
        let now = Utc::now();
        let result = select_next_task_at(store2.all_tasks(), &config, now);
        assert!(
            result.is_none(),
            "unparseable locked_at should be treated as live lock"
        );
    }

    #[test]
    fn unlocked_task_is_always_actionable() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Task", "", vec![], None, None).unwrap();
        assert!(store.get_task("task-001").unwrap().locked_at.is_none());

        let now = Utc::now();
        let selected = select_next_task_at(store.all_tasks(), &config, now).unwrap();
        assert_eq!(selected.id, "task-001");
    }
}
