use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::harness::ContractId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    Runtime,
    StateMachine,
    ToolSchema,
    RequestedWork,
    Scenario,
    QualityGate,
    ArtifactManifest,
    ReplayReport,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContractRef {
    pub id: ContractId,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractRecord {
    pub id: ContractId,
    pub kind: ContractKind,
    pub version: String,
    pub source_path: Utf8PathBuf,
    pub content_sha256: String,
    pub schema_ref: Option<String>,
    pub model_visible_summary: Option<String>,
}

impl ContractRecord {
    pub fn as_ref(&self) -> ContractRef {
        ContractRef {
            id: self.id.clone(),
            version: self.version.clone(),
        }
    }
}
