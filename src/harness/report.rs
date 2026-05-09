use serde::{Deserialize, Serialize};

use crate::harness::{HarnessRunId, QualityGateResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pass,
    Fail,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTimeline {
    pub run_id: HarnessRunId,
    #[serde(default)]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub schema_version: String,
    pub run_id: HarnessRunId,
    pub status: ReplayStatus,
    pub primary_owner: Option<crate::harness::FailureOwner>,
    pub summary: String,
    #[serde(default)]
    pub gate_results: Vec<QualityGateResult>,
    pub restart_point: Option<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
}
