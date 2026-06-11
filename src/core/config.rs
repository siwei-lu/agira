use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::global_config::GlobalConfig;

pub const INITIAL_PHASE_NAME: &str = "pending";
pub const TERMINAL_PHASE_NAME: &str = "done";
pub const DEFAULT_WORKFLOW_NAME: &str = "default";

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PhaseDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub stack: String,
    pub phases: BTreeMap<String, PhaseDef>,
    pub workflows: BTreeMap<String, Vec<String>>,
    pub default_workflow: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Config {
    pub fn sequence(&self, workflow: &str) -> &[String] {
        self.workflows
            .get(workflow)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn phase_def(&self, name: &str) -> Option<&PhaseDef> {
        self.phases.get(name)
    }

    pub fn initial_phase(&self) -> Option<&str> {
        self.sequence(&self.default_workflow)
            .first()
            .map(String::as_str)
    }

    pub fn terminal_phase(&self) -> Option<&str> {
        self.sequence(&self.default_workflow)
            .last()
            .map(String::as_str)
    }

    pub fn new_single_workflow(
        stack: impl Into<String>,
        phase_entries: Vec<(String, PhaseDef)>,
        max_retries: u32,
    ) -> Self {
        let (phases, sequence) = normalize_palette_and_sequence(phase_entries, Vec::new());
        let mut workflows = BTreeMap::new();
        workflows.insert(DEFAULT_WORKFLOW_NAME.to_owned(), sequence);

        Config {
            stack: stack.into(),
            phases,
            workflows,
            default_workflow: DEFAULT_WORKFLOW_NAME.to_owned(),
            max_retries,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.workflows.contains_key(&self.default_workflow) {
            return Err(format!(
                "default_workflow '{}' not found in workflows",
                self.default_workflow
            ));
        }

        for (workflow_name, sequence) in &self.workflows {
            let mut seen = BTreeSet::new();
            for phase_name in sequence {
                if !self.phases.contains_key(phase_name) {
                    return Err(format!(
                        "workflow '{workflow_name}' references unknown phase '{phase_name}'"
                    ));
                }
                if !seen.insert(phase_name) {
                    return Err(format!(
                        "workflow '{workflow_name}' references duplicate phase '{phase_name}'"
                    ));
                }
            }

            match sequence.last() {
                Some(last) if last == TERMINAL_PHASE_NAME => {}
                Some(last) => {
                    return Err(format!(
                        "workflow '{workflow_name}' terminal phase must be '{TERMINAL_PHASE_NAME}' (found '{last}')"
                    ));
                }
                None => {
                    return Err(format!("workflow '{workflow_name}' must not be empty"));
                }
            }
        }

        Ok(())
    }
}

fn default_max_retries() -> u32 {
    3
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

    let mut config =
        serde_json::from_str::<Config>(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;

    config.max_retries = config.max_retries.max(1);
    normalize_config(&mut config);
    config.validate().map_err(|reason| ConfigError::Invalid {
        path: path.to_path_buf(),
        reason,
    })?;

    if config.max_retries == 1 && !contents.contains("\"max_retries\"") {
        config.max_retries = global_config.default_max_retries;
    }

    Ok(config)
}

pub fn normalize_config(config: &mut Config) {
    normalize_mandatory_phase_def(INITIAL_PHASE_NAME, &mut config.phases);
    normalize_mandatory_phase_def(TERMINAL_PHASE_NAME, &mut config.phases);

    for sequence in config.workflows.values_mut() {
        *sequence = normalize_sequence(std::mem::take(sequence));
    }
}

pub fn normalize_sequence(sequence: Vec<String>) -> Vec<String> {
    let mut middle = Vec::new();
    for phase in sequence {
        if phase != INITIAL_PHASE_NAME && phase != TERMINAL_PHASE_NAME {
            middle.push(phase);
        }
    }

    let mut normalized = Vec::with_capacity(middle.len() + 2);
    normalized.push(INITIAL_PHASE_NAME.to_owned());
    normalized.extend(middle);
    normalized.push(TERMINAL_PHASE_NAME.to_owned());
    normalized
}

pub fn normalize_palette_and_sequence(
    entries: Vec<(String, PhaseDef)>,
    sequence_hint: Vec<String>,
) -> (BTreeMap<String, PhaseDef>, Vec<String>) {
    let mut phases = BTreeMap::new();
    let mut sequence = Vec::new();

    for (name, def) in entries {
        sequence.push(name.clone());
        phases.insert(name, def);
    }

    if !sequence_hint.is_empty() {
        sequence = sequence_hint;
    }

    normalize_mandatory_phase_def(INITIAL_PHASE_NAME, &mut phases);
    normalize_mandatory_phase_def(TERMINAL_PHASE_NAME, &mut phases);
    (phases, normalize_sequence(sequence))
}

fn normalize_mandatory_phase_def(name: &str, phases: &mut BTreeMap<String, PhaseDef>) {
    phases.insert(
        name.to_owned(),
        PhaseDef {
            model: None,
            duty: None,
            gate: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // PhaseDef gate serialization round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn phase_def_with_gate_serializes_and_round_trips() {
        let def = PhaseDef {
            model: None,
            duty: None,
            gate: Some("cargo test".to_owned()),
        };
        let json = serde_json::to_string(&def).expect("serialize");
        assert!(
            json.contains("\"gate\": \"cargo test\"") || json.contains("\"gate\":\"cargo test\"")
        );
        let round_tripped: PhaseDef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_tripped, def);
    }

    #[test]
    fn phase_def_without_gate_serializes_without_gate_key() {
        let def = PhaseDef {
            model: None,
            duty: None,
            gate: None,
        };
        let json = serde_json::to_string(&def).expect("serialize");
        assert!(!json.contains("gate"), "gate key must not appear when None");
        let round_tripped: PhaseDef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_tripped.gate, None);
    }

    #[test]
    fn phase_def_missing_gate_in_json_deserializes_to_none() {
        // Simulates an existing config file that has no gate field
        let json = r#"{"model": "gpt-4", "duty": "do stuff"}"#;
        let def: PhaseDef = serde_json::from_str(json).expect("deserialize");
        assert_eq!(def.gate, None);
    }

    #[test]
    fn normalize_mandatory_phase_def_sets_gate_to_none_on_pending_and_done() {
        let mut phases = BTreeMap::new();
        // Insert pending and done with a gate set (should be overwritten by normalize)
        phases.insert(
            INITIAL_PHASE_NAME.to_owned(),
            PhaseDef {
                model: Some("some-model".to_owned()),
                duty: None,
                gate: Some("should be cleared".to_owned()),
            },
        );
        phases.insert(
            TERMINAL_PHASE_NAME.to_owned(),
            PhaseDef {
                model: Some("some-model".to_owned()),
                duty: None,
                gate: Some("should be cleared".to_owned()),
            },
        );

        normalize_mandatory_phase_def(INITIAL_PHASE_NAME, &mut phases);
        normalize_mandatory_phase_def(TERMINAL_PHASE_NAME, &mut phases);

        assert_eq!(phases[INITIAL_PHASE_NAME].gate, None);
        assert_eq!(phases[TERMINAL_PHASE_NAME].gate, None);
    }
}

pub fn write_project_config(path: &Path, config: &Config) -> Result<(), ConfigError> {
    config.validate().map_err(|reason| ConfigError::Invalid {
        path: path.to_path_buf(),
        reason,
    })?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let temporary_path = path.with_extension("json.tmp");
    fs::write(&temporary_path, bytes).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    fs::rename(&temporary_path, path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}
