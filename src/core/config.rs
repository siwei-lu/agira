use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::global_config::GlobalConfig;

pub const INITIAL_PHASE_NAME: &str = "pending";
pub const TERMINAL_PHASE_NAME: &str = "done";
pub const DEFAULT_WORKFLOW_NAME: &str = "default";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PhaseConfig {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duty: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDef {
    pub phases: Vec<PhaseConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub stack: String,
    pub workflows: HashMap<String, WorkflowDef>,
    pub default_workflow: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Config {
    /// Returns the phases of the default workflow.
    pub fn phases(&self) -> &[PhaseConfig] {
        self.workflows
            .get(&self.default_workflow)
            .map(|w| w.phases.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the phases of the named workflow.
    // Used in tests and available for future multi-workflow feature callers.
    #[allow(dead_code)]
    pub fn phases_in(&self, workflow: &str) -> Option<&[PhaseConfig]> {
        self.workflows.get(workflow).map(|w| w.phases.as_slice())
    }

    pub fn terminal_phase(&self) -> Option<&str> {
        self.phases().last().map(|p| p.name.as_str())
    }

    /// Returns the terminal phase name of the named workflow.
    // Available for future multi-workflow feature callers.
    #[allow(dead_code)]
    pub fn terminal_phase_in(&self, workflow: &str) -> Option<&str> {
        self.phases_in(workflow)
            .and_then(|phases| phases.last())
            .map(|p| p.name.as_str())
    }

    /// Returns a mutable reference to the phases of the default workflow.
    /// Panics if the default workflow is not found (should never happen after load).
    // Used in tests to mutate the phase list.
    #[allow(dead_code)]
    pub fn phases_mut(&mut self) -> &mut Vec<PhaseConfig> {
        &mut self
            .workflows
            .get_mut(&self.default_workflow)
            .expect("default_workflow must exist in workflows")
            .phases
    }

    /// Convenience constructor: creates a Config with a single default workflow.
    pub fn new_single_workflow(
        stack: impl Into<String>,
        phases: Vec<PhaseConfig>,
        default_model: Option<String>,
        max_retries: u32,
    ) -> Self {
        let mut workflows = HashMap::new();
        workflows.insert(DEFAULT_WORKFLOW_NAME.to_owned(), WorkflowDef { phases });
        Config {
            stack: stack.into(),
            workflows,
            default_workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            default_model,
            max_retries,
        }
    }
}

fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to load config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid config {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

#[derive(Deserialize)]
struct ProjectConfigFile {
    stack: String,
    // New multi-workflow format
    #[serde(default)]
    workflows: Option<HashMap<String, WorkflowDef>>,
    #[serde(default)]
    default_workflow: Option<String>,
    // Single-workflow format (legacy v2)
    #[serde(default)]
    phases: Option<Vec<PhaseConfig>>,
    // Old format (backward compat v1)
    #[serde(default)]
    state_machine: Option<Vec<String>>,
    #[serde(default)]
    models: Option<BTreeMap<String, String>>,
    #[serde(default)]
    #[allow(dead_code)]
    acceptance_testing: Option<String>,
    max_retries: Option<u32>,
    // Legacy fallback, also preserved in the loaded Config for model-less phases.
    #[serde(default)]
    default_model: Option<String>,
}

pub fn load_project_config(
    path: &Path,
    global_config: &GlobalConfig,
) -> Result<Config, ConfigError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::NotFound {
                path: path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let project_config =
        serde_json::from_str::<ProjectConfigFile>(&contents).map_err(|source| {
            ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            }
        })?;

    let default_model = project_config.default_model;

    // Precedence: workflows > phases > state_machine
    let (workflows, default_workflow) = if let Some(workflows) = project_config.workflows {
        let default_workflow = project_config
            .default_workflow
            .unwrap_or_else(|| DEFAULT_WORKFLOW_NAME.to_owned());
        let workflows: HashMap<String, WorkflowDef> = workflows
            .into_iter()
            .map(|(name, def)| {
                let phases = normalize_mandatory_phases(def.phases);
                (name, WorkflowDef { phases })
            })
            .collect();
        (workflows, default_workflow)
    } else {
        // Legacy single-workflow: phases or state_machine
        let phases = match project_config.phases {
            Some(phases) => phases,
            None => migrate_state_machine(
                project_config.state_machine.unwrap_or_default(),
                project_config.models.as_ref(),
                default_model.as_deref(),
            ),
        };
        let phases = normalize_mandatory_phases(phases);
        let default_workflow = DEFAULT_WORKFLOW_NAME.to_owned();
        let mut workflows = HashMap::new();
        workflows.insert(default_workflow.clone(), WorkflowDef { phases });
        (workflows, default_workflow)
    };

    // Validate default_workflow names an existing key
    if !workflows.contains_key(&default_workflow) {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: format!(
                "default_workflow '{}' not found in workflows map",
                default_workflow
            ),
        });
    }

    // Validate terminal phase for the default workflow
    let default_phases = &workflows[&default_workflow].phases;
    validate_terminal_phase(default_phases).map_err(|reason| ConfigError::Invalid {
        path: path.to_path_buf(),
        reason,
    })?;

    Ok(Config {
        stack: project_config.stack,
        workflows,
        default_workflow,
        default_model,
        max_retries: project_config
            .max_retries
            .unwrap_or(global_config.default_max_retries),
    })
}

fn migrate_state_machine(
    state_machine: Vec<String>,
    models: Option<&BTreeMap<String, String>>,
    project_default_model: Option<&str>,
) -> Vec<PhaseConfig> {
    let fallback = project_default_model.unwrap_or("sonnet");
    state_machine
        .into_iter()
        .map(|name| {
            // Mandatory phases have no model in the new schema.
            let model = if name == INITIAL_PHASE_NAME || name == TERMINAL_PHASE_NAME {
                None
            } else {
                let m = models
                    .and_then(|m| m.get(&name))
                    .map(String::as_str)
                    .unwrap_or(fallback)
                    .to_owned();
                Some(m)
            };
            PhaseConfig {
                name,
                model,
                duty: None,
            }
        })
        .collect()
}

pub fn normalize_mandatory_phases(phases: Vec<PhaseConfig>) -> Vec<PhaseConfig> {
    let mut initial_phase = None;
    let mut terminal_phase = None;
    let mut middle_phases = Vec::new();

    for phase in phases {
        if phase.name == INITIAL_PHASE_NAME {
            initial_phase.get_or_insert(phase);
        } else if phase.name == TERMINAL_PHASE_NAME {
            terminal_phase.get_or_insert(phase);
        } else {
            middle_phases.push(phase);
        }
    }

    // Mandatory phases are transition phases, not AI-driven phases.
    // Strip any model or duty that might have been set on them (e.g. from legacy config or user override).
    let initial_phase = initial_phase
        .map(|p| PhaseConfig {
            name: p.name,
            model: None,
            duty: None,
        })
        .unwrap_or_else(|| PhaseConfig {
            name: INITIAL_PHASE_NAME.to_owned(),
            model: None,
            duty: None,
        });
    let terminal_phase = terminal_phase
        .map(|p| PhaseConfig {
            name: p.name,
            model: None,
            duty: None,
        })
        .unwrap_or_else(|| PhaseConfig {
            name: TERMINAL_PHASE_NAME.to_owned(),
            model: None,
            duty: None,
        });

    let mut normalized = Vec::with_capacity(middle_phases.len() + 2);
    normalized.push(initial_phase);
    normalized.extend(middle_phases);
    normalized.push(terminal_phase);
    normalized
}

pub fn validate_terminal_phase(phases: &[PhaseConfig]) -> Result<(), String> {
    let Some(terminal_phase) = phases.last() else {
        return Ok(());
    };

    if terminal_phase.name == TERMINAL_PHASE_NAME {
        Ok(())
    } else {
        Err(format!(
            "last phase must be named '{TERMINAL_PHASE_NAME}' (found '{}')",
            terminal_phase.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn global_config(default_max_retries: u32) -> GlobalConfig {
        GlobalConfig {
            default_max_retries,
            hook_debug: false,
        }
    }

    /// Build a `Config` with a single default workflow from a flat phases list.
    fn config_with_phases(
        stack: &str,
        phases: Vec<PhaseConfig>,
        default_model: Option<String>,
        max_retries: u32,
    ) -> Config {
        let mut workflows = HashMap::new();
        workflows.insert(DEFAULT_WORKFLOW_NAME.to_owned(), WorkflowDef { phases });
        Config {
            stack: stack.to_owned(),
            workflows,
            default_workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            default_model,
            max_retries,
        }
    }

    fn write_new_format_config(path: &Path, fields: &str) {
        fs::write(
            path,
            format!(
                r#"{{
  "stack": "rust",
  "phases": [{{"name":"enriching","model":"opus"}},{{"name":"done"}}],
  "acceptance_testing": "cli"{fields}
}}"#
            ),
        )
        .unwrap();
    }

    fn write_old_format_config(path: &Path) {
        fs::write(
            path,
            r#"{
  "stack": "rust",
  "state_machine": ["enriching", "done"],
  "models": {},
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();
    }

    #[test]
    fn phase_config_serializes_optional_duty() {
        let with_duty = PhaseConfig {
            name: "enriching".to_owned(),
            model: Some("opus".to_owned()),
            duty: Some("prepare a concrete implementation plan".to_owned()),
        };

        let serialized = serde_json::to_string(&with_duty).unwrap();
        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            value.get("duty").and_then(serde_json::Value::as_str),
            Some("prepare a concrete implementation plan")
        );

        let without_duty = PhaseConfig {
            name: "in_progress".to_owned(),
            model: Some("sonnet".to_owned()),
            duty: None,
        };

        let serialized = serde_json::to_string(&without_duty).unwrap();
        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(value.get("duty").is_none());

        let missing_duty: PhaseConfig =
            serde_json::from_str(r#"{"name":"verifying","model":"haiku"}"#).unwrap();
        assert_eq!(missing_duty.duty, None);
    }

    #[test]
    fn normalize_mandatory_phases_strips_duty_from_pending_and_done() {
        let phases = normalize_mandatory_phases(vec![
            PhaseConfig {
                name: "done".to_owned(),
                model: Some("opus".to_owned()),
                duty: Some("terminal phase duty should be ignored".to_owned()),
            },
            PhaseConfig {
                name: "review".to_owned(),
                model: Some("sonnet".to_owned()),
                duty: Some("review the implementation evidence".to_owned()),
            },
            PhaseConfig {
                name: "pending".to_owned(),
                model: Some("haiku".to_owned()),
                duty: Some("initial phase duty should be ignored".to_owned()),
            },
        ]);

        let names: Vec<&str> = phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["pending", "review", "done"]);
        assert_eq!(phases[0].duty, None);
        assert_eq!(
            phases[1].duty,
            Some("review the implementation evidence".to_owned())
        );
        assert_eq!(phases[2].duty, None);
    }

    #[test]
    fn new_format_normalizes_missing_mandatory_phases() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "phases": [{"name":"enriching","model":"opus"},{"name":"in_progress","model":"sonnet"}],
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases().len(), 4);
        assert_eq!(config.phases()[0].name, "pending");
        assert_eq!(config.phases()[0].model, None);
        assert_eq!(config.phases()[1].name, "enriching");
        assert_eq!(config.phases()[1].model, Some("opus".to_owned()));
        assert_eq!(config.phases()[2].name, "in_progress");
        assert_eq!(config.phases()[2].model, Some("sonnet".to_owned()));
        assert_eq!(config.phases()[3].name, "done");
        assert_eq!(config.phases()[3].model, None);
    }

    #[test]
    fn new_format_strips_model_from_mandatory_phases() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        // Even if a model is provided for pending/done in the JSON, it is stripped.
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "phases": [
    {"name":"enriching","model":"opus"},
    {"name":"pending","model":"haiku"},
    {"name":"review","model":"sonnet"},
    {"name":"done","model":"opus"},
    {"name":"done","model":"haiku"},
    {"name":"pending","model":"sonnet"}
  ],
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let names: Vec<&str> = config.phases().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["pending", "enriching", "review", "done"]);
        // Mandatory phases always have no model regardless of what the JSON says.
        assert_eq!(config.phases()[0].model, None);
        assert_eq!(config.phases()[3].model, None);
    }

    #[test]
    fn old_format_migrated_using_global_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_old_format_config(&path);

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases().len(), 3);
        assert_eq!(config.phases()[0].name, "pending");
        assert_eq!(config.phases()[0].model, None);
        assert_eq!(config.phases()[1].name, "enriching");
        assert_eq!(config.phases()[1].model, Some("sonnet".to_owned()));
        assert_eq!(config.phases()[2].name, "done");
        assert_eq!(config.phases()[2].model, None);
    }

    #[test]
    fn old_format_without_done_normalizes_after_migration() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "state_machine": ["enriching", "verifying"],
  "models": {},
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let names: Vec<&str> = config.phases().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["pending", "enriching", "verifying", "done"]);
    }

    #[test]
    fn project_config_uses_global_defaults_for_max_retries() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_new_format_config(&path, "");

        let config = load_project_config(&path, &global_config(5)).unwrap();

        assert_eq!(config.max_retries, 5);
    }

    #[test]
    fn legacy_verification_key_ignored_on_read() {
        let temp_dir = TempDir::new().unwrap();
        let legacy_path = temp_dir.path().join("legacy-config.json");
        fs::write(
            &legacy_path,
            r#"{
  "stack": "rust",
  "phases": [{"name":"in_progress","model":"sonnet"}],
  "verification": { "commands": ["cargo test"] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let legacy_config = load_project_config(&legacy_path, &global_config(3)).unwrap();
        assert_eq!(legacy_config.stack, "rust");
        assert_eq!(legacy_config.phases()[1].name, "in_progress");
        assert_eq!(legacy_config.max_retries, 3);
    }

    #[test]
    fn project_config_explicit_max_retries_wins() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_new_format_config(&path, r#", "max_retries": 7"#);

        let config = load_project_config(&path, &global_config(5)).unwrap();

        assert_eq!(config.max_retries, 7);
    }

    #[test]
    fn project_config_preserves_default_model() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_new_format_config(&path, r#", "default_model": "codex""#);

        let config = load_project_config(&path, &global_config(5)).unwrap();

        assert_eq!(config.default_model, Some("codex".to_owned()));
    }

    #[test]
    fn old_format_default_model_is_preserved_after_migration() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "state_machine": ["enriching", "done"],
  "models": {},
  "default_model": "opus",
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.default_model, Some("opus".to_owned()));
        let enriching = config
            .phases()
            .iter()
            .find(|p| p.name == "enriching")
            .unwrap();
        assert_eq!(enriching.model, Some("opus".to_owned()));
    }

    #[test]
    fn model_less_middle_phase_preserved_by_normalize() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "phases": [
    {"name":"enriching","model":"opus"},
    {"name":"triage"},
    {"name":"in_progress","model":"sonnet"}
  ],
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let names: Vec<&str> = config.phases().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["pending", "enriching", "triage", "in_progress", "done"]
        );
        // model-less middle phase preserves None
        let triage = config.phases().iter().find(|p| p.name == "triage").unwrap();
        assert_eq!(triage.model, None);
        // model-bearing phases are unchanged
        let enriching = config
            .phases()
            .iter()
            .find(|p| p.name == "enriching")
            .unwrap();
        assert_eq!(enriching.model, Some("opus".to_owned()));
    }

    #[test]
    fn terminal_phase_returns_last_phase() {
        let config = config_with_phases(
            "rust",
            vec![
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
            None,
            3,
        );

        assert_eq!(config.terminal_phase(), Some("done"));
    }

    // ---- New tests for workflows map and default_workflow (TDD: written before implementation) ----

    #[test]
    fn legacy_phases_config_loads_as_default_workflow() {
        // A legacy config with a top-level `phases` array (no `workflows` map) must be
        // auto-migrated by wrapping the phases under `workflows["default"]` and setting
        // `default_workflow = "default"`.
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "phases": [
    {"name":"enriching","model":"opus"},
    {"name":"in_progress","model":"sonnet"}
  ]
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.default_workflow, DEFAULT_WORKFLOW_NAME);
        assert!(config.workflows.contains_key(DEFAULT_WORKFLOW_NAME));
        let phases = config.phases();
        // normalize_mandatory_phases should have run: pending/done inserted
        assert_eq!(phases[0].name, "pending");
        assert_eq!(phases[1].name, "enriching");
        assert_eq!(phases[2].name, "in_progress");
        assert_eq!(phases[3].name, "done");
    }

    #[test]
    fn multi_workflow_config_round_trips() {
        // A new-format config with multiple named workflows must serialize and
        // deserialize without data loss.
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "default_workflow": "fast",
  "workflows": {
    "fast": {
      "phases": [
        {"name":"pending"},
        {"name":"in_progress","model":"sonnet"},
        {"name":"done"}
      ]
    },
    "full": {
      "phases": [
        {"name":"pending"},
        {"name":"enriching","model":"opus"},
        {"name":"in_progress","model":"sonnet"},
        {"name":"verifying","model":"haiku"},
        {"name":"done"}
      ]
    }
  }
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.default_workflow, "fast");
        assert_eq!(config.workflows.len(), 2);

        // Default workflow phases
        let fast_phases = config.phases();
        assert_eq!(fast_phases[0].name, "pending");
        assert_eq!(fast_phases[1].name, "in_progress");
        assert_eq!(fast_phases[2].name, "done");

        // Non-default workflow accessible via phases_in
        let full_phases = config.phases_in("full").unwrap();
        assert_eq!(full_phases.len(), 5);
        assert_eq!(full_phases[1].name, "enriching");
        assert_eq!(full_phases[3].name, "verifying");

        // Round-trip: serialize then deserialize
        let serialized = serde_json::to_string_pretty(&config).unwrap();
        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(value.get("workflows").is_some());
        assert_eq!(
            value.get("default_workflow").and_then(|v| v.as_str()),
            Some("fast")
        );
        assert!(value.get("phases").is_none()); // new format never has top-level phases

        // Write and re-read
        fs::write(&path, &serialized).unwrap();
        let reloaded = load_project_config(&path, &global_config(3)).unwrap();
        assert_eq!(reloaded.default_workflow, config.default_workflow);
        assert_eq!(reloaded.workflows.len(), config.workflows.len());
        assert_eq!(reloaded.phases(), config.phases());
    }

    #[test]
    fn each_workflow_independently_gets_pending_and_done() {
        // normalize_mandatory_phases must run per-workflow. Each workflow that omits
        // pending/done must have them inserted at head and tail independently.
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "default_workflow": "default",
  "workflows": {
    "default": {
      "phases": [
        {"name":"enriching","model":"opus"},
        {"name":"in_progress","model":"sonnet"}
      ]
    },
    "minimal": {
      "phases": [
        {"name":"in_progress","model":"sonnet"}
      ]
    }
  }
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let default_phases = config.phases();
        let names_default: Vec<&str> = default_phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names_default,
            ["pending", "enriching", "in_progress", "done"]
        );

        let minimal_phases = config.phases_in("minimal").unwrap();
        let names_minimal: Vec<&str> = minimal_phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names_minimal, ["pending", "in_progress", "done"]);
    }

    #[test]
    fn invalid_default_workflow_name_errors() {
        // default_workflow must name an existing key in workflows.
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "stack": "rust",
  "default_workflow": "nonexistent",
  "workflows": {
    "default": {
      "phases": [
        {"name":"pending"},
        {"name":"in_progress","model":"sonnet"},
        {"name":"done"}
      ]
    }
  }
}"#,
        )
        .unwrap();

        let err = load_project_config(&path, &global_config(3)).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { ref reason, .. } if reason.contains("nonexistent")),
            "expected Invalid error about 'nonexistent', got: {err}"
        );
    }
}
