use std::cmp::{Ordering, Reverse};

use chrono::{DateTime, FixedOffset};

use crate::core::{
    config::{Config, INITIAL_PHASE_NAME, TERMINAL_PHASE_NAME},
    tasks::{Task, TaskPhase},
};

const NO_TASKS_MESSAGE: &str = "No tasks found. Add tasks with `agira task add \"<title>\"`";
const BLOCKED_STATE: &str = "blocked";
const FAILED_STATE: &str = "failed";

pub(crate) fn format_pick_output(
    config: &Config,
    tasks: &[Task],
    just_done: Option<(&str, &str)>,
) -> String {
    if tasks.is_empty() {
        return NO_TASKS_MESSAGE.to_owned();
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
    let Some(terminal) = config.terminal_phase() else {
        return false;
    };

    !is_non_actionable_state(&task.state, terminal) && phase_index(&task.state, config).is_some()
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

fn format_task_prompt(task: &Task, config: &Config, _just_done: Option<(&str, &str)>) -> String {
    let phase_cfg = config.phases.iter().find(|p| p.name == task.state);
    let model: Option<&str> = phase_cfg.and_then(|p| effective_phase_model(p, config));
    let duty = phase_cfg
        .and_then(|p| p.duty.as_deref())
        .filter(|d| !d.is_empty());

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

    if task.state == INITIAL_PHASE_NAME {
        subagent.push_str("\n\n## Pending Phase\nThis task is currently in the pending phase. You are expected to accept the task and advance it, not just read it. You must call `agira task todo --artifact \"<evidence>\"` to move the task forward to the next phase.");
    }

    subagent.push_str(&format!(
        "\n\n## Checkpoints\nIf you are not confident about a decision and human input is required, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Run:\n`agira task block {} --reason \"<explanation>\"`",
        task.id
    ));

    subagent.push_str(&format!(
        "\n\n## Advance State\nWhen this phase is complete, run:\n`agira task todo --artifact \"<artifact>\"`\n\nIf this phase cannot be completed, run:\n`agira task fail {} --reason \"<reason>\"`",
        task.id
    ));

    // Orchestrator wrapper: only present when the phase has a model (i.e. AI-driven).
    if let Some(model) = model {
        let steps = format!(
            "1. Read the task title and description exactly as given in the subagent prompt below.\n2. Write a SHORT, CLEAR problem statement for the subagent based solely on that title and description. Do not add assumptions, repo context, or findings from any other source.\n3. Spawn a subagent using model `{model}` with that problem statement and the delimited prompt below."
        );

        format!(
            "# Agira Orchestrator Instructions\n\nYou are the **orchestrator** for this task. Do NOT perform this work yourself. This includes investigation and analysis: do not read files, explore the codebase, run commands, or try to understand the problem before delegating.\n\nYour ONLY job is:\n{steps}\n\nThe configured model is `{model}`.\nThe subagent is responsible for all investigation, reasoning, analysis, file reading, command execution, implementation, verification, and state advancement.\n\n--- SUBAGENT PROMPT ---\n{subagent}\n--- END SUBAGENT PROMPT ---"
        )
    } else {
        // No effective model: return subagent prompt directly without orchestrator wrapper.
        subagent
    }
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
    use tempfile::TempDir;

    use super::*;
    use crate::core::config::{PhaseConfig, VerificationConfig};
    use crate::core::tasks::TaskStore;

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
            verification: VerificationConfig { commands: vec![] },
            max_retries: 3,
        }
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    #[test]
    fn no_tasks_message() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let store = test_store(&temp_dir, &config);

        let output = format_pick_output(&config, store.all_tasks(), None);

        assert_eq!(output, NO_TASKS_MESSAGE);
    }

    #[test]
    fn select_most_advanced_phase_wins() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Later phase", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.add_task("Earlier phase", "", vec![], None).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn select_lowest_id_within_phase() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("First", "", vec![], None).unwrap();
        store.add_task("Second", "", vec![], None).unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn test_blocked_task_not_selected() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Blocking task", "", vec![], None).unwrap();
        store
            .add_task("Blocked task", "", vec!["task-001".to_owned()], None)
            .unwrap();

        let selected = select_next_task(store.all_tasks(), &config).unwrap();

        assert_eq!(selected.id, "task-001");
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
            .add_task("Blocked current task", "", vec![], None)
            .unwrap();
        store
            .add_task("Next actionable task", "", vec![], None)
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
            .add_task("Failed current task", "", vec![], None)
            .unwrap();
        store
            .add_task("Next actionable task", "", vec![], None)
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

        store.add_task("Blocked task", "", vec![], None).unwrap();
        store.add_task("Failed task", "", vec![], None).unwrap();
        store.block_task("task-001", "waiting").unwrap();
        store.fail_task("task-002", "broken").unwrap();

        let output = format_pick_output(&config, store.all_tasks(), None);

        assert!(output.contains("All remaining tasks are blocked, failed, or complete."));
        assert!(output.contains("task-001: Blocked task (blocked)"));
        assert!(output.contains("task-002: Failed task (failed)"));
    }

    #[test]
    fn task_prompt_has_orchestrator_preamble_and_delimiters() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        // Advance to enriching phase which has a model (opus) — orchestrator wrapper is present.
        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("# Agira Orchestrator Instructions"));
        assert!(prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(prompt.contains("--- END SUBAGENT PROMPT ---"));
        assert!(prompt.contains("# Agira Task Prompt"));
        assert!(prompt.contains("- Agent role: opus"));
        assert!(prompt.contains("The configured model is `opus`"));
        assert!(prompt.contains("Do NOT perform this work yourself"));
        assert!(prompt.contains("do not read files"));
        assert!(prompt.contains("explore the codebase"));
        assert!(prompt.contains("run commands"));
        assert!(prompt.contains("try to understand the problem before delegating"));
        assert!(prompt.contains("The subagent is responsible for all investigation"));
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

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("## Phase Duty"));
        assert!(prompt.contains("investigate and write a plan"));
    }

    #[test]
    fn task_prompt_omits_phase_duty_when_current_phase_duty_is_none() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

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

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

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

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store
            .record_phase_artifact(
                "task-001",
                "accepted pending task",
                "2026-06-07T00:00:00+00:00".to_owned(),
            )
            .unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);
        let duty_index = prompt.find("## Phase Duty").unwrap();
        let acceptance_index = prompt.find("## Acceptance Criteria").unwrap();

        assert!(duty_index < acceptance_index);
    }

    #[test]
    fn explicit_phase_model_wins_over_default_model() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.default_model = Some("codex".to_owned());
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("- Agent role: opus"));
        assert!(prompt.contains("The configured model is `opus`"));
        assert!(!prompt.contains("- Agent role: codex"));
    }

    #[test]
    fn pending_phase_prompt_has_no_orchestrator_wrapper() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = test_config();
        config.default_model = Some("codex".to_owned());
        let mut store = test_store(&temp_dir, &config);

        // Task in pending phase — transition phase, no model, no orchestrator wrapper.
        store.add_task("Implement pick", "", vec![], None).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(!prompt.contains("--- END SUBAGENT PROMPT ---"));
        assert!(prompt.contains("# Agira Task Prompt"));
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

        store.add_task("Implement pick", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        // Task is in in_progress state but config has no in_progress phase: model is None.
        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

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
            verification: VerificationConfig { commands: vec![] },
            max_retries: 3,
        };
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Triage work", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap(); // pending -> triage

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("# Agira Orchestrator Instructions"));
        assert!(prompt.contains("--- SUBAGENT PROMPT ---"));
        assert!(prompt.contains("# Agira Task Prompt"));
        assert!(prompt.contains("- Agent role: codex"));
        assert!(prompt.contains("The configured model is `codex`"));
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
            verification: VerificationConfig { commands: vec![] },
            max_retries: 3,
        };
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Triage work", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(!prompt.contains("# Agira Orchestrator Instructions"));
        assert!(!prompt.contains("- Agent role:"));
    }

    #[test]
    fn task_prompt_contains_todo_command() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement work", "", vec![], None).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("agira task todo --artifact"));
    }

    #[test]
    fn pending_task_prompt_tells_agent_to_accept_and_advance() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Accept work", "", vec![], None).unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("## Pending Phase"));
        assert!(prompt.contains("This task is currently in the pending phase."));
        assert!(
            prompt
                .contains("You are expected to accept the task and advance it, not just read it.")
        );
        assert!(prompt.contains(
            "You must call `agira task todo --artifact \"<evidence>\"` to move the task forward to the next phase."
        ));
    }

    #[test]
    fn non_pending_task_prompt_omits_pending_phase_instruction() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Continue work", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(!prompt.contains("## Pending Phase"));
        assert!(!prompt.contains("This task is currently in the pending phase."));
        assert!(
            !prompt
                .contains("You are expected to accept the task and advance it, not just read it.")
        );
        assert!(!prompt.contains("agira task todo --artifact \"<evidence>\""));
    }

    #[test]
    fn task_prompt_contains_checkpoint_block_instruction() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store
            .add_task("Clarify requirements", "", vec![], None)
            .unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("not confident about a decision"));
        assert!(prompt.contains("human input is required"));
        assert!(prompt.contains("Blocking is the correct escalation path"));
        assert!(prompt.contains("not a last resort"));
        assert!(prompt.contains("agira task block task-001 --reason \"<explanation>\""));
    }

    #[test]
    fn task_prompt_without_just_done_has_three_steps() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        // Advance to enriching (has model: opus) so orchestrator wrapper is present.
        store.add_task("Next task", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(store.get_task("task-001").unwrap(), &config, None);

        assert!(prompt.contains("1. Read the task title and description"));
        assert!(prompt.contains("2. Write a SHORT, CLEAR problem statement"));
        assert!(prompt.contains("3. Spawn a subagent using model `opus`"));
        assert!(prompt.contains("The configured model is `opus`"));
        assert!(!prompt.contains("Once the subagent finishes"));
        assert!(!prompt.contains("1. Spawn a subagent"));
        assert!(!prompt.contains("2. Pass the content"));
        assert!(!prompt.contains("3. Once the subagent finishes"));
    }

    #[test]
    fn task_prompt_just_done_does_not_add_orchestrator_commands() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        // Advance to enriching (has model: opus) so orchestrator wrapper is present.
        store.add_task("Next task", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let prompt = format_task_prompt(
            store.get_task("task-001").unwrap(),
            &config,
            Some(("task-000", "Previous Task")),
        );

        assert!(!prompt.contains("Commit all changes"));
        assert!(!prompt.contains("task-000 \"Previous Task\""));
        assert!(prompt.contains("1. Read the task title and description"));
        assert!(prompt.contains("2. Write a SHORT, CLEAR problem statement"));
        assert!(prompt.contains("3. Spawn a subagent using model `opus`"));
        assert!(!prompt.contains("Once the subagent finishes"));
        assert!(!prompt.contains("4. Once the subagent finishes"));
    }

    #[test]
    fn task_prompt_no_ansi() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("Implement pick", "", vec![], None).unwrap();

        let output = format_pick_output(&config, store.all_tasks(), None);

        assert!(!output.as_bytes().contains(&0x1B));
    }

    #[test]
    fn completion_summary_all_done() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config();
        let mut store = test_store(&temp_dir, &config);

        store.add_task("First done task", "", vec![], None).unwrap();
        store
            .add_task("Second done task", "", vec![], None)
            .unwrap();
        for id in ["task-001", "task-002"] {
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
            store.next_phase(id).unwrap();
        }

        let output = format_pick_output(&config, store.all_tasks(), None);

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
