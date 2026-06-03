use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_CONFIG_TOML: &str = "default_max_retries = 3\ndefault_model = \"sonnet\"\n";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    #[serde(default = "default_max_retries")]
    pub default_max_retries: u32,
    #[serde(default = "default_model")]
    pub default_model: String,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            default_max_retries: default_max_retries(),
            default_model: default_model(),
        }
    }
}

fn default_max_retries() -> u32 {
    3
}

fn default_model() -> String {
    "sonnet".to_owned()
}

#[derive(Debug, Error)]
pub enum GlobalConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid global config at ~/.agira/config.toml: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn load_or_create(agira_root: &Path) -> Result<GlobalConfig, GlobalConfigError> {
    let path = agira_root.join("config.toml");

    match fs::read_to_string(&path) {
        Ok(contents) => {
            toml::from_str::<GlobalConfig>(&contents).map_err(|error| GlobalConfigError::Parse {
                path,
                message: error.to_string(),
            })
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            write_default_config(&path)?;
            Ok(GlobalConfig::default())
        }
        Err(source) => Err(GlobalConfigError::Read { path, source }),
    }
}

fn write_default_config(path: &Path) -> Result<(), GlobalConfigError> {
    let temporary_path = path.with_extension("toml.tmp");

    fs::write(&temporary_path, DEFAULT_CONFIG_TOML)
        .and_then(|_| fs::rename(&temporary_path, path))
        .map_err(|source| GlobalConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn default_config_written_on_first_run() {
        let agira_root = TempDir::new().unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 3);
        assert_eq!(config.default_model, "sonnet");
        assert_eq!(
            fs::read_to_string(agira_root.path().join("config.toml")).unwrap(),
            DEFAULT_CONFIG_TOML
        );
    }

    #[test]
    fn existing_config_loaded() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 5\ndefault_model = \"opus\"\n",
        )
        .unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 5);
        assert_eq!(config.default_model, "opus");
    }

    #[test]
    fn malformed_toml_returns_parse_error() {
        let agira_root = TempDir::new().unwrap();
        let path = agira_root.path().join("config.toml");
        fs::write(&path, "[invalid").unwrap();

        let error = load_or_create(agira_root.path()).unwrap_err();

        match error {
            GlobalConfigError::Parse {
                path: error_path, ..
            } => assert_eq!(error_path, path),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn unknown_keys_ignored() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 7\nextra = \"ignored\"\n",
        )
        .unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 7);
        assert_eq!(config.default_model, "sonnet");
    }
}
