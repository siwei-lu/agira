use std::cmp::{Ordering, Reverse};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};

use crate::core::{
    config::{Config, INITIAL_PHASE_NAME, TERMINAL_PHASE_NAME},
    runner::{RunnerRegistry, is_lease_expired},
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

pub(crate) fn format_pick_output_for_runner(
    config: &Config,
    tasks: &[Task],
    state_dir: &Path,
    runner_registry: &RunnerRegistry,
    runner_id: &str,
    now: DateTime<Utc>,
) -> String {
    if tasks.is_empty() {
        return NO_TASKS_MESSAGE.to_owned();
    }

    if is_all_done(tasks, config) {
        return format_completion_summary(tasks);
    }

    if let Some(task) = select_next_task_for_runner(tasks, config, runner_registry, runner_id, now)
    {
        return format_task_prompt_output(task, config, state_dir);
    }

    format_non_actionable_summary(tasks)
}

pub(crate) fn select_next_task<'a>(all_tasks: &'a [Task], config: &Config) -> Option<&'a Task> {
    select_next_task_at(all_tasks, config, Utc::now())
}

pub(crate) fn select_next_task_for_runner<'a>(
    all_tasks: &'a [Task],
    config: &Config,
    runner_registry: &RunnerRegistry,
    runner_id: &str,
    now: DateTime<Utc>,
) -> Option<&'a Task> {
    select_next_task_with_leases_at(all_tasks, config, Some((runner_registry, runner_id)), now)
}

fn select_next_task_at<'a>(
    all_tasks: &'a [Task],
    config: &Config,
    now: DateTime<Utc>,
) -> Option<&'a Task> {
    select_next_task_with_leases_at(all_tasks, config, None, now)
}

fn select_next_task_with_leases_at<'a>(
    all_tasks: &'a [Task],
    config: &Config,
    runner: Option<(&RunnerRegistry, &str)>,
    now: DateTime<Utc>,
) -> Option<&'a Task> {
    let terminal_phase = config.terminal_phase()?;

    all_tasks
        .iter()
        .filter(|task| {
            is_actionable(task, config)
                && !is_lock_live(task.locked_at.as_deref(), now)
                && !is_live_lease_held_by_other_runner(task, runner, now)
                && deps_satisfied(task, all_tasks, terminal_phase)
        })
        .max_by_key(|task| {
            (
                task_phase_index(task, &task.state, config).unwrap_or(0),
                Reverse(task_id_number(&task.id)),
            )
        })
}

