use std::{collections::BTreeMap, io, path::PathBuf};

use thiserror::Error;

use crate::core::{
    config::{Config, ConfigError, WorkflowDef, load_project_config},
    project::Project,
};

#[derive(Debug, Error)]
pub enum WorkflowListError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to load config {path}: {source}")]
    Load {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid config {path}: {reason}")]
    InvalidConfig { path: PathBuf, reason: String },

    #[error("failed to serialize workflow list")]
    JsonOutput(#[source] serde_json::Error),
}

pub fn run_workflow_list(project: &Project, json: bool) -> Result<(), WorkflowListError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;

    if json {
        let output = format_workflow_list_json(&config)?;
        println!("{output}");
    } else {
        let output = format_workflow_list(&config);
        print!("{output}");
    }

    Ok(())
}

fn format_workflow_list_json(config: &Config) -> Result<String, WorkflowListError> {
    // Sort workflow names for deterministic output
    let sorted: BTreeMap<&str, &WorkflowDef> = config
        .workflows
        .iter()
        .map(|(name, def)| (name.as_str(), def))
        .collect();

    serde_json::to_string_pretty(&sorted).map_err(WorkflowListError::JsonOutput)
}

fn format_workflow_list(config: &Config) -> String {
    if config.workflows.is_empty() {
        return "no workflows configured\n".to_owned();
    }

    // Sort workflow names for deterministic output
    let mut names: Vec<&str> = config.workflows.keys().map(String::as_str).collect();
    names.sort();

    let mut output = String::new();
    for name in names {
        let def = &config.workflows[name];
        let is_default = name == config.default_workflow;
        let phases_str = format_phases(&def.phases);

        if is_default {
            output.push_str(&format!("{name} (default)  {phases_str}\n"));
        } else {
            output.push_str(&format!("{name}  {phases_str}\n"));
        }
    }
    output
}

