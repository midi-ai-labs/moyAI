use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::harness::{ArtifactId, ContractRef, HarnessEventId, HarnessRunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    WorkspaceFile,
    VerificationLog,
    RequestDiagnostics,
    Transcript,
    ImageAttachment,
    DoclingOutput,
    StateSnapshot,
    ReplayReport,
    GateResults,
    ContractSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactTag {
    Verification,
    ImageTransport,
    ScenarioOutput,
    StateSnapshot,
    Replay,
    Diagnostics,
    Contract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub id: ArtifactId,
    pub run_id: HarnessRunId,
    pub kind: ArtifactKind,
    pub relative_path: Utf8PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub tags: BTreeSet<ArtifactTag>,
    pub created_by_event: Option<HarnessEventId>,
    #[serde(default)]
    pub contract_refs: Vec<ContractRef>,
}

pub fn hash_file(path: &Utf8Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path.as_std_path())?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
