use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub stack: String,
    pub state_machine: Vec<String>,
    pub models: BTreeMap<String, String>,
    pub verification: VerificationConfig,
    pub acceptance_testing: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prd_path: Option<String>,
}

fn default_max_retries() -> u32 {
    3
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VerificationConfig {
    pub commands: Vec<String>,
}
