use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_CONFIG_TOML: &str = r#"default_max_retries = 3
hook_debug = false
on_retry_exhausted = "block"

[runner]
lease_ttl = "5m"

# [runner.claude]
# command = "claude"
# model = "sonnet"
# permission_mode = "auto"
# valid permission_mode choices: auto, acceptEdits, bypassPermissions, dontAsk, default, plan
# settings_path = ""
# extra_args = []
#
# [runner.claude.env]
"#;

const VALID_CLAUDE_PERMISSION_MODES: &[&str] = &[
    "auto",
    "acceptEdits",
    "bypassPermissions",
    "dontAsk",
    "default",
    "plan",
];

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
    #[serde(default)]
    pub orchestrator_template_path: Option<PathBuf>,
    #[serde(rename = "type", default = "default_runner_type")]
    pub runner_type: String,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub claude: ClaudeRunnerConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRunnerConfig {
    #[serde(default = "default_claude_runner_command")]
    pub command: String,
    #[serde(default = "default_claude_runner_model")]
    pub model: String,
    #[serde(default = "default_claude_runner_permission_mode")]
    pub permission_mode: String,
    #[serde(default)]
    pub settings_path: Option<PathBuf>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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

fn default_runner_type() -> String {
    "claude-tmux".to_owned()
}

fn default_claude_runner_command() -> String {
    "claude".to_owned()
}

fn default_claude_runner_model() -> String {
    "sonnet".to_owned()
}

fn default_claude_runner_permission_mode() -> String {
    "auto".to_owned()
}

fn default_on_retry_exhausted() -> OnRetryExhausted {
    OnRetryExhausted::Block
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            lease_ttl: default_runner_lease_ttl(),
            orchestrator_template_path: None,
            runner_type: default_runner_type(),
            auto_start: false,
            claude: ClaudeRunnerConfig::default(),
        }
    }
}

impl RunnerConfig {
    pub fn lease_ttl_duration(&self) -> Result<Duration, String> {
        parse_duration(&self.lease_ttl)
            .map_err(|reason| format!("invalid runner.lease_ttl '{}': {reason}", self.lease_ttl))
    }
}

impl Default for ClaudeRunnerConfig {
    fn default() -> Self {
        Self {
            command: default_claude_runner_command(),
            model: default_claude_runner_model(),
            permission_mode: default_claude_runner_permission_mode(),
            settings_path: None,
            extra_args: Vec::new(),
            env: BTreeMap::new(),
        }
    }
}

impl ClaudeRunnerConfig {
    pub fn validate_permission_mode(&self) -> Result<(), String> {
        if VALID_CLAUDE_PERMISSION_MODES.contains(&self.permission_mode.as_str()) {
            return Ok(());
        }

        Err(format!(
            "invalid runner.claude.permission_mode '{}': expected one of {}",
            self.permission_mode,
            VALID_CLAUDE_PERMISSION_MODES.join(", ")
        ))
    }
}

impl OnRetryExhausted {
    pub fn as_str(self) -> &'static str {
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
                .map_err(|message| GlobalConfigError::Parse {
                    path: path.clone(),
                    message,
                })?;
            config
                .runner
                .claude
                .validate_permission_mode()
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
        match &config.runner.orchestrator_template_path {
            Some(path) => {
                table.insert(
                    "orchestrator_template_path".to_owned(),
                    toml::Value::String(path.to_string_lossy().into_owned()),
                );
            }
            None => {
                table.remove("orchestrator_template_path");
            }
        }
        table.insert(
            "type".to_owned(),
            toml::Value::String(config.runner.runner_type.clone()),
        );
        table.insert(
            "auto_start".to_owned(),
            toml::Value::Boolean(config.runner.auto_start),
        );
        let claude = table
            .entry("claude".to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !claude.is_table() {
            *claude = toml::Value::Table(toml::Table::new());
        }
        if let Some(claude_table) = claude.as_table_mut() {
            claude_table.insert(
                "command".to_owned(),
                toml::Value::String(config.runner.claude.command.clone()),
            );
            claude_table.insert(
                "model".to_owned(),
                toml::Value::String(config.runner.claude.model.clone()),
            );
            claude_table.insert(
                "permission_mode".to_owned(),
                toml::Value::String(config.runner.claude.permission_mode.clone()),
            );
            match &config.runner.claude.settings_path {
                Some(path) => {
                    claude_table.insert(
                        "settings_path".to_owned(),
                        toml::Value::String(path.to_string_lossy().into_owned()),
                    );
                }
                None => {
                    claude_table.remove("settings_path");
                }
            }
            claude_table.insert(
                "extra_args".to_owned(),
                toml::Value::Array(
                    config
                        .runner
                        .claude
                        .extra_args
                        .iter()
                        .cloned()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
            claude_table.insert(
                "env".to_owned(),
                toml::Value::Table(
                    config
                        .runner
                        .claude
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
                        .collect(),
                ),
            );
        }
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
    fn global_config_defaults_orchestrator_template_path_to_none() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert_eq!(config.runner.orchestrator_template_path, None);
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

    #[test]
    fn global_config_orchestrator_template_path_round_trips() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let template_path = PathBuf::from("/tmp/agira/orchestrator-template.md");
        let config = GlobalConfig {
            runner: RunnerConfig {
                orchestrator_template_path: Some(template_path.clone()),
                ..RunnerConfig::default()
            },
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(
            loaded.runner.orchestrator_template_path,
            Some(template_path)
        );
    }

    #[test]
    fn runner_config_defaults_runner_type_to_claude_tmux() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert_eq!(config.runner.runner_type, "claude-tmux");
    }

    #[test]
    fn runner_config_defaults_auto_start_to_false() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert!(!config.runner.auto_start);
    }

    #[test]
    fn claude_runner_config_defaults_when_section_absent() {
        let config: GlobalConfig = toml::from_str("").expect("deserialize empty config");

        assert_eq!(config.runner.claude.command, "claude");
        assert_eq!(config.runner.claude.model, "sonnet");
        assert_eq!(config.runner.claude.permission_mode, "auto");
        assert_eq!(config.runner.claude.settings_path, None);
        assert!(config.runner.claude.extra_args.is_empty());
        assert!(config.runner.claude.env.is_empty());
    }

    #[test]
    fn claude_runner_config_full_round_trips_through_save_and_load() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let mut env = BTreeMap::new();
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            "https://example.test".to_owned(),
        );
        env.insert("HTTPS_PROXY".to_owned(), "http://127.0.0.1:8080".to_owned());
        let claude = ClaudeRunnerConfig {
            command: "/opt/bin/claude-wrapper".to_owned(),
            model: "opus".to_owned(),
            permission_mode: "dontAsk".to_owned(),
            settings_path: Some(PathBuf::from("/tmp/claude-settings.json")),
            extra_args: vec![
                "--add-dir".to_owned(),
                "/tmp/work".to_owned(),
                "--mcp-config".to_owned(),
            ],
            env,
        };
        let config = GlobalConfig {
            runner: RunnerConfig {
                claude: claude.clone(),
                ..RunnerConfig::default()
            },
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(loaded.runner.claude, claude);
    }

