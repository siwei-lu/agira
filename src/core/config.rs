use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::global_config::GlobalConfig;

pub const INITIAL_PHASE_NAME: &str = "pending";
pub const TERMINAL_PHASE_NAME: &str = "done";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PhaseConfig {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub stack: String,
    pub phases: Vec<PhaseConfig>,
    pub verification: VerificationConfig,
    pub acceptance_testing: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prd_path: Option<String>,
}

impl Config {
    pub fn terminal_phase(&self) -> Option<&str> {
        self.phases.last().map(|p| p.name.as_str())
    }
}

fn default_max_retries() -> u32 {
    3
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VerificationConfig {
    pub commands: Vec<String>,
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
    // New format
    #[serde(default)]
    phases: Option<Vec<PhaseConfig>>,
    // Old format (backward compat)
    #[serde(default)]
    state_machine: Option<Vec<String>>,
    #[serde(default)]
    models: Option<BTreeMap<String, String>>,
    verification: VerificationConfig,
    acceptance_testing: String,
    max_retries: Option<u32>,
    // Legacy fallback used only for old-format migration.
    #[serde(default)]
    default_model: Option<String>,
    prd_path: Option<String>,
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

    let phases = match project_config.phases {
        Some(phases) => phases,
        None => migrate_state_machine(
            project_config.state_machine.unwrap_or_default(),
            project_config.models.as_ref(),
            project_config.default_model.as_deref(),
        ),
    };
    let phases = normalize_mandatory_phases(phases);
    validate_terminal_phase(&phases).map_err(|reason| ConfigError::Invalid {
        path: path.to_path_buf(),
        reason,
    })?;

    Ok(Config {
        stack: project_config.stack,
        phases,
        verification: project_config.verification,
        acceptance_testing: project_config.acceptance_testing,
        max_retries: project_config
            .max_retries
            .unwrap_or(global_config.default_max_retries),
        prd_path: project_config.prd_path,
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
            PhaseConfig { name, model }
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

    // Mandatory phases have no model — they are transition phases, not AI-driven phases.
    // Strip any model that might have been set on them (e.g. from legacy config or user override).
    let initial_phase = initial_phase
        .map(|p| PhaseConfig {
            name: p.name,
            model: None,
        })
        .unwrap_or_else(|| PhaseConfig {
            name: INITIAL_PHASE_NAME.to_owned(),
            model: None,
        });
    let terminal_phase = terminal_phase
        .map(|p| PhaseConfig {
            name: p.name,
            model: None,
        })
        .unwrap_or_else(|| PhaseConfig {
            name: TERMINAL_PHASE_NAME.to_owned(),
            model: None,
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
        }
    }

    fn write_new_format_config(path: &Path, fields: &str) {
        fs::write(
            path,
            format!(
                r#"{{
  "stack": "rust",
  "phases": [{{"name":"enriching","model":"opus"}},{{"name":"done"}}],
  "verification": {{ "commands": [] }},
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
  "verification": { "commands": [] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();
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
  "verification": { "commands": [] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases.len(), 4);
        assert_eq!(config.phases[0].name, "pending");
        assert_eq!(config.phases[0].model, None);
        assert_eq!(config.phases[1].name, "enriching");
        assert_eq!(config.phases[1].model, Some("opus".to_owned()));
        assert_eq!(config.phases[2].name, "in_progress");
        assert_eq!(config.phases[2].model, Some("sonnet".to_owned()));
        assert_eq!(config.phases[3].name, "done");
        assert_eq!(config.phases[3].model, None);
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
  "verification": { "commands": [] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let names: Vec<&str> = config.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["pending", "enriching", "review", "done"]);
        // Mandatory phases always have no model regardless of what the JSON says.
        assert_eq!(config.phases[0].model, None);
        assert_eq!(config.phases[3].model, None);
    }

    #[test]
    fn old_format_migrated_using_global_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_old_format_config(&path);

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases.len(), 3);
        assert_eq!(config.phases[0].name, "pending");
        assert_eq!(config.phases[0].model, None);
        assert_eq!(config.phases[1].name, "enriching");
        assert_eq!(config.phases[1].model, Some("sonnet".to_owned()));
        assert_eq!(config.phases[2].name, "done");
        assert_eq!(config.phases[2].model, None);
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
  "verification": { "commands": [] },
  "acceptance_testing": "cli"
}"#,
        )
        .unwrap();

        let config = load_project_config(&path, &global_config(3)).unwrap();

        let names: Vec<&str> = config.phases.iter().map(|p| p.name.as_str()).collect();
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
    fn project_config_explicit_max_retries_wins() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_new_format_config(&path, r#", "max_retries": 7"#);

        let config = load_project_config(&path, &global_config(5)).unwrap();

        assert_eq!(config.max_retries, 7);
    }

    #[test]
    fn terminal_phase_returns_last_phase() {
        let config = Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                },
            ],
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
        };

        assert_eq!(config.terminal_phase(), Some("done"));
    }
}
