use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::global_config::GlobalConfig;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub stack: String,
    pub state_machine: Vec<String>,
    pub models: BTreeMap<String, String>,
    pub verification: VerificationConfig,
    pub acceptance_testing: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_model", skip_serializing_if = "is_default_model")]
    pub default_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prd_path: Option<String>,
}

fn default_max_retries() -> u32 {
    3
}

fn default_model() -> String {
    "sonnet".to_owned()
}

fn is_default_model(value: &str) -> bool {
    value == "sonnet"
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
}

#[derive(Deserialize)]
struct ProjectConfigFile {
    stack: String,
    state_machine: Vec<String>,
    models: BTreeMap<String, String>,
    verification: VerificationConfig,
    acceptance_testing: String,
    max_retries: Option<u32>,
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

    Ok(Config {
        stack: project_config.stack,
        state_machine: project_config.state_machine,
        models: project_config.models,
        verification: project_config.verification,
        acceptance_testing: project_config.acceptance_testing,
        max_retries: project_config
            .max_retries
            .unwrap_or(global_config.default_max_retries),
        default_model: project_config
            .default_model
            .unwrap_or_else(|| global_config.default_model.clone()),
        prd_path: project_config.prd_path,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn global_config(default_max_retries: u32, default_model: &str) -> GlobalConfig {
        GlobalConfig {
            default_max_retries,
            default_model: default_model.to_owned(),
        }
    }

    fn write_project_config(path: &Path, fields: &str) {
        fs::write(
            path,
            format!(
                r#"{{
  "stack": "rust",
  "state_machine": ["enriching", "done"],
  "models": {{}},
  "verification": {{ "commands": [] }},
  "acceptance_testing": "cli"{fields}
}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn project_config_uses_global_defaults_for_omitted_fields() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_project_config(&path, "");

        let config = load_project_config(&path, &global_config(5, "opus")).unwrap();

        assert_eq!(config.max_retries, 5);
        assert_eq!(config.default_model, "opus");
    }

    #[test]
    fn project_config_explicit_fields_win_over_global_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.json");
        write_project_config(&path, r#", "max_retries": 3, "default_model": "haiku""#);

        let config = load_project_config(&path, &global_config(5, "opus")).unwrap();

        assert_eq!(config.max_retries, 3);
        assert_eq!(config.default_model, "haiku");
    }
}
