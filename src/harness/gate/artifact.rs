use camino::Utf8Path;

use crate::harness::ArtifactManifest;
use crate::harness::artifact::hash_file;
use crate::harness::gate::{
    FailureOwner, GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult,
};

pub fn evaluate(root: &Utf8Path, artifacts: &[ArtifactManifest]) -> GateEvaluation {
    for artifact in artifacts {
        let path = root.join(&artifact.relative_path);
        if !path.exists() {
            return GateEvaluation {
                result: QualityGateResult::blocked(
                    GateKind::Artifact,
                    FailureOwner::HarnessCapture,
                    format!("artifact body is missing: {}", artifact.relative_path),
                ),
                derived: GateDerivedOutput::default(),
            };
        }
        match hash_file(&path) {
            Ok((hash, size)) if hash == artifact.sha256 && size == artifact.size_bytes => {}
            Ok(_) => {
                return GateEvaluation {
                    result: QualityGateResult::fail(
                        GateKind::Artifact,
                        FailureOwner::HarnessCapture,
                        format!("artifact hash mismatch: {}", artifact.relative_path),
                    ),
                    derived: GateDerivedOutput::default(),
                };
            }
            Err(error) => {
                return GateEvaluation {
                    result: QualityGateResult::blocked(
                        GateKind::Artifact,
                        FailureOwner::HarnessCapture,
                        format!(
                            "artifact cannot be read: {}: {error}",
                            artifact.relative_path
                        ),
                    ),
                    derived: GateDerivedOutput::default(),
                };
            }
        }
    }
    GateEvaluation {
        result: QualityGateResult::pass(GateKind::Artifact, "artifact manifests resolve and hash"),
        derived: GateDerivedOutput::default(),
    }
}
