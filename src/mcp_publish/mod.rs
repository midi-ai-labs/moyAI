//! Explicit, authenticated Desktop MCP publishing, independent of outbound MCP clients.

mod credentials;
pub mod dispatch;
mod profiles;
mod service;
mod store;
pub mod tls;
pub mod transport;

pub use profiles::{
    PublishAuthentication, PublishBackgroundPolicy, PublishMode, PublishProfile, PublishProfileId,
    PublishProfileSet, PublishTarget, PublishTls, PublishTransport,
};
pub use service::{PublishDraft, PublishProjection, PublishService, PublishTokenReceipt};
pub use store::PublishProfileStore;

use crate::tool::registry::ToolRegistry;

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("MCP publish configuration is invalid: {0}")]
    InvalidConfiguration(&'static str),
    #[error("MCP publish profile does not exist")]
    UnknownProfile,
    #[error("MCP publish profile is disabled")]
    Disabled,
    #[error("MCP publish profile has not been paired")]
    Unpaired,
    #[error("MCP publish target does not match the configured publication scope")]
    TargetMismatch,
    #[error("MCP publish tool is unavailable or outside the supported selection")]
    ToolUnavailable,
    #[error("MCP publish configuration changed; reload before saving")]
    StaleRevision,
    #[error("MCP publish configuration is being saved; retry after reloading")]
    StoreBusy,
    #[error("MCP publish configuration cannot be decoded")]
    InvalidDocument,
    #[error("MCP publish configuration cannot be read or saved")]
    Storage(#[source] std::io::Error),
}

/// Configuration preflight. A successful preview is never
/// an authenticated principal, a workspace grant, or proof that a server is running.
/// No network access or tool side effects occur here, including for enabled profiles.
pub fn validate_profile_start(
    profiles: &PublishProfileSet,
    profile_id: PublishProfileId,
    registry: &ToolRegistry,
    available_target: &PublishTarget,
) -> Result<(), PublishError> {
    profiles.validate()?;
    let profile = profiles
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or(PublishError::UnknownProfile)?;
    if !profile.enabled {
        return Err(PublishError::Disabled);
    }
    if profile.authentication == (PublishAuthentication::Unpaired {}) {
        return Err(PublishError::Unpaired);
    }
    if &profile.target != available_target {
        return Err(PublishError::TargetMismatch);
    }
    profile.preview_tool_specs(registry)?;
    Ok(())
}

#[cfg(test)]
mod tests;
