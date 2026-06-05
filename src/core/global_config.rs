use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_CONFIG_TOML: &str = "default_max_retries = 3\nhook_debug = false\n";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    #[serde(default = "default_max_retries")]
    pub default_max_retries: u32,
    #[serde(default)]
    pub hook_debug: bool,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            default_max_retries: default_max_retries(),
            hook_debug: false,
        }
    }
}

fn default_max_retries() -> u32 {
    3
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

pub fn save_global_config(agira_root: &Path, config: &GlobalConfig) -> anyhow::Result<()> {
    let path = agira_root.join("config.toml");
    let mut document = match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<toml::Table>(&contents)
            .with_context(|| format!("invalid global config at {}", path.display()))?,
        Err(source) if source.kind() == io::ErrorKind::NotFound => toml::Table::new(),
        Err(source) => {
            return Err(source).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    document.insert(
        "default_max_retries".to_owned(),
        toml::Value::Integer(i64::from(config.default_max_retries)),
    );
    document.insert(
        "hook_debug".to_owned(),
        toml::Value::Boolean(config.hook_debug),
    );

    let contents = toml::to_string_pretty(&document)
        .with_context(|| format!("failed to serialize global config for {}", path.display()))?;

    write_config_file(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn write_default_config(path: &Path) -> Result<(), GlobalConfigError> {
    write_config_file(path, DEFAULT_CONFIG_TOML.to_owned()).map_err(|source| {
        GlobalConfigError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

fn write_config_file(path: &Path, contents: String) -> Result<(), io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temporary_path = path.with_extension("toml.tmp");

    fs::write(&temporary_path, contents).and_then(|_| fs::rename(&temporary_path, path))
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
        assert!(!config.hook_debug);
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
            "default_max_retries = 5\n",
        )
        .unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 5);
        assert!(!config.hook_debug);
    }

    #[test]
    fn hook_debug_loaded_from_existing_config() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 5\nhook_debug = true\n",
        )
        .unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 5);
        assert!(config.hook_debug);
    }

    #[test]
    fn old_config_with_default_model_ignored() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 5\ndefault_model = \"opus\"\n",
        )
        .unwrap();

        let config = load_or_create(agira_root.path()).unwrap();

        assert_eq!(config.default_max_retries, 5);
        assert!(!config.hook_debug);
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
        assert!(!config.hook_debug);
    }

    #[test]
    fn save_global_config_writes_config_atomically() {
        let agira_root = TempDir::new().unwrap();
        let config = GlobalConfig {
            default_max_retries: 11,
            hook_debug: true,
        };

        save_global_config(agira_root.path(), &config).unwrap();

        let contents = fs::read_to_string(agira_root.path().join("config.toml")).unwrap();
        assert!(contents.contains("default_max_retries = 11"));
        assert!(contents.contains("hook_debug = true"));
        assert!(!agira_root.path().join("config.toml.tmp").exists());
        assert_eq!(load_or_create(agira_root.path()).unwrap(), config);
    }

    #[test]
    fn save_global_config_preserves_global_hooks() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            r#"default_max_retries = 3

[[hooks]]
on = "done"
run = "echo done"
"#,
        )
        .unwrap();

        save_global_config(
            agira_root.path(),
            &GlobalConfig {
                default_max_retries: 7,
                hook_debug: true,
            },
        )
        .unwrap();

        let contents = fs::read_to_string(agira_root.path().join("config.toml")).unwrap();
        assert!(contents.contains("default_max_retries = 7"));
        assert!(contents.contains("hook_debug = true"));
        assert!(contents.contains("[[hooks]]"));
        assert!(contents.contains("on = \"done\""));
        assert!(contents.contains("run = \"echo done\""));
    }

    #[test]
    fn load_or_create_creates_missing_agira_root() {
        let parent = TempDir::new().unwrap();
        let agira_root = parent.path().join(".agira");

        let config = load_or_create(&agira_root).unwrap();

        assert_eq!(config, GlobalConfig::default());
        assert_eq!(
            fs::read_to_string(agira_root.join("config.toml")).unwrap(),
            DEFAULT_CONFIG_TOML
        );
    }
}
