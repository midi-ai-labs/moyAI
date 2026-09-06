//! Desktop Hub catalog, explicit review, and per-turn route ownership.
//!
//! Direct remains the default. Hub permits and gateway targets live only in memory;
//! prompt and response traffic uses a separate local gateway process.

mod client;
mod connection;
mod settings;
pub use client::HubCatalogClient;
pub use connection::{
    HubConnection, HubConnectionProjection, HubConnectionStatus, HubReviewContext,
};
pub use connection::{HubRouteMode, HubTurnRoute};
pub use settings::{HubSettings, HubSettingsStore};

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_MODELS: usize = 128;
const MAX_SELECTED_MODELS: usize = 128;
const MAX_CAPABILITIES: usize = 32;

/// Decimal wire representation avoids loss of revision identity in JavaScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogRevision(u64);

impl CatalogRevision {
    pub fn new(value: u64) -> Result<Self, HubError> {
        if value == 0 {
            return Err(HubError::InvalidCatalog);
        }
        Ok(Self(value))
    }
}

impl Serialize for CatalogRevision {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for CatalogRevision {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let value = raw.parse::<u64>().map_err(serde::de::Error::custom)?;
        if value == 0 || value.to_string() != raw {
            return Err(serde::de::Error::custom(
                "expected positive canonical decimal revision",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubModel {
    pub id: String,
    pub label: String,
    pub capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogChange {
    pub revision: CatalogRevision,
    pub at_ms: u64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubCatalog {
    pub hub_id: String,
    pub software_version: String,
    pub revision: CatalogRevision,
    pub models: Vec<HubModel>,
    pub changes: Vec<CatalogChange>,
}

impl HubCatalog {
    pub fn validate(&self) -> Result<(), HubError> {
        if !valid_id(&self.hub_id)
            || !bounded_text(&self.software_version, 128)
            || self.models.len() > MAX_MODELS
            || self.changes.len() > 64
        {
            return Err(HubError::InvalidCatalog);
        }
        let mut ids = BTreeSet::new();
        for model in &self.models {
            if !valid_id(&model.id)
                || !bounded_text(&model.label, 256)
                || !ids.insert(&model.id)
                || model.capabilities.len() > MAX_CAPABILITIES
                || model
                    .capabilities
                    .iter()
                    .any(|capability| !valid_capability(capability))
            {
                return Err(HubError::InvalidCatalog);
            }
        }
        let mut previous = None;
        for change in &self.changes {
            if change.revision > self.revision
                || previous.is_some_and(|revision| revision >= change.revision)
                || !bounded_text(&change.summary, 2048)
            {
                return Err(HubError::InvalidCatalog);
            }
            previous = Some(change.revision);
        }
        Ok(())
    }

    /// UI review data; comparing catalogs never marks a revision reviewed.
    pub fn diff(&self, previous: &Self) -> Result<CatalogDiff, HubError> {
        self.validate()?;
        previous.validate()?;
        if self.hub_id != previous.hub_id {
            return Err(HubError::DifferentHub);
        }
        if self.revision < previous.revision {
            return Err(HubError::RevisionRollback);
        }
        if self.revision == previous.revision && self != previous {
            return Err(HubError::InvalidCatalog);
        }
        let mut diff = CatalogDiff {
            software_changed: self.software_version != previous.software_version,
            ..CatalogDiff::default()
        };
        for model in &self.models {
            match previous.models.iter().find(|old| old.id == model.id) {
                None => diff.added.push(model.id.clone()),
                Some(old) if old != model => diff.changed.push(model.id.clone()),
                Some(_) => {}
            }
        }
        diff.removed.extend(
            previous
                .models
                .iter()
                .filter(|old| !self.models.iter().any(|model| model.id == old.id))
                .map(|old| old.id.clone()),
        );
        Ok(diff)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub software_changed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubWaitPolicy {
    #[default]
    WaitForPreferred,
    AllowSelectedFallback,
}

/// Durable preferences contain logical identities only, never allocated endpoints or permits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubSelection {
    pub allowed_model_ids: BTreeSet<String>,
    pub preferred_model_id: String,
    pub required_capabilities: BTreeSet<String>,
    pub wait_policy: HubWaitPolicy,
    pub affinity_turns: u16,
}

impl HubSelection {
    pub fn validate_shape(&self) -> Result<(), HubError> {
        if self.allowed_model_ids.is_empty()
            || self.allowed_model_ids.len() > MAX_SELECTED_MODELS
            || self.allowed_model_ids.iter().any(|id| !valid_id(id))
            || !self.allowed_model_ids.contains(&self.preferred_model_id)
            || !(1..=100).contains(&self.affinity_turns)
            || self.required_capabilities.len() > MAX_CAPABILITIES
            || self
                .required_capabilities
                .iter()
                .any(|capability| !valid_capability(capability))
        {
            return Err(HubError::InvalidSelection);
        }
        Ok(())
    }

    pub fn validate(&self, catalog: &HubCatalog) -> Result<(), HubError> {
        catalog.validate()?;
        self.validate_shape()?;
        let mut compatible = false;
        for id in &self.allowed_model_ids {
            let model = catalog
                .models
                .iter()
                .find(|model| &model.id == id)
                .ok_or(HubError::ModelRemoved)?;
            if self.required_capabilities.is_subset(&model.capabilities) {
                compatible = true;
            }
        }
        if !compatible {
            return Err(HubError::CapabilityMismatch);
        }
        if self.wait_policy == HubWaitPolicy::WaitForPreferred
            && catalog
                .models
                .iter()
                .find(|model| model.id == self.preferred_model_id)
                .is_none_or(|model| !self.required_capabilities.is_subset(&model.capabilities))
        {
            return Err(HubError::CapabilityMismatch);
        }
        Ok(())
    }
}

/// Candidate produced by an explicit review action. The caller must atomically persist it with
/// the settings target before publishing it as effective configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedHubSelection {
    pub hub_id: String,
    pub reviewed_revision: CatalogRevision,
    pub selection: HubSelection,
}

impl ReviewedHubSelection {
    pub fn review(
        catalog: &HubCatalog,
        expected_hub_id: &str,
        expected_revision: CatalogRevision,
        selection: HubSelection,
    ) -> Result<Self, HubError> {
        if catalog.hub_id != expected_hub_id {
            return Err(HubError::DifferentHub);
        }
        if catalog.revision != expected_revision {
            return Err(HubError::ReviewRequired);
        }
        selection.validate(catalog)?;
        Ok(Self {
            hub_id: catalog.hub_id.clone(),
            reviewed_revision: catalog.revision,
            selection,
        })
    }

    /// Gate every Hub-managed turn/request before allocation. No provider fallback is returned.
    pub fn check_admission(&self, catalog: &HubCatalog) -> Result<(), HubError> {
        catalog.validate()?;
        if catalog.hub_id != self.hub_id {
            return Err(HubError::DifferentHub);
        }
        if catalog.revision < self.reviewed_revision {
            return Err(HubError::RevisionRollback);
        }
        if catalog.revision != self.reviewed_revision {
            return Err(HubError::ReviewRequired);
        }
        self.selection.validate(catalog)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelRoute {
    #[default]
    Direct,
    Hub {
        reviewed: ReviewedHubSelection,
    },
}

impl ModelRoute {
    pub fn check_admission(&self, catalog: Option<&HubCatalog>) -> Result<(), HubError> {
        match self {
            Self::Direct => Ok(()),
            Self::Hub { reviewed } => {
                reviewed.check_admission(catalog.ok_or(HubError::Unavailable)?)
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopHubRoutes {
    pub main: ModelRoute,
    pub side_chat: ModelRoute,
}

/// Safe public errors intentionally omit URL, response body and credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HubError {
    #[error("A Hub turn is already active for this context")]
    RouteBusy,
    #[error("Hub request routing is unavailable")]
    GatewayUnavailable,
    #[error("Hub connection changed; reload the current connection")]
    ConnectionChanged,
    #[error("Hub settings changed; reload before saving")]
    SettingsChanged,
    #[error("Hub settings are invalid; the existing file was preserved")]
    SettingsInvalid,
    #[error("Hub settings storage is unavailable")]
    SettingsUnavailable,
    #[error("Hub settings are being saved by another process")]
    SettingsBusy,
    #[error("Hub connection settings are invalid")]
    InvalidConnection,
    #[error("Hub authentication failed")]
    Unauthorized,
    #[error("Hub did not respond within the configured deadline")]
    Deadline,
    #[error("Hub is unavailable; the selected route has not been changed")]
    Unavailable,
    #[error("Hub redirects are not permitted")]
    Redirect,
    #[error("Hub catalog exceeded the response limit")]
    ResponseLimit,
    #[error("Hub catalog is invalid or unsupported")]
    InvalidCatalog,
    #[error("The connected Hub identity differs from the reviewed Hub")]
    DifferentHub,
    #[error("Hub catalog revision moved backwards")]
    RevisionRollback,
    #[error("Review and explicitly save the current Hub catalog before continuing")]
    ReviewRequired,
    #[error("Choose a nonempty model set, preferred model and valid affinity")]
    InvalidSelection,
    #[error("A selected model no longer exists; update the selection")]
    ModelRemoved,
    #[error("The selected models do not satisfy the required capabilities and wait policy")]
    CapabilityMismatch,
}

impl HubError {
    pub fn code(self) -> &'static str {
        match self {
            Self::RouteBusy => "route_busy",
            Self::GatewayUnavailable => "gateway_unavailable",
            Self::ConnectionChanged => "connection_changed",
            Self::SettingsChanged => "settings_changed",
            Self::SettingsInvalid => "settings_invalid",
            Self::SettingsUnavailable => "settings_unavailable",
            Self::SettingsBusy => "settings_busy",
            Self::InvalidConnection => "invalid_connection",
            Self::Unauthorized => "unauthorized",
            Self::Deadline => "deadline",
            Self::Unavailable => "unavailable",
            Self::Redirect => "redirect",
            Self::ResponseLimit => "response_limit",
            Self::InvalidCatalog => "invalid_catalog",
            Self::DifferentHub => "different_hub",
            Self::RevisionRollback => "revision_rollback",
            Self::ReviewRequired => "catalog_changed",
            Self::InvalidSelection => "invalid_selection",
            Self::ModelRemoved => "model_removed",
            Self::CapabilityMismatch => "capability_mismatch",
        }
    }
}

pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_capability(value: &str) -> bool {
    value.len() <= 64 && valid_id(value)
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

impl fmt::Display for CatalogRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests;