fn format_phases(phases: &[crate::core::config::PhaseConfig]) -> String {
    phases
        .iter()
        .map(|p| match p.model.as_deref() {
            Some(m) => format!("{}:{}", p.name, m),
            None => p.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn map_config_error(error: ConfigError) -> WorkflowListError {
    match error {
        ConfigError::NotFound { path } => WorkflowListError::NotFound { path },
        ConfigError::Read { path, source } => WorkflowListError::Read { path, source },
        ConfigError::Parse { path, source } => WorkflowListError::Load { path, source },
        ConfigError::Invalid { path, reason } => WorkflowListError::InvalidConfig { path, reason },
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs};

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig, WorkflowDef},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
    };

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        }
    }

    fn write_config(project: &Project, config: &Config) {
        let contents = serde_json::to_vec_pretty(config).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn make_phases(specs: &[(&str, Option<&str>)]) -> Vec<PhaseConfig> {
        specs
            .iter()
            .map(|(name, model)| PhaseConfig {
                name: name.to_string(),
                model: model.map(str::to_owned),
                duty: None,
            })
            .collect()
    }

    fn make_config_multi_workflow() -> Config {
        let fast_phases = make_phases(&[
            ("pending", None),
            ("in_progress", Some("sonnet")),
            ("done", None),
        ]);
        let full_phases = make_phases(&[
            ("pending", None),
            ("enriching", Some("opus")),
            ("in_progress", Some("sonnet")),
            ("verifying", Some("haiku")),
            ("done", None),
        ]);

        let mut workflows = HashMap::new();
        workflows.insert(
            "fast".to_owned(),
            WorkflowDef {
                phases: fast_phases,
            },
        );
        workflows.insert(
            "full".to_owned(),
            WorkflowDef {
                phases: full_phases,
            },
        );

        Config {
            stack: "rust".to_owned(),
            workflows,
            default_workflow: "fast".to_owned(),
            default_model: None,
            max_retries: 3,
        }
    }

    fn make_config_single_workflow() -> Config {
        Config::new_single_workflow(
            "rust",
            make_phases(&[
                ("pending", None),
                ("enriching", Some("opus")),
                ("in_progress", Some("sonnet")),
                ("done", None),
            ]),
            None,
            3,
        )
    }

    // ---- Tests (written before implementation, per TDD requirement) ----

    #[test]
    fn list_shows_default_marker_on_default_workflow() {
        let config = make_config_multi_workflow();
        let output = format_workflow_list(&config);
        // "fast" is the default, should have "(default)" marker
        assert!(
            output.contains("fast (default)"),
            "expected 'fast (default)' in output, got:\n{output}"
        );
        // "full" is not the default, should NOT have the marker
        assert!(
            !output.contains("full (default)"),
            "unexpected '(default)' on non-default workflow in output:\n{output}"
        );
    }

    #[test]
    fn list_shows_all_workflows() {
        let config = make_config_multi_workflow();
        let output = format_workflow_list(&config);
        assert!(
            output.contains("fast"),
            "expected 'fast' in output, got:\n{output}"
        );
        assert!(
            output.contains("full"),
            "expected 'full' in output, got:\n{output}"
        );
    }

    #[test]
    fn list_shows_phases_in_name_colon_model_format() {
        let config = make_config_multi_workflow();
        let output = format_workflow_list(&config);
        // The fast workflow has in_progress:sonnet
        assert!(
            output.contains("in_progress:sonnet"),
            "expected 'in_progress:sonnet' in output, got:\n{output}"
        );
        // The full workflow has enriching:opus
        assert!(
            output.contains("enriching:opus"),
            "expected 'enriching:opus' in output, got:\n{output}"
        );
    }

    #[test]
    fn list_shows_model_less_phases_without_colon() {
        let config = make_config_single_workflow();
        let output = format_workflow_list(&config);
        // pending and done have no model — they should appear without ':'
        assert!(
            output.contains("pending ->") || output.contains("pending:") == false,
            "model-less phase 'pending' should not have a colon suffix"
        );
        // Verify the format: "pending" appears bare (no colon)
        assert!(
            !output.contains("pending:"),
            "model-less phase 'pending' must not have ':' in output"
        );
    }

    #[test]
    fn list_output_is_sorted_by_workflow_name() {
        let config = make_config_multi_workflow();
        let output = format_workflow_list(&config);
        let fast_pos = output.find("fast").unwrap();
        let full_pos = output.find("full").unwrap();
        assert!(
            fast_pos < full_pos,
            "workflows should be sorted: 'fast' should appear before 'full'"
        );
    }

    #[test]
    fn list_json_is_parseable_and_contains_all_workflows() {
        let config = make_config_multi_workflow();
        let json_str = format_workflow_list_json(&config).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&json_str).expect("JSON output must be parseable");

        let obj = value.as_object().expect("JSON must be an object");
        assert!(
            obj.contains_key("fast"),
            "JSON must contain 'fast' workflow"
        );
        assert!(
            obj.contains_key("full"),
            "JSON must contain 'full' workflow"
        );
        // JSON keys are sorted (BTreeMap)
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        let mut sorted_keys = keys.clone();
        sorted_keys.sort();
        assert_eq!(keys, sorted_keys, "JSON keys must be sorted");
    }

    #[test]
    fn list_json_structure_matches_config_workflows() {
        let config = make_config_multi_workflow();
        let json_str = format_workflow_list_json(&config).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // Each workflow entry should have a "phases" array
        let fast = value.get("fast").expect("'fast' key should exist");
        let phases = fast
            .get("phases")
            .expect("'fast' should have 'phases' array");
        assert!(phases.is_array(), "'phases' must be a JSON array");

        let fast_phases = phases.as_array().unwrap();
        assert_eq!(fast_phases.len(), 3); // pending, in_progress, done

        // Check phase entries have "name" fields
        let phase_names: Vec<&str> = fast_phases
            .iter()
            .map(|p| p.get("name").and_then(|n| n.as_str()).unwrap())
            .collect();
        assert_eq!(phase_names, ["pending", "in_progress", "done"]);
    }

    #[test]
    fn list_single_workflow_default_is_marked() {
        let config = make_config_single_workflow();
        let output = format_workflow_list(&config);
        assert!(
            output.contains("default (default)"),
            "single workflow named 'default' should be marked as default, got:\n{output}"
        );
    }

    #[test]
    fn list_errors_when_config_missing() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        // No config.json written
        let err = run_workflow_list(&project, false).unwrap_err();
        assert!(
            matches!(err, WorkflowListError::NotFound { .. }),
            "expected NotFound error, got: {err}"
        );
    }

    #[test]
    fn list_returns_ok_with_valid_config() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = make_config_single_workflow();
        write_config(&project, &config);
        run_workflow_list(&project, false).unwrap();
    }

    #[test]
    fn list_json_returns_ok_with_valid_config() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = make_config_multi_workflow();
        write_config(&project, &config);
        run_workflow_list(&project, true).unwrap();
    }
}
