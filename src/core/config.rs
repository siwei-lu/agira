use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::global_config::GlobalConfig;

pub const TERMINAL_PHASE_NAME: &str = "done";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PhaseConfig {
    pub name: String,
    pub model: String,
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
            let model = models
                .and_then(|m| m.get(&name))
                .map(String::as_str)
                .unwrap_or(fallback)
                .to_owned();
            PhaseConfig { name, model }
        })
        .collect()
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
  "phases": [{{"name":"enriching","model":"opus"}},{{"name":"done","model":"haiku"}}],
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

    fn write_config_with_terminal_phase(path: &Path, terminal_phase: &str) {
        fs::write(
            path,
            format!(
                r#"{{
  "stack": "rust",
  "phases": [{{"name":"enriching","model":"opus"}},{{"name":"{terminal_phase}","model":"haiku"}}],
  "verification": {{ "commands": [] }},
  "acceptance_testing": "cli"
}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn new_format_loads_phases_directly() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_new_format_config(&path, "");

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases.len(), 2);
        assert_eq!(config.phases[0].name, "enriching");
        assert_eq!(config.phases[0].model, "opus");
        assert_eq!(config.phases[1].name, "done");
        assert_eq!(config.phases[1].model, "haiku");
    }

    #[test]
    fn new_format_rejects_non_done_terminal_phase() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_config_with_terminal_phase(&path, "verifying");

        let error = load_project_config(&path, &global_config(3)).unwrap_err();

        match error {
            ConfigError::Invalid { reason, .. } => {
                assert_eq!(
                    reason,
                    "last phase must be named 'done' (found 'verifying')"
                );
            }
            other => panic!("expected invalid config, got: {other}"),
        }
    }

    #[test]
    fn old_format_migrated_using_global_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_old_format_config(&path);

        let config = load_project_config(&path, &global_config(3)).unwrap();

        assert_eq!(config.phases.len(), 2);
        assert_eq!(config.phases[0].name, "enriching");
        assert_eq!(config.phases[0].model, "sonnet");
        assert_eq!(config.phases[1].name, "done");
        assert_eq!(config.phases[1].model, "sonnet");
    }

    #[test]
    fn old_format_rejects_non_done_terminal_phase_after_migration() {
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

        let error = load_project_config(&path, &global_config(3)).unwrap_err();

        match error {
            ConfigError::Invalid { reason, .. } => {
                assert_eq!(
                    reason,
                    "last phase must be named 'done' (found 'verifying')"
                );
            }
            other => panic!("expected invalid config, got: {other}"),
        }
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
                    model: "opus".to_owned(),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: "haiku".to_owned(),
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
