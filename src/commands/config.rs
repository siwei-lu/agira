use std::path::Path;

use thiserror::Error;

use crate::core::global_config::{GlobalConfigError, load_or_create, save_global_config};

const HOOK_DEBUG_KEY: &str = "hook-debug";

#[derive(Debug, Error)]
pub enum ConfigCommandError {
    #[error("home directory missing")]
    HomeDirectoryMissing,

    #[error("unknown config key: {key}; valid keys: hook-debug")]
    UnknownKey { key: String },

    #[error("invalid value for {key}: {value}; expected true or false")]
    InvalidValue { key: String, value: String },

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
    if key != HOOK_DEBUG_KEY {
        return Err(ConfigCommandError::UnknownKey {
            key: key.to_owned(),
        });
    }

    let hook_debug = parse_bool_value(key, value)?;
    let mut config = load_or_create(agira_root)?;
    config.hook_debug = hook_debug;
    save_global_config(agira_root, &config).map_err(ConfigCommandError::Save)?;
    print_config_output(&format!("{HOOK_DEBUG_KEY} = {hook_debug}\n"));

    Ok(())
}

fn parse_bool_value(key: &str, value: &str) -> Result<bool, ConfigCommandError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ConfigCommandError::InvalidValue {
            key: key.to_owned(),
            value: value.to_owned(),
        }),
    }
}

fn format_config_get(config: &crate::core::global_config::GlobalConfig) -> String {
    format!(
        "default-max-retries = {}\nhook-debug = {}\n",
        config.default_max_retries, config.hook_debug
    )
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
