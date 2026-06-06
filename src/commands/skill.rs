use thiserror::Error;

/// Errors that can occur while printing skill management prompts.
///
/// Printing prompts is infallible today, but the `Result` signature is kept so
/// the command surface stays consistent with the rest of `commands` and can grow
/// fallible behavior later without a breaking change.
#[derive(Debug, Error)]
pub enum SkillError {}

const INSTALL_PROMPT: &str = "\
Write a personal skill named `agira-task-add` that helps add tasks to Agira.

Do not create the skill in this conversation by guessing file paths. Instead,
follow your environment's documented procedure for authoring a personal skill,
and give that skill the behavior described below.

## When the skill should trigger
Activate whenever the user asks to add, create, or register an Agira task — for
example \"add an agira task\", \"create a task to ...\", or \"register a follow-up in agira\".

## What the skill should do
1. Discover which Agira projects are initialized by running `agira project list`,
   which prints each project's slug and its workspace path.
2. Infer the correct workspace for the request from the user's current directory
   and the discovered project paths. If the workspace is ambiguous, ask the user
   which project to target instead of guessing.
3. Confirm the chosen workspace before making any change.
4. From that workspace, run `agira task add \"<title>\" --description \"<details>\"`
   (adding `--depends-on` or `--phase` only when the user supplies them).
5. Never edit `~/.agira/<slug>/tasks.json` or any other Agira state file directly;
   always go through the `agira` CLI so IDs, history, and the state machine stay valid.

## Safety
Treat task creation as a real side effect: confirm the workspace and the task
details with the user before running `agira task add`. Do not add tasks to a
project the user did not intend.
";

const UNINSTALL_PROMPT: &str = "\
Delete the personal skill named `agira-task-add` that was previously created from
the `agira skill install` guidance.

Remove only that skill. Do not delete, disable, or modify any other personal
skills — including unrelated Agira skills or skills that merely mention Agira.
If you cannot find a skill named `agira-task-add`, report that nothing was removed
rather than deleting a different skill.
";

/// Print the prompt that asks an agent to author the `agira-task-add` personal skill.
pub fn run_skill_install() -> Result<(), SkillError> {
    print!("{INSTALL_PROMPT}");
    Ok(())
}

/// Print the prompt that asks an agent to delete the `agira-task-add` personal skill.
pub fn run_skill_uninstall() -> Result<(), SkillError> {
    print!("{UNINSTALL_PROMPT}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_prompt_names_the_managed_skill() {
        assert!(
            INSTALL_PROMPT.contains("agira-task-add"),
            "install prompt should name the skill it creates"
        );
    }

    #[test]
    fn install_prompt_describes_task_add_trigger() {
        assert!(
            INSTALL_PROMPT.contains("add, create, or register an Agira task"),
            "install prompt should describe when the skill triggers"
        );
    }

    #[test]
    fn install_prompt_points_at_project_discovery() {
        assert!(
            INSTALL_PROMPT.contains("agira project list"),
            "install prompt should discover projects via 'agira project list'"
        );
    }

    #[test]
    fn install_prompt_runs_task_add_from_workspace() {
        assert!(
            INSTALL_PROMPT.contains("agira task add"),
            "install prompt should run 'agira task add'"
        );
        assert!(
            INSTALL_PROMPT.contains("Confirm the chosen workspace"),
            "install prompt should require workspace confirmation before acting"
        );
    }

    #[test]
    fn install_prompt_forbids_direct_state_edits() {
        assert!(
            INSTALL_PROMPT.contains("tasks.json"),
            "install prompt should forbid editing tasks.json directly"
        );
    }

    #[test]
    fn uninstall_prompt_scopes_deletion_to_the_managed_skill() {
        assert!(
            UNINSTALL_PROMPT.contains("agira-task-add"),
            "uninstall prompt should name the skill it deletes"
        );
        assert!(
            UNINSTALL_PROMPT.contains("only"),
            "uninstall prompt should scope deletion to only the managed skill"
        );
        assert!(
            UNINSTALL_PROMPT.contains("Do not delete"),
            "uninstall prompt should warn against deleting other skills"
        );
    }
}
