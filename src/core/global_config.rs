use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_CONFIG_TOML: &str = "default_max_retries = 3\nhook_debug = false\non_retry_exhausted = \"block\"\n\n[runner]\nlease_ttl = \"5m\"\n";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    #[serde(default = "default_max_retries")]
    pub default_max_retries: u32,
    #[serde(default)]
    pub hook_debug: bool,
    #[serde(default = "default_on_retry_exhausted")]
    pub on_retry_exhausted: OnRetryExhausted,
    #[serde(default)]
    pub runner: RunnerConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    #[serde(default = "default_runner_lease_ttl")]
    pub lease_ttl: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnRetryExhausted {
    Block,
    Fail,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            default_max_retries: default_max_retries(),
            hook_debug: false,
            on_retry_exhausted: default_on_retry_exhausted(),
            runner: RunnerConfig::default(),
        }
    }
}

fn default_max_retries() -> u32 {
    3
}

fn default_runner_lease_ttl() -> String {
    "5m".to_owned()
}

fn default_on_retry_exhausted() -> OnRetryExhausted {
    OnRetryExhausted::Block
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            lease_ttl: default_runner_lease_ttl(),
        }
    }
}

impl RunnerConfig {
    pub fn lease_ttl_duration(&self) -> Result<Duration, String> {
        parse_duration(&self.lease_ttl)
            .map_err(|reason| format!("invalid runner.lease_ttl '{}': {reason}", self.lease_ttl))
    }
}

impl OnRetryExhausted {
    fn as_str(self) -> &'static str {
        match self {
            OnRetryExhausted::Block => "block",
            OnRetryExhausted::Fail => "fail",
        }
    }
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
            let config = toml::from_str::<GlobalConfig>(&contents).map_err(|error| {
                GlobalConfigError::Parse {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
            config
                .runner
                .lease_ttl_duration()
                .map_err(|message| GlobalConfigError::Parse { path, message })?;
            Ok(config)
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
    document.insert(
        "on_retry_exhausted".to_owned(),
        toml::Value::String(config.on_retry_exhausted.as_str().to_owned()),
    );
    let runner = document
        .entry("runner".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !runner.is_table() {
        *runner = toml::Value::Table(toml::Table::new());
    }
    if let Some(table) = runner.as_table_mut() {
        table.insert(
            "lease_ttl".to_owned(),
            toml::Value::String(config.runner.lease_ttl.clone()),
        );
    }

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

fn parse_duration(input: &str) -> Result<Duration, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("expected a duration like 5m".to_owned());
    }

    let (digits, unit) = input.split_at(
        input
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(input.len()),
    );
    if digits.is_empty() || unit.is_empty() {
        return Err("expected a duration like 5m".to_owned());
    }

    let amount: i64 = digits
        .parse()
        .map_err(|_| "expected a positive integer duration amount".to_owned())?;
    if amount <= 0 {
        return Err("duration must be positive".to_owned());
    }

    match unit {
        "s" => Ok(Duration::seconds(amount)),
        "m" => Ok(Duration::minutes(amount)),
        "h" => Ok(Duration::hours(amount)),
        "d" => Ok(Duration::days(amount)),
        _ => Err("supported units are s, m, h, and d".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_config_defaults_to_block_policy() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert_eq!(config.on_retry_exhausted, OnRetryExhausted::Block);
    }

    #[test]
    fn global_config_defaults_runner_lease_ttl_to_five_minutes() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert_eq!(
            config.runner.lease_ttl_duration().expect("parse ttl"),
            Duration::minutes(5)
        );
    }

    #[test]
    fn global_config_parses_runner_lease_ttl_duration() {
        let config: GlobalConfig =
            toml::from_str("[runner]\nlease_ttl = \"15m\"\n").expect("deserialize config");

        assert_eq!(
            config.runner.lease_ttl_duration().expect("parse ttl"),
            Duration::minutes(15)
        );
    }

    #[test]
    fn global_config_rejects_invalid_runner_lease_ttl() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        fs::write(
            dir.path().join("config.toml"),
            "[runner]\nlease_ttl = \"soon\"\n",
        )
        .expect("write config");

        let error = load_or_create(dir.path()).expect_err("invalid ttl");

        assert!(error.to_string().contains("invalid global config"));
        assert!(error.to_string().contains("invalid runner.lease_ttl"));
    }

    #[test]
    fn global_config_fail_policy_round_trips() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let config = GlobalConfig {
            on_retry_exhausted: OnRetryExhausted::Fail,
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(loaded.on_retry_exhausted, OnRetryExhausted::Fail);
    }
}
