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
    let rendered_description: String;
    let description_text = if task.description.is_empty() {
        "No description provided."
    } else if phase_is_gated(&task.state, config) {
        rendered_description = strip_acceptance_criteria(&task.description);
        rendered_description.as_str()
    } else {
        task.description.as_str()
    };
    subagent.push_str(&format!("\n\n## Description\n{description_text}"));

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
        "\n\n## Checkpoints\nNEVER ask the user a question in-session. If a user decision is required to proceed, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Write each question clearly on its own line in the reason, then stop. Run:\n`agira task block {} --reason \"<questions>\"`",
        task.id
    ));

    // Append the Completion section only when the phase is NOT dispatched to a sub-agent backend.
    // Dispatched phases (those with a non-empty model) are advanced by the orchestrator, not the
    // sub-agent, so the completion command would only confuse the sub-agent into self-advancing.
    // Phases with no model are executed directly (human/CLI) and still need the hint.
    let phase_has_model = model.is_some();
    if task.state != INITIAL_PHASE_NAME && !phase_has_model {
        subagent.push_str(&format!(
            "\n\n## Completion\n`agira task todo --task {} --from {} --artifact \"<evidence>\"`",
            task.id, task.state
        ));
    }

    subagent
}

/// Returns true when the heading text (without the `#` prefix and trimmed) identifies an
/// Acceptance-Criteria section.  Matches English and two common Chinese localizations.
fn is_acceptance_criteria_heading(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    lower == "acceptance criteria" || lower == "验收" || lower == "验收标准"
}

/// Parse the `#` level of an ATX heading line (e.g. `## Foo` → 2). Returns `None` when the
/// line is not an ATX heading.
fn atx_heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start_matches('#');
    let level = line.len() - trimmed.len();
    if level == 0 || level > 6 {
        return None;
    }
    // After the `#` characters there must be a space (or the line ends)
    if !trimmed.is_empty() && !trimmed.starts_with(' ') {
        return None;
    }
    Some(level)
}

/// Remove the `## Acceptance Criteria` section (and its Chinese aliases) from `text`.
/// Only the first matching section is removed. If no such heading exists, returns the input
/// string unchanged.  Surrounding blank lines are collapsed so the output stays clean.
pub(crate) fn strip_acceptance_criteria(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len();

    // Find the index of the AC heading line.
    let ac_start = lines.iter().position(|line| {
        if let Some(level) = atx_heading_level(line) {
            if level == 2 {
                let heading_text = line.trim_start_matches('#').trim();
                return is_acceptance_criteria_heading(heading_text);
            }
        }
        false
    });

    let Some(ac_start) = ac_start else {
        return text.to_owned();
    };

    // The AC heading is at level 2.  Find the next heading at the same or higher level (≤ 2).
    let ac_end = lines[ac_start + 1..]
        .iter()
        .position(|line| atx_heading_level(line).is_some_and(|lvl| lvl <= 2));

    let section_end = match ac_end {
        Some(rel) => ac_start + 1 + rel, // exclusive end: first line of next section
        None => n,
    };

    // Build result by omitting lines [ac_start, section_end).
    let before: Vec<&str> = lines[..ac_start].to_vec();
    let after: Vec<&str> = lines[section_end..].to_vec();

    // Trim trailing blank lines from `before` and leading blank lines from `after`
    // so we don't accumulate extra empty lines.
    let before_trimmed = trim_trailing_blank_lines(before);
    let after_trimmed = trim_leading_blank_lines(after);

    match (before_trimmed.is_empty(), after_trimmed.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after_trimmed.join("\n"),
        (false, true) => before_trimmed.join("\n"),
        (false, false) => {
            let mut result = before_trimmed;
            result.push("");
            result.push("");
            result.extend(after_trimmed);
            result.join("\n")
        }
    }
}

