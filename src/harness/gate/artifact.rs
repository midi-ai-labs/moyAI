use camino::Utf8Path;

use crate::harness::ArtifactManifest;
use crate::harness::artifact::hash_file;
use crate::harness::gate::{
    FailureOwner, GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult,
};

pub fn evaluate(root: &Utf8Path, artifacts: &[ArtifactManifest]) -> GateEvaluation {
    if artifacts.is_empty() {
        return GateEvaluation {
            result: QualityGateResult::blocked(
                GateKind::Artifact,
                FailureOwner::HarnessCapture,
                "artifact manifest is empty",
            ),
            derived: GateDerivedOutput::default(),
        };
    }
    let canonical_root = match std::fs::canonicalize(root.as_std_path()) {
        Ok(path) => path,
        Err(error) => {
            return GateEvaluation {
                result: QualityGateResult::blocked(
                    GateKind::Artifact,
                    FailureOwner::HarnessCapture,
                    format!("artifact root cannot be resolved: {root}: {error}"),
                ),
                derived: GateDerivedOutput::default(),
            };
        }
    };
    for artifact in artifacts {
        if artifact.relative_path.is_absolute()
            || artifact
                .relative_path
                .components()
                .any(|component| component.as_str() == "..")
        {
            return GateEvaluation {
                result: QualityGateResult::fail(
                    GateKind::Artifact,
                    FailureOwner::HarnessCapture,
                    format!(
                        "artifact path escapes the replay root: {}",
                        artifact.relative_path
                    ),
                ),
                derived: GateDerivedOutput::default(),
            };
        }
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
        let canonical_path = match std::fs::canonicalize(path.as_std_path()) {
            Ok(path) => path,
            Err(error) => {
                return GateEvaluation {
                    result: QualityGateResult::blocked(
                        GateKind::Artifact,
                        FailureOwner::HarnessCapture,
                        format!(
                            "artifact cannot be resolved: {}: {error}",
                            artifact.relative_path
                        ),
                    ),
                    derived: GateDerivedOutput::default(),
                };
            }
        };
        if !canonical_path.starts_with(&canonical_root) {
            return GateEvaluation {
                result: QualityGateResult::fail(
                    GateKind::Artifact,
                    FailureOwner::HarnessCapture,
                    format!(
                        "artifact resolves outside the replay root: {}",
                        artifact.relative_path
                    ),
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
