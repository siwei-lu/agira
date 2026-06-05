use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    pub on: String,
    pub run: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

#[derive(Debug, Error)]
pub enum HookConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid hooks config at {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

pub fn load_hooks(path: &Path) -> Result<HookConfig, HookConfigError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(HookConfig::default());
        }
        Err(source) => {
            return Err(HookConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    toml::from_str::<HookConfig>(&contents).map_err(|error| HookConfigError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn absent_file_returns_empty_config() {
        let temp_dir = TempDir::new().unwrap();

        let config = load_hooks(&temp_dir.path().join("hooks.toml")).unwrap();

        assert!(config.hooks.is_empty());
    }

    #[test]
    fn valid_toml_parses_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(
            &path,
            r#"
[[hooks]]
on = "done"
run = "echo done"

[[hooks]]
on = "*"
run = "echo changed"
"#,
        )
        .unwrap();

        let config = load_hooks(&path).unwrap();

        assert_eq!(
            config,
            HookConfig {
                hooks: vec![
                    HookEntry {
                        on: "done".to_owned(),
                        run: "echo done".to_owned(),
                    },
                    HookEntry {
                        on: "*".to_owned(),
                        run: "echo changed".to_owned(),
                    },
                ],
            }
        );
    }

    #[test]
    fn malformed_toml_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("hooks.toml");
        fs::write(&path, "[[hooks]]\non = [").unwrap();

        let error = load_hooks(&path).unwrap_err();

        match error {
            HookConfigError::Parse {
                path: error_path, ..
            } => assert_eq!(error_path, path),
            other => panic!("unexpected error: {other}"),
        }
    }
}