fn trim_trailing_blank_lines(mut lines: Vec<&str>) -> Vec<&str> {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn trim_leading_blank_lines(mut lines: Vec<&str>) -> Vec<&str> {
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    lines
}

/// Returns true when the given phase has a non-empty `gate` in config.
fn phase_is_gated(phase: &str, config: &Config) -> bool {
    config
        .phase_def(phase)
        .and_then(|p| p.gate.as_deref())
        .is_some_and(|g| !g.is_empty())
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

    use super::{
        format_task_prompt_output, select_next_task_for_runner, strip_acceptance_criteria,
    };

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
        // The implementing phase has a model ("dispatch exec -a codex"), so ## Completion is
        // omitted — the orchestrator advances the task, not the sub-agent.
        let expected = "# Agira Task Prompt\n\n## Task\n- ID: task-109\n- Title: inject previous review feedback\n- Current phase: implementing\n- Agent role: dispatch exec -a codex\n\n## Description\nImplement retry feedback in the todo prompt so later implementers can see the most recent reviewer rejection and inspect previously written attachment evidence before changing code.\n\n## Phase Duty\nImplement the task.\n\n## Attachments\nSave evidence files (screenshots, recordings, test output) to:\n/tmp/agira-state/attachments/task-109/\nCreate the directory if it does not exist. Reference saved files in your --artifact text.\n\n## Checkpoints\nNEVER ask the user a question in-session. If a user decision is required to proceed, block the task instead of proceeding or guessing. Blocking is the correct escalation path whenever a checkpoint is needed, not a last resort. Write each question clearly on its own line in the reason, then stop. Run:\n`agira task block task-109 --reason \"<questions>\"`";

        assert_eq!(output, expected);
        assert!(!output.contains("## Previous Review Feedback"));
        assert!(!output.contains("Read any existing files under"));
        assert!(
            !output.contains("## Completion"),
            "dispatched phase must not expose Completion"
        );
    }

    #[test]
    fn task_prompt_requires_blocking_instead_of_in_session_questions() {
        let config = test_config();
        let state_dir = Path::new("/tmp/agira-state");
        let task = test_task();

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(output.contains("NEVER ask the user a question in-session."));
        assert!(output.contains(
            "If a user decision is required to proceed, block the task instead of proceeding or guessing."
        ));
        assert!(
            output
                .contains("Write each question clearly on its own line in the reason, then stop.")
        );
        assert!(output.contains("`agira task block task-109 --reason \"<questions>\"`"));
        assert!(!output.contains("--task task-109 --reason"));
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

    // -----------------------------------------------------------------------
    // strip_acceptance_criteria unit tests
    // -----------------------------------------------------------------------

    /// AC section followed by another heading: only the AC section is removed;
    /// Goal, Changes, and Constraints survive.
    #[test]
    fn strip_ac_removes_ac_section_preserves_others() {
        let input = "## Goal\nDo the thing.\n\n## Acceptance Criteria\n- AC item 1\n- AC item 2\n\n## Constraints\nDo not do X.";
        let result = strip_acceptance_criteria(input);
        assert!(
            !result.contains("Acceptance Criteria"),
            "AC heading must be gone"
        );
        assert!(!result.contains("AC item 1"), "AC body must be gone");
        assert!(result.contains("## Goal"), "Goal must survive");
        assert!(
            result.contains("## Constraints"),
            "Constraints must survive"
        );
    }

    /// AC section as the last section (no trailing heading): stripped to end-of-string.
    #[test]
    fn strip_ac_removes_ac_section_when_last() {
        let input = "## Goal\nDo the thing.\n\n## Acceptance Criteria\n- AC item 1\n- AC item 2\n";
        let result = strip_acceptance_criteria(input);
        assert!(!result.contains("Acceptance Criteria"));
        assert!(!result.contains("AC item"));
        assert!(result.contains("## Goal"));
    }

    /// No AC heading: output is byte-identical to input.
    #[test]
    fn strip_ac_noop_when_no_ac_heading() {
        let input = "## Goal\nDo the thing.\n\n## Constraints\nDo not do X.";
        let result = strip_acceptance_criteria(input);
        assert_eq!(result, input);
    }

    /// Localized heading `## 验收标准` is also stripped.
    #[test]
    fn strip_ac_strips_localized_heading_yan_shou_biao_zhun() {
        let input =
            "## Goal\nDo the thing.\n\n## 验收标准\n- 条件 1\n\n## Constraints\nDo not do X.";
        let result = strip_acceptance_criteria(input);
        assert!(!result.contains("验收标准"));
        assert!(!result.contains("条件 1"));
        assert!(result.contains("## Goal"));
        assert!(result.contains("## Constraints"));
    }

    /// Localized heading `## 验收` is also stripped.
    #[test]
    fn strip_ac_strips_localized_heading_yan_shou() {
        let input = "## Goal\nDo the thing.\n\n## 验收\n- 条件 1\n\n## Constraints\nDo not do X.";
        let result = strip_acceptance_criteria(input);
        assert!(!result.contains("验收"));
        assert!(!result.contains("条件 1"));
        assert!(result.contains("## Constraints"));
    }

    // -----------------------------------------------------------------------
    // format_task_prompt_output: AC stripping integrated tests
    // -----------------------------------------------------------------------

    fn gated_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![(
                "in_progress".to_owned(),
                PhaseDef {
                    model: Some("sonnet".to_owned()),
                    duty: Some("Implement the task.".to_owned()),
                    gate: Some("cargo test".to_owned()),
                },
            )],
            3,
        )
    }

    fn ungated_config() -> Config {
        Config::new_single_workflow(
            "test",
            vec![(
                "accepting".to_owned(),
                PhaseDef {
                    model: Some("opus".to_owned()),
                    duty: Some("Accept the task.".to_owned()),
                    gate: None,
                },
            )],
            3,
        )
    }

    fn task_with_ac_description(state: &str) -> Task {
        Task {
            id: "task-200".to_owned(),
            title: "feature with ac".to_owned(),
            description: "## Goal\nBuild the feature.\n\n## Acceptance Criteria\n- Must pass tests\n- Must lint clean\n\n## Constraints\nNo new deps.".to_owned(),
            state: state.to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            clarifications: Vec::new(),
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: std::collections::BTreeMap::new(),
            history: Vec::new(),
            created_at: "2026-06-18T00:00:00Z".to_owned(),
            workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            locked_at: None,
        }
    }

    /// Gated phase: AC section is stripped from the rendered Description.
    #[test]
    fn prompt_gated_phase_strips_ac_from_description() {
        let config = gated_config();
        let state_dir = Path::new("/tmp/agira-state");
        let task = task_with_ac_description("in_progress");

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(
            !output.contains("Acceptance Criteria"),
            "AC must be absent for gated phase"
        );
        assert!(
            !output.contains("Must pass tests"),
            "AC body must be absent"
        );
        assert!(output.contains("## Goal"), "Goal must be present");
        assert!(
            output.contains("## Constraints"),
            "Constraints must be present"
        );
    }

    /// Ungated phase: Description is rendered verbatim including AC section.
    #[test]
    fn prompt_ungated_phase_keeps_ac_in_description() {
        let config = ungated_config();
        let state_dir = Path::new("/tmp/agira-state");
        let task = task_with_ac_description("accepting");

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(
            output.contains("Acceptance Criteria"),
            "AC must be present for ungated phase"
        );
        assert!(
            output.contains("Must pass tests"),
            "AC body must be present"
        );
    }

    /// Gated phase with a Description that has no AC section: output is not changed.
    #[test]
    fn prompt_gated_phase_no_ac_description_unchanged() {
        let config = gated_config();
        let state_dir = Path::new("/tmp/agira-state");
        let mut task = task_with_ac_description("in_progress");
        task.description = "## Goal\nBuild the feature.\n\n## Constraints\nNo new deps.".to_owned();

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(output.contains("## Goal"));
        assert!(output.contains("## Constraints"));
        // No extra blank lines or corruption
        assert!(!output.contains("Acceptance Criteria"));
    }

    // -----------------------------------------------------------------------
    // Completion section visibility: keyed off model presence
    // -----------------------------------------------------------------------

    fn config_with_model() -> Config {
        Config::new_single_workflow(
            "test",
            vec![(
                "in_progress".to_owned(),
                PhaseDef {
                    model: Some("sonnet".to_owned()),
                    duty: Some("Implement the task.".to_owned()),
                    gate: None,
                },
            )],
            3,
        )
    }

    fn config_without_model() -> Config {
        Config::new_single_workflow(
            "test",
            vec![(
                "in_progress".to_owned(),
                PhaseDef {
                    model: None,
                    duty: Some("Accept the task.".to_owned()),
                    gate: None,
                },
            )],
            3,
        )
    }

    fn task_in_phase(phase: &str) -> Task {
        Task {
            id: "task-300".to_owned(),
            title: "test task".to_owned(),
            description: "A long enough description for testing purposes that exceeds one hundred and fifty characters in total length, so no warning fires.".to_owned(),
            state: phase.to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            clarifications: Vec::new(),
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: std::collections::BTreeMap::new(),
            history: Vec::new(),
            created_at: "2026-06-18T00:00:00Z".to_owned(),
            workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            locked_at: None,
        }
    }

    /// Phase with a model (dispatched to sub-agent): ## Completion must be absent.
    /// ## Checkpoints and ## Phase Duty must still be present.
    #[test]
    fn prompt_with_model_omits_completion_section() {
        let config = config_with_model();
        let state_dir = Path::new("/tmp/agira-state");
        let task = task_in_phase("in_progress");

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(
            !output.contains("## Completion"),
            "Completion must be absent when phase has a model"
        );
        assert!(
            output.contains("## Checkpoints"),
            "Checkpoints must still be present"
        );
        assert!(
            output.contains("## Phase Duty"),
            "Phase Duty must still be present"
        );
    }

    /// Phase without a model (direct human/CLI execution): ## Completion must be present.
    #[test]
    fn prompt_without_model_includes_completion_section() {
        let config = config_without_model();
        let state_dir = Path::new("/tmp/agira-state");
        let task = task_in_phase("in_progress");

        let output = format_task_prompt_output(&task, &config, state_dir);

        assert!(
            output.contains("## Completion"),
            "Completion must be present when phase has no model"
        );
        assert!(
            output.contains("`agira task todo --task task-300 --from in_progress --artifact"),
            "Completion command must reference task id and phase"
        );
    }
}