fn is_live_lease_held_by_other_runner(
    task: &Task,
    runner: Option<(&RunnerRegistry, &str)>,
    now: DateTime<Utc>,
) -> bool {
    let Some((registry, requesting_runner_id)) = runner else {
        return false;
    };

    registry.runners.iter().any(|(runner_id, runner)| {
        runner_id != requesting_runner_id
            && runner.current_task.as_deref() == Some(task.id.as_str())
            && !is_lease_expired(runner.lease_expires_at.as_deref(), now)
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
    config
        .sequence(&config.default_workflow)
        .iter()
        .position(|p| p == phase)
}

fn task_phase_names<'a>(task: &'a Task, config: &'a Config) -> Vec<&'a str> {
    config
        .sequence(&task.workflow)
        .iter()
        .map(String::as_str)
        .collect()
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

#[cfg(any())]
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
        if !task.clarifications.is_empty() {
            subagent.push_str("\n\n## 已澄清事项");
            for clarification in &task.clarifications {
                subagent.push_str(&format!(
                    "\n- Phase: {}\n  Question: {}\n  Answer: {}",
                    clarification.phase, clarification.question, clarification.answer
                ));
            }
        }

        if let Some(feedback) = previous_review_feedback(task) {
            subagent.push_str(&format!(
                "\n\n## Previous Review Feedback\n{feedback}\n\nRead any existing files under {}/ before implementing; prior reviewer findings or evidence may already be there.",
                attachments_path.display()
            ));
        }

        subagent.push_str(&format!(
            "\n\n## Attachments\nSave evidence files (screenshots, recordings, test output) to:\n{}/\nCreate the directory if it does not exist. Reference saved files in your --artifact text.",
            attachments_path.display()
        ));
    }

    subagent.push_str(&format!(
        "\n\n## Checkpoints\nIf you are not confident about a decision and human input is required, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Run:\n`agira task block {} --reason \"<explanation>\"`",
        task.id
    ));

    // Append the Completion section for all non-initial, non-terminal phases.
    // (format_task_prompt_output is not called for terminal tasks in the current code path.)
    if task.state != INITIAL_PHASE_NAME {
        subagent.push_str(&format!(
            "\n\n## Completion\n`agira task todo --task {} --from {} --artifact \"<evidence>\"`",
            task.id, task.state
        ));
    }

    subagent
}

fn previous_review_feedback(task: &Task) -> Option<&str> {
    if task.retry_count == 0 {
        return None;
    }

    task.history
        .iter()
        .rev()
        .find_map(|entry| entry.reason.strip_prefix("retry "))
        .map(|retry_reason| {
            retry_reason
                .split_once(": ")
                .map_or(retry_reason, |(_, feedback)| feedback)
        })
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

fn effective_task_model<'a>(task: &'a Task, phase: &str, config: &'a Config) -> Option<&'a str> {
    if phase == INITIAL_PHASE_NAME || phase == TERMINAL_PHASE_NAME {
        return None;
    }

    if !config
        .sequence(&task.workflow)
        .iter()
        .any(|name| name == phase)
    {
        return None;
    }

    config
        .phase_def(phase)
        .and_then(|phase_cfg| phase_cfg.model.as_deref())
}

fn effective_task_duty<'a>(task: &'a Task, phase: &str, config: &'a Config) -> Option<&'a str> {
    if !config
        .sequence(&task.workflow)
        .iter()
        .any(|name| name == phase)
    {
        return None;
    }

    config
        .phase_def(phase)
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
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };

    use chrono::{DateTime, Utc};

    use crate::core::{
        config::{Config, DEFAULT_WORKFLOW_NAME, PhaseDef},
        runner::{Runner, RunnerRegistry},
        tasks::{Clarification, HistoryEntry, Task},
    };

    use super::{format_task_prompt_output, select_next_task_for_runner};

    fn test_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![(
                "implementing".to_owned(),
                PhaseDef {
                    model: Some("dispatch exec -a codex".to_owned()),
                    duty: Some("Implement the task.".to_owned()),
                    gate: None,
                },
            )],
            3,
        )
    }

    fn test_task() -> Task {
        Task {
            id: "task-109".to_owned(),
            title: "inject previous review feedback".to_owned(),
            description: "Implement retry feedback in the todo prompt so later implementers can see the most recent reviewer rejection and inspect previously written attachment evidence before changing code.".to_owned(),
            state: "implementing".to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            clarifications: Vec::new(),
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: BTreeMap::new(),
            history: Vec::new(),
            created_at: "2026-06-10T05:00:00Z".to_owned(),
            workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            locked_at: None,
        }
    }

    fn task_with_id(id: &str, state: &str) -> Task {
        Task {
            id: id.to_owned(),
            title: format!("{id} title"),
            description: "description".to_owned(),
            state: state.to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            clarifications: Vec::new(),
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: BTreeMap::new(),
            history: Vec::new(),
            created_at: "2026-06-10T00:00:00Z".to_owned(),
            workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            locked_at: None,
        }
    }

    fn runner_with_lease(id: &str, task_id: &str, lease_expires_at: &str) -> Runner {
        Runner {
            id: id.to_owned(),
            runner_type: "local".to_owned(),
            tmux_session: String::new(),
            pgid: None,
            status: "running".to_owned(),
            current_task: Some(task_id.to_owned()),
            lease_expires_at: Some(lease_expires_at.to_owned()),
            last_heartbeat: Some("2026-06-11T12:00:00Z".to_owned()),
            idle_since: None,
            registered_at: "2026-06-11T12:00:00Z".to_owned(),
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
            .expect("parse fixed time")
            .with_timezone(&Utc)
    }

    fn attachments_path(state_dir: &Path, task_id: &str) -> PathBuf {
        state_dir.join("attachments").join(task_id)
    }

    #[test]
    fn runner_selection_skips_task_held_by_other_live_lease() {
        let config = test_config();
        let tasks = vec![
            task_with_id("task-001", "implementing"),
            task_with_id("task-002", "implementing"),
        ];
        let mut registry = RunnerRegistry::default();
        registry.runners.insert(
            "runner-other".to_owned(),
            runner_with_lease("runner-other", "task-001", "2026-06-11T12:05:00Z"),
        );

        let selected =
            select_next_task_for_runner(&tasks, &config, &registry, "runner-me", fixed_now())
                .expect("select task");

        assert_eq!(selected.id, "task-002");
    }

    #[test]
    fn runner_selection_allows_task_held_by_other_expired_lease() {
        let config = test_config();
        let tasks = vec![
            task_with_id("task-001", "implementing"),
            task_with_id("task-002", "implementing"),
        ];
        let mut registry = RunnerRegistry::default();
        registry.runners.insert(
            "runner-other".to_owned(),
            runner_with_lease("runner-other", "task-001", "2026-06-11T11:59:59Z"),
        );

        let selected =
            select_next_task_for_runner(&tasks, &config, &registry, "runner-me", fixed_now())
                .expect("select task");

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn runner_selection_allows_task_held_by_requesting_runner() {
        let config = test_config();
        let tasks = vec![
            task_with_id("task-001", "implementing"),
            task_with_id("task-002", "implementing"),
        ];
        let mut registry = RunnerRegistry::default();
        registry.runners.insert(
            "runner-me".to_owned(),
            runner_with_lease("runner-me", "task-001", "2026-06-11T12:05:00Z"),
        );

        let selected =
            select_next_task_for_runner(&tasks, &config, &registry, "runner-me", fixed_now())
                .expect("select task");

        assert_eq!(selected.id, "task-001");
    }

    #[test]
    fn task_prompt_includes_previous_review_feedback_on_retry() {
        let config = test_config();
        let state_dir = Path::new("/tmp/agira-state");
        let mut task = test_task();
        task.retry_count = 1;
        task.history.push(HistoryEntry {
            from: Some("reviewing".to_owned()),
            to: "implementing".to_owned(),
            timestamp: "2026-06-10T05:30:00Z".to_owned(),
            reason: "retry 1/3: handle the nil retry feedback case".to_owned(),
        });

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(output.contains("## Previous Review Feedback"));
        assert!(output.contains("handle the nil retry feedback case"));
        assert!(output.contains("Read any existing files under"));
        assert!(output.contains(&attachments_path(state_dir, &task.id).display().to_string()));
    }

    #[test]
    fn task_prompt_omits_previous_review_feedback_on_first_attempt() {
        let config = test_config();
        let state_dir = Path::new("/tmp/agira-state");
        let task = test_task();

        let output = format_task_prompt_output(&task, &config, state_dir);
        let expected = "# Agira Task Prompt\n\n## Task\n- ID: task-109\n- Title: inject previous review feedback\n- Current phase: implementing\n- Agent role: dispatch exec -a codex\n\n## Description\nImplement retry feedback in the todo prompt so later implementers can see the most recent reviewer rejection and inspect previously written attachment evidence before changing code.\n\n## Phase Duty\nImplement the task.\n\n## Attachments\nSave evidence files (screenshots, recordings, test output) to:\n/tmp/agira-state/attachments/task-109/\nCreate the directory if it does not exist. Reference saved files in your --artifact text.\n\n## Checkpoints\nIf you are not confident about a decision and human input is required, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Run:\n`agira task block task-109 --reason \"<explanation>\"`\n\n## Completion\n`agira task todo --task task-109 --from implementing --artifact \"<evidence>\"`";

        assert_eq!(output, expected);
        assert!(!output.contains("## Previous Review Feedback"));
        assert!(!output.contains("Read any existing files under"));
    }

    #[test]
    fn task_prompt_includes_resolved_clarifications_when_present() {
        let config = test_config();
        let state_dir = Path::new("/tmp/agira-state");
        let mut task = test_task();
        task.clarifications.push(Clarification {
            question: "Which API should be used?".to_owned(),
            answer: "Use the stable v2 endpoint.".to_owned(),
            phase: "implementing".to_owned(),
            timestamp: "2026-06-13T10:00:00Z".to_owned(),
        });

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(output.contains("## 已澄清事项"));
        assert!(output.contains("Question: Which API should be used?"));
        assert!(output.contains("Answer: Use the stable v2 endpoint."));
        assert!(output.contains("Phase: implementing"));
    }

    #[test]
    fn task_prompt_omits_resolved_clarifications_when_empty() {
        let config = test_config();
        let state_dir = Path::new("/tmp/agira-state");
        let task = test_task();

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(!output.contains("## 已澄清事项"));
    }
}
