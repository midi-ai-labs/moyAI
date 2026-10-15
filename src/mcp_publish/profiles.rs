use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use camino::{Utf8Component, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::PublishError;
use crate::config::AccessMode;
use crate::session::{ProjectId, SessionId};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolEffectClass, ToolEffectPolicy, ToolName, ToolSpec};

pub(crate) const SCHEMA_VERSION: u32 = 3;
const MAX_PROFILES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublishProfileId(pub Ulid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishTransport {
    StreamableHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublishMode {
    ReadTools {},
    Agent { access_mode: AccessMode },
}

impl Default for PublishMode {
    fn default() -> Self {
        Self::ReadTools {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishTls {
    pub certificate_path: Utf8PathBuf,
    pub private_key_path: Utf8PathBuf,
}

/// References an eventual local credential store. Tokens and secrets are never
/// accepted in the configuration document; a reference alone proves no identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublishAuthentication {
    Unpaired {},
    LocalCredential { credential_id: Ulid },
}

impl Default for PublishAuthentication {
    fn default() -> Self {
        Self::Unpaired {}
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishBackgroundPolicy {
    #[default]
    StopWhenWindowCloses,
    KeepWhileApplicationRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublishTarget {
    Project {
        project_id: ProjectId,
        workspace_root: Utf8PathBuf,
    },
    Temp {},
    /// Schema 1 compatibility. Retain its exact chat/folder authority until the
    /// user explicitly selects a project; migration must never widen a scope.
    LegacySession {
        project_id: ProjectId,
        root_session_id: SessionId,
        workspace_root: Utf8PathBuf,
    },
}

impl PublishTarget {
    pub fn workspace_root(&self) -> Option<&Utf8PathBuf> {
        match self {
            Self::Project { workspace_root, .. } | Self::LegacySession { workspace_root, .. } => {
                Some(workspace_root)
            }
            Self::Temp {} => None,
        }
    }

    pub fn supports_tool(&self, tool: ToolName) -> bool {
        supported_tool(tool) && (!matches!(self, Self::Temp {}) || tool == ToolName::CurrentTime)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishProfile {
    pub id: PublishProfileId,
    pub label: String,
    #[serde(default)]
    pub enabled: bool,
    pub bind: SocketAddr,
    pub transport: PublishTransport,
    #[serde(default)]
    pub tls: Option<PublishTls>,
    #[serde(default)]
    pub mode: PublishMode,
    #[serde(default)]
    pub authentication: PublishAuthentication,
    pub target: PublishTarget,
    pub tools: Vec<ToolName>,
    pub max_concurrent_calls: u16,
    #[serde(default)]
    pub background: PublishBackgroundPolicy,
}

impl PublishProfile {
    /// Create an unpaired, disabled profile. Registration never enables publishing.
    pub fn new(label: String, target: PublishTarget) -> Self {
        Self {
            id: PublishProfileId(Ulid::new()),
            label,
            enabled: false,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7332),
            transport: PublishTransport::StreamableHttp,
            tls: None,
            mode: PublishMode::default(),
            authentication: PublishAuthentication::Unpaired {},
            target,
            tools: Vec::new(),
            max_concurrent_calls: 1,
            background: PublishBackgroundPolicy::StopWhenWindowCloses,
        }
    }

    pub fn validate(&self) -> Result<(), PublishError> {
        if self.label.trim().is_empty()
            || self.label.chars().count() > 80
            || self.label.chars().any(char::is_control)
        {
            return Err(PublishError::InvalidConfiguration("invalid profile label"));
        }
        if self.bind.port() == 0
            || self.bind.ip().is_unspecified()
            || self.bind.ip().is_multicast()
            || (self.tls.is_none() && !self.bind.ip().is_loopback())
        {
            return Err(PublishError::InvalidConfiguration(
                "a nonzero port and explicit address are required; LAN serving requires TLS",
            ));
        }
        if let Some(tls) = &self.tls {
            for path in [&tls.certificate_path, &tls.private_key_path] {
                if !path.is_absolute()
                    || path
                        .components()
                        .any(|part| matches!(part, Utf8Component::ParentDir))
                {
                    return Err(PublishError::InvalidConfiguration(
                        "TLS certificate and key paths must be absolute without parent traversal",
                    ));
                }
            }
        }
        if self.target.workspace_root().is_some_and(|root| {
            !root.is_absolute()
                || root
                    .components()
                    .any(|part| matches!(part, Utf8Component::ParentDir))
        }) {
            return Err(PublishError::InvalidConfiguration(
                "workspace target must be an absolute path without parent traversal",
            ));
        }
        if !(1..=16).contains(&self.max_concurrent_calls) {
            return Err(PublishError::InvalidConfiguration(
                "concurrent call limit must be between 1 and 16",
            ));
        }
        if matches!(self.mode, PublishMode::Agent { .. })
            && (!self.tools.is_empty()
                || matches!(&self.target, PublishTarget::LegacySession { .. }))
        {
            return Err(PublishError::InvalidConfiguration(
                "agent publication requires an explicit project or temp target and no read-tool selection",
            ));
        }
        let mut tools = HashSet::new();
        for tool in &self.tools {
            if !self.target.supports_tool(*tool) || !tools.insert(*tool) {
                return Err(PublishError::ToolUnavailable);
            }
        }
        if self.enabled && !self.has_public_operations() {
            return Err(PublishError::InvalidConfiguration(
                "an enabled profile must select at least one tool",
            ));
        }
        if self.enabled && self.authentication == (PublishAuthentication::Unpaired {}) {
            return Err(PublishError::Unpaired);
        }
        Ok(())
    }

    pub fn has_public_operations(&self) -> bool {
        matches!(self.mode, PublishMode::Agent { .. }) || !self.tools.is_empty()
    }

    /// Configuration preview only; do not expose this as unauthenticated tools/list.
    /// Specs and effect classification come from the caller's actual registry.
    pub fn preview_tool_specs(
        &self,
        registry: &ToolRegistry,
    ) -> Result<Vec<ToolSpec>, PublishError> {
        self.validate()?;
        if !matches!(self.mode, PublishMode::ReadTools {}) {
            return Err(PublishError::ToolUnavailable);
        }
        let specs = registry.specs();
        self.tools
            .iter()
            .map(|name| {
                specs
                    .iter()
                    .find(|spec| {
                        spec.name == *name
                            && spec.effect == ToolEffectPolicy::Static(ToolEffectClass::Read)
                    })
                    .cloned()
                    .ok_or(PublishError::ToolUnavailable)
            })
            .collect()
    }
}

fn supported_tool(tool: ToolName) -> bool {
    // Read effect also includes session/agent bookkeeping. That classification
    // alone must not expose goals, plans, agent control, or downstream MCP routes.
    matches!(
        tool,
        ToolName::List
            | ToolName::Glob
            | ToolName::Grep
            | ToolName::Read
            | ToolName::InspectDirectory
            | ToolName::CurrentTime
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishProfileSet {
    pub schema_version: u32,
    pub revision: u64,
    pub profiles: Vec<PublishProfile>,
}

impl Default for PublishProfileSet {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            profiles: Vec::new(),
        }
    }
}

impl PublishProfileSet {
    pub fn validate(&self) -> Result<(), PublishError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(PublishError::InvalidConfiguration(
                "unsupported publish configuration version",
            ));
        }
        if self.profiles.len() > MAX_PROFILES {
            return Err(PublishError::InvalidConfiguration(
                "at most 32 publish profiles are supported",
            ));
        }
        let mut ids = HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(profile.id) {
                return Err(PublishError::InvalidConfiguration("duplicate profile ID"));
            }
        }
        Ok(())
    }
}
