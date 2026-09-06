//! Hub-managed device identities and explicit peer selection.
//!
//! Shared configuration carries public trust only. Private device credentials,
//! runtime grants and receiver authority are never model configuration.

mod client;
mod identity;
mod outgoing;
mod receiver;
mod service;
mod settings;
pub(crate) use client::{DeviceClient, DeviceGrant, DirectoryPeer};
pub use client::{GrantClaims, VerifiedGrant};
pub(crate) use identity::{DeviceIdentity, DeviceIdentityStore};
pub use outgoing::{DeviceDelegationRow, DeviceNetworkJobs};
pub(crate) use service::WeakDeviceNetwork;
pub use service::{DeviceNetworkProjection, DeviceNetworkService};
pub use settings::{DeviceSettings, DeviceSettingsStore, ReceiverSettings, SelectedPeer};

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod config_tests;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedHubConfig {
    pub hub_url: String,
    pub ca_certificate_pem: String,
}

impl SharedHubConfig {
    pub fn configured(&self) -> bool {
        !self.hub_url.is_empty() || !self.ca_certificate_pem.is_empty()
    }

    pub fn validate(&self) -> Result<reqwest::Url, DeviceError> {
        if self.hub_url.len() > 2048 || self.ca_certificate_pem.len() > 65536 {
            return Err(DeviceError::InvalidConfiguration);
        }
        let url =
            reqwest::Url::parse(&self.hub_url).map_err(|_| DeviceError::InvalidConfiguration)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(DeviceError::InvalidConfiguration);
        }
        crate::mcp_publish::tls::public_certificate(&self.ca_certificate_pem)
            .map_err(|_| DeviceError::InvalidConfiguration)?;
        Ok(url)
    }

    /// Import only public network trust; another machine's model, MCP secrets,
    /// workspace paths and receiver choices must not be adopted with this file.
    pub fn import(text: &str) -> Result<Self, DeviceError> {
        if text.len() > 256 * 1024 {
            return Err(DeviceError::InvalidConfiguration);
        }
        #[derive(Deserialize)]
        struct Document {
            device_network: SharedHubConfig,
        }
        let document: Document =
            toml::from_str(text).map_err(|_| DeviceError::InvalidConfiguration)?;
        document.device_network.validate()?;
        Ok(document.device_network)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("invalid_configuration")]
    InvalidConfiguration,
    #[error("invalid_identity")]
    InvalidIdentity,
    #[error("settings_corrupt")]
    SettingsCorrupt,
    #[error("settings_changed")]
    SettingsChanged,
    #[error("connection_changed")]
    ConnectionChanged,
    #[error("store_busy")]
    StoreBusy,
    #[error("storage_error")]
    Storage,
    #[error("unavailable")]
    Unavailable,
    #[error("enrollment_denied")]
    EnrollmentDenied,
    #[error("device_revoked")]
    Revoked,
    #[error("policy_denied")]
    PolicyDenied,
    #[error("grant_denied")]
    GrantDenied,
    #[error("invalid_response")]
    InvalidResponse,
    #[error("receiver_busy")]
    ReceiverBusy,
    #[error("confirmation_required")]
    ConfirmationRequired,
}

pub(crate) fn canonical_revision(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|number| number.to_string() == value)
}

pub(crate) fn stable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte))
}
