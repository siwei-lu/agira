use std::{fmt::Write, path::Path};

use thiserror::Error;

use crate::core::global_config::{
    GlobalConfig, GlobalConfigError, OnRetryExhausted, load_or_create, save_global_config,
};

const LEGACY_ALIASES: &[(&str, &str)] = &[
    ("hook-debug", "hook_debug"),
    ("default-max-retries", "default_max_retries"),
];

type Getter = fn(&GlobalConfig) -> String;
type Setter = fn(&mut GlobalConfig, &str, &str) -> Result<(), ConfigCommandError>;

struct ConfigEntry {
    cli_key: &'static str,
    kind: ConfigValueKind,
    getter: Getter,
    setter: Setter,
}

#[derive(Clone, Copy)]
enum ConfigValueKind {
    Bool,
    U32,
    DurationString,
    OnRetryExhausted,
    String,
}

impl ConfigValueKind {
    fn help_label(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U32 => "u32",
            Self::DurationString => "duration string, e.g. 5m",
            Self::OnRetryExhausted => "block|fail",
            Self::String => "string",
        }
    }
}

const CONFIG_REGISTRY: &[ConfigEntry] = &[
    ConfigEntry {
        cli_key: "hook_debug",
        kind: ConfigValueKind::Bool,
        getter: |config| config.hook_debug.to_string(),
        setter: set_hook_debug,
    },
    ConfigEntry {
        cli_key: "default_max_retries",
        kind: ConfigValueKind::U32,
        getter: |config| config.default_max_retries.to_string(),
        setter: set_default_max_retries,
    },
    ConfigEntry {
        cli_key: "on_retry_exhausted",
        kind: ConfigValueKind::OnRetryExhausted,
        getter: |config| config.on_retry_exhausted.as_str().to_owned(),
        setter: set_on_retry_exhausted,
    },
    ConfigEntry {
        cli_key: "runner.auto_start",
        kind: ConfigValueKind::Bool,
        getter: |config| config.runner.auto_start.to_string(),
        setter: set_runner_auto_start,
    },
    ConfigEntry {
        cli_key: "runner.lease_ttl",
        kind: ConfigValueKind::DurationString,
        getter: |config| config.runner.lease_ttl.clone(),
        setter: set_runner_lease_ttl,
    },
    ConfigEntry {
        cli_key: "runner.type",
        kind: ConfigValueKind::String,
        getter: |config| config.runner.runner_type.clone(),
        setter: set_runner_type,
    },
];

#[derive(Debug, Error)]
pub enum ConfigCommandError {
    #[error("home directory missing")]
    HomeDirectoryMissing,

    #[error("unknown config key: {key}; valid keys: {valid_keys}")]
    UnknownKey { key: String, valid_keys: String },

    #[error("invalid value for {key}: {value}; expected {expected}")]
    InvalidValue {
        key: String,
        value: String,
        expected: &'static str,
    },

    #[error("invalid value for {key}: {value}; {reason}")]
    InvalidDurationValue {
        key: String,
        value: String,
        reason: String,
    },

