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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn capture_output<F>(run: F) -> (Result<(), ConfigCommandError>, String)
    where
        F: FnOnce() -> Result<(), ConfigCommandError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });

        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());

        (result, output)
    }

    #[test]
    fn get_prints_all_key_value_settings() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 9\nhook_debug = true\n",
        )
        .unwrap();

        let (result, output) = capture_output(|| run_config_get(agira_root.path()));

        result.unwrap();
        assert_eq!(output, "default-max-retries = 9\nhook-debug = true\n");
    }

    #[test]
    fn set_hook_debug_valid_value_saves_config() {
        let agira_root = TempDir::new().unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            r#"default_max_retries = 4

[[hooks]]
on = "done"
run = "echo done"
"#,
        )
        .unwrap();

        let (result, output) =
            capture_output(|| run_config_set(agira_root.path(), "hook-debug", "true"));

        result.unwrap();
        assert_eq!(output, "hook-debug = true\n");

        let contents = fs::read_to_string(agira_root.path().join("config.toml")).unwrap();
        assert!(contents.contains("default_max_retries = 4"));
        assert!(contents.contains("hook_debug = true"));
        assert!(contents.contains("[[hooks]]"));
        assert!(contents.contains("run = \"echo done\""));
    }

    #[test]
    fn set_rejects_invalid_key() {
        let agira_root = TempDir::new().unwrap();

        let error = run_config_set(agira_root.path(), "default-max-retries", "5").unwrap_err();

        assert!(matches!(error, ConfigCommandError::UnknownKey { .. }));
        assert!(!agira_root.path().join("config.toml").exists());
    }

    #[test]
    fn set_rejects_invalid_value() {
        let agira_root = TempDir::new().unwrap();

        let error = run_config_set(agira_root.path(), "hook-debug", "yes").unwrap_err();

        assert!(matches!(error, ConfigCommandError::InvalidValue { .. }));
        assert!(!agira_root.path().join("config.toml").exists());
    }
}
