use std::{fs, path::Path};

use crate::core::config::Config;

pub const DEFAULT_ORCHESTRATOR_TEMPLATE: &str = r#"# agira claude-tmux orchestrator

static marker: agira-orchestrator-template-v1

You are the thin Agira orchestrator for this project.

Idle-wait protocol:
- When no task is actionable, wait silently for a task notification or user instruction.
- While delegated work is running, do not do unrelated analysis or phase work in this context.

Agira CLI protocol:
- Call `agira task todo --runner "$AGIRA_RUNNER_ID"` to claim and print the next task prompt.
- Use the exact `## Completion` command from the task prompt: `agira task todo --task <id> --from <phase> --artifact "<evidence>"`.
- Include the runner identity through `AGIRA_RUNNER_ID` for all runner-owned task claims.

Backend routing:
- Route each phase according to the phase table below.
- Claude model phases delegate to a background Claude sub-agent.
- `dispatch exec -a codex` phases run as a Bash one-shot `codex exec` command using `AGIRA_PROMPT_FILE`.

Thin-orchestrator rule:
- Never implement, verify, review, fix, or enrich phase work directly in this interactive session.
- Only claim tasks, dispatch the configured backend, wait for completion, and advance with evidence.
"#;

pub const DEFAULT_ORCHESTRATOR_KICKOFF: &str = r#"Start the Agira orchestration loop now. Claim the next actionable task with `agira task todo --runner "$AGIRA_RUNNER_ID"`, follow the orchestrator protocol from your system prompt exactly, and when no task is actionable, idle-wait silently for the next task notification or user instruction."#;

pub fn render_phase_table(config: &Config) -> String {
    let mut output =
        String::from("\n## phase routing table\n\n| phase | backend | duty |\n|---|---|---|\n");

    for phase_name in config.sequence(&config.default_workflow) {
        let phase = config
            .phase_def(phase_name)
            .expect("validated workflow phases have definitions");
        output.push_str(&format!(
            "| {} | {} | {} |\n",
            escape_table_cell(phase_name),
            escape_table_cell(phase.model.as_deref().unwrap_or("none")),
            escape_table_cell(phase.duty.as_deref().unwrap_or("none")),
        ));
    }

    output
}

pub fn assemble_orchestrator_prompt(template: &str, config: &Config) -> String {
    let template = template.trim_end();
    format!("{template}\n{}", render_phase_table(config))
}

pub fn load_template_override(path: &Path) -> Result<String, std::io::Error> {
    fs::read_to_string(path)
}

fn escape_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use crate::core::config::{Config, PhaseDef};

    use super::{
        DEFAULT_ORCHESTRATOR_KICKOFF, DEFAULT_ORCHESTRATOR_TEMPLATE, assemble_orchestrator_prompt,
        render_phase_table,
    };

    fn config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![
                (
                    "implementing".to_owned(),
                    PhaseDef {
                        model: Some("dispatch exec -a codex".to_owned()),
                        duty: Some("write tests | then code".to_owned()),
                        gate: None,
                    },
                ),
                (
                    "verifying".to_owned(),
                    PhaseDef {
                        model: Some("sonnet".to_owned()),
                        duty: Some("run cargo test\nrun clippy".to_owned()),
                        gate: None,
                    },
                ),
            ],
            3,
        )
    }

    #[test]
    fn render_phase_table_uses_default_workflow_order_and_phase_fields() {
        let table = render_phase_table(&config());

        assert!(table.contains("| pending | none | none |"));
        assert!(
            table.contains("| implementing | dispatch exec -a codex | write tests \\| then code |")
        );
        assert!(table.contains("| verifying | sonnet | run cargo test<br>run clippy |"));
        assert!(table.contains("| done | none | none |"));
        assert!(
            table.find("| pending |").expect("pending row")
                < table.find("| implementing |").expect("implementing row")
        );
    }

    #[test]
    fn assemble_orchestrator_prompt_concatenates_static_template_and_phase_table() {
        let prompt = assemble_orchestrator_prompt(DEFAULT_ORCHESTRATOR_TEMPLATE, &config());

        assert!(prompt.contains("agira-orchestrator-template-v1"));
        assert!(prompt.contains("Thin-orchestrator rule"));
        assert!(prompt.contains("`agira task todo --runner \"$AGIRA_RUNNER_ID\"`"));
        assert!(prompt.contains(
            "Use the exact `## Completion` command from the task prompt: `agira task todo --task <id> --from <phase> --artifact \"<evidence>\"`"
        ));
        assert!(prompt.contains("| implementing | dispatch exec -a codex |"));
    }

    #[test]
    fn default_orchestrator_kickoff_claims_runner_task() {
        assert!(
            DEFAULT_ORCHESTRATOR_KICKOFF.contains("agira task todo --runner \"$AGIRA_RUNNER_ID\"")
        );
        assert!(DEFAULT_ORCHESTRATOR_KICKOFF.contains("idle-wait"));
    }
}