    #[test]
    fn claude_runner_config_rejects_invalid_permission_mode_at_load() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        fs::write(
            dir.path().join("config.toml"),
            "[runner.claude]\npermission_mode = \"yolo\"\n",
        )
        .expect("write config");

        let error = load_or_create(dir.path()).expect_err("invalid permission mode");
        let message = error.to_string();

        assert!(message.contains("invalid global config"));
        assert!(message.contains("runner.claude.permission_mode"));
        assert!(message.contains("yolo"));
        assert!(message.contains(
            "expected one of auto, acceptEdits, bypassPermissions, dontAsk, default, plan"
        ));
        assert!(!message.ends_with('.'));
    }

    #[test]
    fn claude_runner_config_parses_env_table() {
        let config: GlobalConfig = toml::from_str(
            "[runner.claude.env]\nANTHROPIC_BASE_URL = \"https://example.test\"\nHTTPS_PROXY = \"http://127.0.0.1:8080\"\n",
        )
        .expect("deserialize config");

        assert_eq!(
            config.runner.claude.env.get("ANTHROPIC_BASE_URL"),
            Some(&"https://example.test".to_owned())
        );
        assert_eq!(
            config.runner.claude.env.get("HTTPS_PROXY"),
            Some(&"http://127.0.0.1:8080".to_owned())
        );
    }

    #[test]
    fn runner_config_without_claude_section_loads_default_claude_config() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        fs::write(
            dir.path().join("config.toml"),
            "[runner]\nlease_ttl = \"10m\"\ntype = \"claude-tmux\"\nauto_start = true\n",
        )
        .expect("write config");

        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(loaded.runner.claude, ClaudeRunnerConfig::default());
    }

    #[test]
    fn every_claude_runner_permission_mode_is_accepted_at_load() {
        for permission_mode in VALID_CLAUDE_PERMISSION_MODES {
            let dir = tempfile::TempDir::new().expect("create temp dir");
            fs::write(
                dir.path().join("config.toml"),
                format!("[runner.claude]\npermission_mode = \"{permission_mode}\"\n"),
            )
            .expect("write config");

            let loaded = load_or_create(dir.path()).expect("load config");

            assert_eq!(loaded.runner.claude.permission_mode, *permission_mode);
        }
    }

    #[test]
    fn runner_type_round_trips_through_save_and_load() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let config = GlobalConfig {
            runner: RunnerConfig {
                runner_type: "custom-backend".to_owned(),
                ..RunnerConfig::default()
            },
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(loaded.runner.runner_type, "custom-backend");
    }

    #[test]
    fn auto_start_round_trips_through_save_and_load() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let config = GlobalConfig {
            runner: RunnerConfig {
                auto_start: true,
                ..RunnerConfig::default()
            },
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert!(loaded.runner.auto_start);
    }

    #[test]
    fn save_preserves_existing_runner_keys_alongside_new_fields() {
        // Simulate a config.toml already having lease_ttl, then save new fields
        let dir = tempfile::TempDir::new().expect("create temp dir");
        // Write a config with existing keys
        fs::write(
            dir.path().join("config.toml"),
            "[runner]\nlease_ttl = \"10m\"\norchestrator_template_path = \"/tmp/tmpl.md\"\n",
        )
        .expect("write existing config");

        let config = GlobalConfig {
            runner: RunnerConfig {
                lease_ttl: "10m".to_owned(),
                orchestrator_template_path: Some(PathBuf::from("/tmp/tmpl.md")),
                runner_type: "claude-tmux".to_owned(),
                auto_start: true,
                ..RunnerConfig::default()
            },
            ..GlobalConfig::default()
        };

        save_global_config(dir.path(), &config).expect("save config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert_eq!(loaded.runner.lease_ttl, "10m");
        assert_eq!(
            loaded.runner.orchestrator_template_path,
            Some(PathBuf::from("/tmp/tmpl.md"))
        );
        assert_eq!(loaded.runner.runner_type, "claude-tmux");
        assert!(loaded.runner.auto_start);
    }
}