    #[error(transparent)]
    GlobalConfig(#[from] GlobalConfigError),

    #[error("failed to save global config: {0}")]
    Save(#[source] anyhow::Error),
}

pub fn run_config_get(agira_root: &Path) -> Result<(), ConfigCommandError> {
    let config = load_or_create(agira_root)?;
    print_config_output(&format_config_get(&config));

    Ok(())
}

pub fn run_config_set(agira_root: &Path, key: &str, value: &str) -> Result<(), ConfigCommandError> {
    let canonical_key = canonical_key(key);
    let entry = find_entry(canonical_key).ok_or_else(|| ConfigCommandError::UnknownKey {
        key: key.to_owned(),
        valid_keys: valid_keys().join(", "),
    })?;

    let mut config = load_or_create(agira_root)?;
    (entry.setter)(&mut config, entry.cli_key, value)?;
    save_global_config(agira_root, &config).map_err(ConfigCommandError::Save)?;
    print_config_output(&format!(
        "{} = {}\n",
        entry.cli_key,
        (entry.getter)(&config)
    ));

    Ok(())
}

pub fn config_keys_help() -> String {
    let mut help = String::from("Valid config keys:\n\n");
    for key in valid_keys() {
        writeln!(help, "  {key}").expect("write config help");
    }
    help.push_str("\nKeys use the same snake_case dotted paths as ~/.agira/config.toml.");
    help
}

pub fn config_get_help() -> String {
    let mut help = String::from("Displayed config keys:\n\n");
    for key in valid_keys() {
        writeln!(help, "  {key}").expect("write config get help");
    }
    help
}

pub fn config_set_help() -> String {
    let key_width = CONFIG_REGISTRY
        .iter()
        .map(|entry| entry.cli_key.len())
        .max()
        .unwrap_or(0);
    let mut help = String::from("Valid keys:\n\n");
    for entry in CONFIG_REGISTRY {
        writeln!(
            help,
            "  {key:<key_width$}  {kind}",
            key = entry.cli_key,
            kind = entry.kind.help_label()
        )
        .expect("write config set help");
    }
    help.push_str("\nLegacy aliases hook-debug and default-max-retries are still accepted.");
    help
}

fn set_hook_debug(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    config.hook_debug = parse_bool_value(key, value)?;
    Ok(())
}

fn set_default_max_retries(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    config.default_max_retries =
        value
            .parse::<u32>()
            .map_err(|_| ConfigCommandError::InvalidValue {
                key: key.to_owned(),
                value: value.to_owned(),
                expected: "an unsigned 32-bit integer",
            })?;
    Ok(())
}

fn set_on_retry_exhausted(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    config.on_retry_exhausted = match value {
        "block" => OnRetryExhausted::Block,
        "fail" => OnRetryExhausted::Fail,
        _ => {
            return Err(ConfigCommandError::InvalidValue {
                key: key.to_owned(),
                value: value.to_owned(),
                expected: "block or fail",
            });
        }
    };
    Ok(())
}

fn set_runner_auto_start(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    config.runner.auto_start = parse_bool_value(key, value)?;
    Ok(())
}

fn set_runner_lease_ttl(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    let previous = std::mem::replace(&mut config.runner.lease_ttl, value.to_owned());
    match config.runner.lease_ttl_duration() {
        Ok(_) => Ok(()),
        Err(reason) => {
            config.runner.lease_ttl = previous;
            Err(ConfigCommandError::InvalidDurationValue {
                key: key.to_owned(),
                value: value.to_owned(),
                reason,
            })
        }
    }
}

fn set_runner_type(
    config: &mut GlobalConfig,
    _key: &str,
    value: &str,
) -> Result<(), ConfigCommandError> {
    config.runner.runner_type = value.to_owned();
    Ok(())
}

fn parse_bool_value(key: &str, value: &str) -> Result<bool, ConfigCommandError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ConfigCommandError::InvalidValue {
            key: key.to_owned(),
            value: value.to_owned(),
            expected: "true or false",
        }),
    }
}

fn format_config_get(config: &GlobalConfig) -> String {
    let mut output = String::new();
    for entry in CONFIG_REGISTRY {
        writeln!(output, "{} = {}", entry.cli_key, (entry.getter)(config))
            .expect("write config output");
    }
    output
}

fn find_entry(key: &str) -> Option<&'static ConfigEntry> {
    CONFIG_REGISTRY.iter().find(|entry| entry.cli_key == key)
}

fn canonical_key(key: &str) -> &str {
    LEGACY_ALIASES
        .iter()
        .find_map(|(alias, canonical)| (*alias == key).then_some(*canonical))
        .unwrap_or(key)
}

fn valid_keys() -> Vec<&'static str> {
    CONFIG_REGISTRY.iter().map(|entry| entry.cli_key).collect()
}

