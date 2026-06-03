use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub stack: String,
    pub state_machine: Vec<String>,
    pub models: BTreeMap<String, String>,
    pub verification: VerificationConfig,
    pub acceptance_testing: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prd_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VerificationConfig {
    pub commands: Vec<String>,
}