fn print_config_output(output: &str) {
    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(captured) = capture.borrow_mut().as_mut() {
            captured.push_str(output);
        }
    });

    print!("{output}");
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::core::global_config::load_or_create;

    fn capture_output(action: impl FnOnce()) -> String {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });

        action();

        OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().expect("captured output"))
    }

    #[test]
    fn config_get_output_includes_every_registry_key() {
        let config = GlobalConfig::default();
        let output = format_config_get(&config);

        for key in valid_keys() {
            assert!(
                output.contains(&format!("{key} = ")),
                "expected config get output to include {key}"
            );
        }
        assert!(!output.contains("hook-debug"));
        assert!(!output.contains("default-max-retries"));
    }

    #[test]
    fn runner_auto_start_round_trips_false_and_true_through_config_set() {
        for value in ["true", "false"] {
            let dir = tempfile::TempDir::new().expect("create temp dir");

            run_config_set(dir.path(), "runner.auto_start", value).expect("set auto_start");
            let loaded = load_or_create(dir.path()).expect("load config");

            assert_eq!(loaded.runner.auto_start, value == "true");
        }
    }

    #[test]
    fn unknown_key_error_lists_registry_canonical_keys() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let error = run_config_set(dir.path(), "runner.missing", "true").expect_err("unknown key");
        let message = error.to_string();

        for key in valid_keys() {
            assert!(
                message.contains(key),
                "expected unknown-key message to include {key}: {message}"
            );
        }
        assert!(!message.contains("hook-debug"));
        assert!(!message.ends_with('.'));
    }

    #[test]
    fn invalid_bool_value_has_kind_specific_error() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let error =
            run_config_set(dir.path(), "runner.auto_start", "yes").expect_err("invalid bool");

        assert_eq!(
            error.to_string(),
            "invalid value for runner.auto_start: yes; expected true or false"
        );
    }

    #[test]
    fn invalid_u32_value_has_kind_specific_error() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let error =
            run_config_set(dir.path(), "default_max_retries", "many").expect_err("invalid u32");

        assert_eq!(
            error.to_string(),
            "invalid value for default_max_retries: many; expected an unsigned 32-bit integer"
        );
    }

    #[test]
    fn invalid_enum_value_has_kind_specific_error() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let error =
            run_config_set(dir.path(), "on_retry_exhausted", "retry").expect_err("invalid enum");

        assert_eq!(
            error.to_string(),
            "invalid value for on_retry_exhausted: retry; expected block or fail"
        );
    }

    #[test]
    fn invalid_duration_value_reuses_existing_duration_validation() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let error =
            run_config_set(dir.path(), "runner.lease_ttl", "soon").expect_err("invalid duration");
        let message = error.to_string();

        assert!(message.contains("invalid value for runner.lease_ttl: soon"));
        assert!(message.contains("invalid runner.lease_ttl 'soon'"));
        assert!(!message.ends_with('.'));
    }

    #[test]
    fn legacy_kebab_aliases_set_underlying_fields() {
        let dir = tempfile::TempDir::new().expect("create temp dir");

        run_config_set(dir.path(), "hook-debug", "true").expect("set hook_debug alias");
        run_config_set(dir.path(), "default-max-retries", "7").expect("set retries alias");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert!(loaded.hook_debug);
        assert_eq!(loaded.default_max_retries, 7);
    }

    #[test]
    fn config_set_prints_canonical_key_even_when_alias_is_used() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let output = capture_output(|| {
            run_config_set(dir.path(), "hook-debug", "true").expect("set hook_debug alias");
        });

        assert_eq!(output, "hook_debug = true\n");
    }

    #[test]
    fn config_set_writes_runner_values_through_save_merge_path() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        fs::write(
            dir.path().join("config.toml"),
            "# keep me\ncustom_key = \"untouched\"\n[runner]\nlease_ttl = \"5m\"\n",
        )
        .expect("write existing config");

        run_config_set(dir.path(), "runner.lease_ttl", "10m").expect("set lease ttl");
        run_config_set(dir.path(), "runner.type", "custom-runner").expect("set runner type");
        let contents = fs::read_to_string(dir.path().join("config.toml")).expect("read config");
        let loaded = load_or_create(dir.path()).expect("load config");

        assert!(contents.contains("custom_key = \"untouched\""));
        assert_eq!(loaded.runner.lease_ttl, "10m");
        assert_eq!(loaded.runner.runner_type, "custom-runner");
    }
}
