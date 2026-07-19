use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ProviderApiMode, ProviderMetadataMode, ResolvedConfig};

/// Immutable provider timing policy captured together with a turn admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDeadlines {
    /// One budget shared by connection attempts, retry delays, and response headers.
    pub response_start_timeout_ms: u64,
    /// Rolling timeout after a streaming response has started.
    pub stream_idle_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_connect_retries: u8,
}

/// Product-owned upper bounds for one fully prepared provider request.
///
/// These limits are captured with the turn so live settings cannot change the request contract
/// after admission. Text-token admission remains owned by `ContextManager`; this envelope covers
/// the JSON, schema, stop-sequence, and image surfaces that token accounting cannot safely bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRequestLimits {
    pub max_serialized_body_bytes: u64,
    pub max_messages: usize,
    pub max_tools: usize,
    pub max_tool_schema_bytes: u64,
    pub max_extra_body_bytes: u64,
    pub max_stop_sequences: usize,
    pub max_stop_sequence_bytes: u64,
    pub max_images: usize,
    pub max_single_image_decoded_bytes: u64,
    pub max_total_image_decoded_bytes: u64,
    pub max_total_image_base64_chars: u64,
    pub max_image_width: u32,
    pub max_image_height: u32,
    pub max_image_pixels: u64,
}

impl ProviderRequestLimits {
    pub const ALLOWED_IMAGE_MIME_TYPES: [&'static str; 4] =
        ["image/png", "image/jpeg", "image/gif", "image/webp"];

    pub const fn product_default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            // Eight images are admitted separately below. This body ceiling is intentionally
            // larger than the aggregate 40 MiB decoded-image budget after base64 expansion.
            max_serialized_body_bytes: 64 * MIB,
            max_messages: 512,
            max_tools: 128,
            max_tool_schema_bytes: 2 * MIB,
            max_extra_body_bytes: 256 * 1024,
            max_stop_sequences: 64,
            max_stop_sequence_bytes: 16 * 1024,
            max_images: 8,
            max_single_image_decoded_bytes: 20 * MIB,
            max_total_image_decoded_bytes: 40 * MIB,
            // 4 * ceil((40 MiB) / 3), excluding any data-URL prefix.
            max_total_image_base64_chars: 55_924_056,
            max_image_width: 16_384,
            max_image_height: 16_384,
            max_image_pixels: 100_000_000,
        }
    }

    pub fn allows_image_mime_type(self, mime_type: &str) -> bool {
        Self::ALLOWED_IMAGE_MIME_TYPES.contains(&mime_type)
    }
}

impl Default for ProviderRequestLimits {
    fn default() -> Self {
        Self::product_default()
    }
}

/// Aggregate bounds for a provider stream after response headers are received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderStreamLimits {
    pub max_raw_bytes: u64,
    pub max_events: u64,
    pub max_tool_calls: u64,
    pub max_tool_call_argument_bytes: u64,
    pub max_duration_ms: u64,
}

impl ProviderStreamLimits {
    pub const fn product_default() -> Self {
        Self {
            max_raw_bytes: 16 * 1024 * 1024,
            max_events: 100_000,
            max_tool_calls: 256,
            max_tool_call_argument_bytes: 1024 * 1024,
            max_duration_ms: 30 * 60 * 1_000,
        }
    }
}

impl Default for ProviderStreamLimits {
    fn default() -> Self {
        Self::product_default()
    }
}

/// A safe validation failure that never echoes the rejected endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProviderEndpointError {
    #[error("provider endpoint must not be empty")]
    Empty,
    #[error("provider endpoint must be a valid absolute URL")]
    InvalidAbsoluteUrl,
    #[error("provider endpoint must use http or https")]
    UnsupportedScheme,
    #[error("provider endpoint must include a host")]
    MissingHost,
    #[error(
        "provider endpoint must not contain URL userinfo; configure credentials through api_key_env or extra_headers"
    )]
    UserInfoNotAllowed,
    #[error("provider endpoint must not contain a query string")]
    QueryNotAllowed,
    #[error("provider endpoint must not contain a fragment")]
    FragmentNotAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolvedTurnConfigError {
    #[error(transparent)]
    ProviderEndpoint(#[from] ProviderEndpointError),
    #[error("{message}")]
    ProviderRuntime { message: String },
    #[error("{message}")]
    WorkspaceBoundary { message: String },
}

/// The only parsed owner of an OpenAI-compatible provider endpoint.
///
/// It accepts an HTTP(S) origin or base path (including LM Studio's optional
/// `/v1` suffix), canonicalizes trailing slashes, and cannot contain URL-borne
/// credentials, query data, or fragments.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProviderEndpoint {
    url: reqwest::Url,
    canonical: Arc<str>,
}

impl ProviderEndpoint {
    pub fn parse(raw: &str) -> Result<Self, ProviderEndpointError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ProviderEndpointError::Empty);
        }
        let mut url =
            reqwest::Url::parse(trimmed).map_err(|_| ProviderEndpointError::InvalidAbsoluteUrl)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ProviderEndpointError::UnsupportedScheme);
        }
        if url.host_str().is_none() {
            return Err(ProviderEndpointError::MissingHost);
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || raw_authority_contains_userinfo(trimmed)
        {
            return Err(ProviderEndpointError::UserInfoNotAllowed);
        }
        if url.query().is_some() {
            return Err(ProviderEndpointError::QueryNotAllowed);
        }
        if url.fragment().is_some() {
            return Err(ProviderEndpointError::FragmentNotAllowed);
        }

        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(if path.is_empty() { "/" } else { &path });
        Ok(Self::from_normalized_url(url))
    }

    fn from_normalized_url(url: reqwest::Url) -> Self {
        let serialized = url.to_string();
        let canonical = serialized.trim_end_matches('/').to_string();
        Self {
            url,
            canonical: Arc::from(canonical),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// Provider metadata endpoints live at the provider root even when a user
    /// enters the generation API base with a terminal `/v1` segment.
    pub fn catalog_root(&self) -> Self {
        let mut url = self.url.clone();
        let path = url.path().trim_end_matches('/');
        let root_path = path.strip_suffix("/v1").unwrap_or(path).to_string();
        url.set_path(if root_path.is_empty() {
            "/"
        } else {
            &root_path
        });
        Self::from_normalized_url(url)
    }

    pub(crate) fn join_api_path(
        &self,
        endpoint_path: &str,
    ) -> Result<reqwest::Url, ProviderEndpointError> {
        let mut url = self.url.clone();
        let base_path = url.path().trim_end_matches('/');
        let endpoint_path = endpoint_path.trim_start_matches('/');
        let base_owns_v1 = base_path == "/v1" || base_path.ends_with("/v1");
        let endpoint_suffix = if base_owns_v1 {
            endpoint_path.strip_prefix("v1/").unwrap_or(endpoint_path)
        } else {
            endpoint_path
        };
        let joined_path = if base_path.is_empty() {
            format!("/{endpoint_suffix}")
        } else {
            format!("{base_path}/{endpoint_suffix}")
        };
        url.set_path(&joined_path);
        Ok(url)
    }
}

impl fmt::Debug for ProviderEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProviderEndpoint")
            .field(&self.canonical)
            .finish()
    }
}

impl fmt::Display for ProviderEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn raw_authority_contains_userinfo(raw: &str) -> bool {
    raw.split_once("://")
        .map(|(_, remainder)| {
            remainder
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
                .contains('@')
        })
        .unwrap_or(false)
}

/// The single provider endpoint/model contract used by one admitted turn.
///
/// The endpoint is parsed before construction and therefore cannot carry URL
/// credentials, query data, or fragments. Transport receives the typed value;
/// logs and UI projections receive its canonical string.
#[derive(Clone)]
pub struct ProviderTarget {
    endpoint: ProviderEndpoint,
    model: Arc<str>,
    metadata_mode: ProviderMetadataMode,
    api_mode: ProviderApiMode,
    deadlines: ProviderDeadlines,
    request_limits: ProviderRequestLimits,
    stream_limits: ProviderStreamLimits,
}

impl ProviderTarget {
    pub fn new(
        endpoint: &str,
        model: &str,
        metadata_mode: ProviderMetadataMode,
        api_mode: ProviderApiMode,
        deadlines: ProviderDeadlines,
    ) -> Result<Self, ResolvedTurnConfigError> {
        let model = model.trim();
        if model.is_empty() {
            return Err(ResolvedTurnConfigError::ProviderRuntime {
                message: "config field `model.model` must not be empty".to_string(),
            });
        }
        if deadlines.response_start_timeout_ms == 0 {
            return Err(ResolvedTurnConfigError::ProviderRuntime {
                message: "config field `model.request_timeout_ms` must be greater than zero"
                    .to_string(),
            });
        }
        Ok(Self {
            endpoint: ProviderEndpoint::parse(endpoint)?,
            model: Arc::from(model),
            metadata_mode,
            api_mode,
            deadlines,
            request_limits: ProviderRequestLimits::product_default(),
            stream_limits: ProviderStreamLimits::product_default(),
        })
    }

    pub fn from_resolved_config(config: &ResolvedConfig) -> Result<Self, ResolvedTurnConfigError> {
        Self::new(
            &config.model.base_url,
            &config.model.model,
            config.model.provider_metadata_mode,
            config.model.provider_api_mode,
            ProviderDeadlines {
                response_start_timeout_ms: config.model.request_timeout_ms,
                stream_idle_timeout_ms: config.model.stream_idle_timeout_ms,
                connect_timeout_ms: config.model.connect_timeout_ms,
                max_connect_retries: config.model.max_retries,
            },
        )
    }

    pub(crate) fn endpoint(&self) -> &ProviderEndpoint {
        &self.endpoint
    }

    pub fn sanitized_endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn metadata_mode(&self) -> ProviderMetadataMode {
        self.metadata_mode
    }

    pub fn api_mode(&self) -> ProviderApiMode {
        self.api_mode
    }

    pub fn deadlines(&self) -> ProviderDeadlines {
        self.deadlines
    }

    pub fn request_limits(&self) -> ProviderRequestLimits {
        self.request_limits
    }

    pub fn stream_limits(&self) -> ProviderStreamLimits {
        self.stream_limits
    }

    #[cfg(test)]
    pub(crate) fn replace_request_limits(&mut self, request_limits: ProviderRequestLimits) {
        self.request_limits = request_limits;
    }

    #[cfg(test)]
    pub(crate) fn replace_stream_limits(&mut self, stream_limits: ProviderStreamLimits) {
        self.stream_limits = stream_limits;
    }
}

impl fmt::Debug for ProviderTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTarget")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("metadata_mode", &self.metadata_mode)
            .field("api_mode", &self.api_mode)
            .field("deadlines", &self.deadlines)
            .field("request_limits", &self.request_limits)
            .field("stream_limits", &self.stream_limits)
            .finish()
    }
}

/// Complete, immutable effective configuration for one turn.
///
/// Partial configuration is an input-boundary concern. Once captured, runtime
/// code receives this value directly and never reconstructs an effective turn
/// by applying a `PartialResolvedConfig` to another base.
#[derive(Clone)]
pub struct ResolvedTurnConfig {
    effective: Arc<ResolvedConfig>,
    provider: ProviderTarget,
}

impl ResolvedTurnConfig {
    pub fn capture(mut effective: ResolvedConfig) -> Result<Self, ResolvedTurnConfigError> {
        effective
            .normalize_and_validate_provider_runtime()
            .map_err(|message| ResolvedTurnConfigError::ProviderRuntime { message })?;
        effective
            .validate_workspace_boundary_roots()
            .map_err(|message| ResolvedTurnConfigError::WorkspaceBoundary { message })?;
        let provider = ProviderTarget::from_resolved_config(&effective)?;
        effective.model.base_url = provider.sanitized_endpoint().to_string();
        effective.model.model = provider.model().to_string();
        Ok(Self {
            effective: Arc::new(effective),
            provider,
        })
    }

    pub fn from_effective(effective: &ResolvedConfig) -> Result<Self, ResolvedTurnConfigError> {
        Self::capture(effective.clone())
    }

    pub fn runtime_config(&self) -> &ResolvedConfig {
        &self.effective
    }

    pub fn provider(&self) -> &ProviderTarget {
        &self.provider
    }

    pub fn with_model_override(&self, model: &str) -> Result<Self, ResolvedTurnConfigError> {
        let mut effective = self.runtime_config().clone();
        effective.model.model = model.to_string();
        Self::capture(effective)
    }
}

impl fmt::Debug for ResolvedTurnConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedTurnConfig")
            .field("provider", &self.provider)
            .field("access_mode", &self.effective.permissions.access_mode)
            .field("multi_agent", &self.effective.multi_agent)
            .finish_non_exhaustive()
    }
}

/// Projects only validated endpoints; rejected input becomes a fixed marker so
/// diagnostics can never echo URL-borne credentials or query data.
pub fn sanitize_provider_endpoint(raw: &str) -> String {
    ProviderEndpoint::parse(raw)
        .map(|endpoint| endpoint.as_str().to_string())
        .unwrap_or_else(|_| "<invalid-provider-endpoint>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lm_studio_root_and_v1_endpoints_are_canonical_and_join_without_duplication() {
        let root = ProviderEndpoint::parse(" http://m4macmini.local:1234/ ").expect("root");
        let v1 = ProviderEndpoint::parse("http://m4macmini.local:1234/v1/").expect("v1");

        assert_eq!(root.as_str(), "http://m4macmini.local:1234");
        assert_eq!(v1.as_str(), "http://m4macmini.local:1234/v1");
        assert_eq!(v1.catalog_root(), root);
        assert_eq!(
            root.join_api_path("v1/responses").expect("join").as_str(),
            "http://m4macmini.local:1234/v1/responses"
        );
        assert_eq!(
            v1.join_api_path("v1/responses").expect("join").as_str(),
            "http://m4macmini.local:1234/v1/responses"
        );
    }

    #[test]
    fn endpoint_rejects_url_borne_secrets_and_non_http_targets_without_echoing_them() {
        for (raw, expected) in [
            (
                "https://user:secret@provider.example/v1",
                ProviderEndpointError::UserInfoNotAllowed,
            ),
            (
                "https://provider.example/v1?api_key=hidden",
                ProviderEndpointError::QueryNotAllowed,
            ),
            (
                "https://provider.example/v1#debug",
                ProviderEndpointError::FragmentNotAllowed,
            ),
            (
                "file:///tmp/provider.sock",
                ProviderEndpointError::UnsupportedScheme,
            ),
        ] {
            let error = ProviderEndpoint::parse(raw).expect_err("endpoint must be rejected");
            assert_eq!(error, expected);
            let diagnostic = format!("{error:?}: {error}");
            assert!(!diagnostic.contains("secret"));
            assert!(!diagnostic.contains("hidden"));
            assert!(!diagnostic.contains(raw));
            assert_eq!(
                sanitize_provider_endpoint(raw),
                "<invalid-provider-endpoint>"
            );
        }
    }

    #[test]
    fn complete_turn_capture_preserves_explicit_none_without_partial_transfer() {
        let mut config = ResolvedConfig::default();
        config.model.api_key_env = None;
        config.model.temperature = None;
        config.model.seed = None;
        config.model.extra_body_json = None;
        let turn = ResolvedTurnConfig::capture(config).expect("valid endpoint");

        assert_eq!(turn.runtime_config().model.api_key_env, None);
        assert_eq!(turn.runtime_config().model.temperature, None);
        assert_eq!(turn.runtime_config().model.seed, None);
        assert_eq!(turn.runtime_config().model.extra_body_json, None);
    }

    #[test]
    fn complete_turn_capture_owns_canonical_model_and_response_start_deadline() {
        let mut canonical = ResolvedConfig::default();
        canonical.model.model = "  canonical-model  ".to_string();
        let turn = ResolvedTurnConfig::capture(canonical).expect("canonical turn config");
        assert_eq!(turn.runtime_config().model.model, "canonical-model");
        assert_eq!(turn.provider().model(), "canonical-model");

        let overridden = turn
            .with_model_override("  override-model  ")
            .expect("valid canonical model override");
        assert_eq!(overridden.runtime_config().model.model, "override-model");
        assert_eq!(overridden.provider().model(), "override-model");
        assert!(turn.with_model_override(" \t ").is_err());

        for (field, mut invalid) in [
            ("model.model", {
                let mut config = ResolvedConfig::default();
                config.model.model = "  ".to_string();
                config
            }),
            ("model.request_timeout_ms", {
                let mut config = ResolvedConfig::default();
                config.model.request_timeout_ms = 0;
                config
            }),
        ] {
            let error = ResolvedTurnConfig::capture(std::mem::take(&mut invalid))
                .expect_err("invalid provider runtime config must fail closed");
            assert!(matches!(
                &error,
                ResolvedTurnConfigError::ProviderRuntime { .. }
            ));
            assert!(error.to_string().contains(field));
        }
    }

    #[test]
    fn turn_capture_freezes_the_admission_access_snapshot_and_multi_agent_settings() {
        let mut effective = ResolvedConfig::default();
        effective.permissions.access_mode = crate::config::AccessMode::Default;
        effective.multi_agent.enabled = false;
        let turn = ResolvedTurnConfig::capture(effective.clone()).expect("valid endpoint");

        effective.permissions.access_mode = crate::config::AccessMode::FullAccess;
        effective.multi_agent.enabled = true;
        effective.multi_agent.mode = crate::config::MultiAgentMode::Proactive;

        assert_eq!(
            turn.runtime_config().permissions.access_mode,
            crate::config::AccessMode::Default
        );
        assert!(!turn.runtime_config().multi_agent.enabled);
    }

    #[test]
    fn complete_turn_capture_rejects_relative_workspace_boundary_roots() {
        let mut additional_read = ResolvedConfig::default();
        additional_read.permissions.additional_read_roots =
            vec![camino::Utf8PathBuf::from("relative/read-root")];
        let mut additional_write = ResolvedConfig::default();
        additional_write.permissions.additional_write_roots =
            vec![camino::Utf8PathBuf::from("relative/write-root")];
        let mut protected = ResolvedConfig::default();
        protected.workspace.protected_paths =
            vec![camino::Utf8PathBuf::from("relative/protected-root")];

        for (field, config) in [
            ("permissions.additional_read_roots[0]", additional_read),
            ("permissions.additional_write_roots[0]", additional_write),
            ("workspace.protected_paths[0]", protected),
        ] {
            let error = ResolvedTurnConfig::capture(config)
                .expect_err("relative safety roots must not enter immutable turn state");
            assert!(matches!(
                &error,
                ResolvedTurnConfigError::WorkspaceBoundary { .. }
            ));
            assert!(error.to_string().contains(field));
            assert!(error.to_string().contains("absolute path"));
        }
    }

    #[test]
    fn rejected_endpoint_never_enters_turn_state_or_debug_projection() {
        let mut config = ResolvedConfig::default();
        config.model.base_url =
            "https://user:secret@provider.example/v1?api_key=hidden".to_string();
        let error = ResolvedTurnConfig::capture(config).expect_err("reject secret URL");

        let debug = format!("{error:?}: {error}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("hidden"));
    }

    #[test]
    fn turn_capture_owns_the_complete_provider_deadline_policy() {
        let mut config = ResolvedConfig::default();
        config.model.request_timeout_ms = 91_000;
        config.model.stream_idle_timeout_ms = 17_000;
        config.model.connect_timeout_ms = 3_000;
        config.model.max_retries = 4;

        let turn = ResolvedTurnConfig::capture(config).expect("valid endpoint");

        assert_eq!(
            turn.provider().deadlines(),
            ProviderDeadlines {
                response_start_timeout_ms: 91_000,
                stream_idle_timeout_ms: 17_000,
                connect_timeout_ms: 3_000,
                max_connect_retries: 4,
            }
        );
    }

    #[test]
    fn provider_target_constructor_rejects_blank_model_and_zero_response_start_deadline() {
        let deadlines = ProviderDeadlines {
            response_start_timeout_ms: 1_000,
            stream_idle_timeout_ms: 1_000,
            connect_timeout_ms: 100,
            max_connect_retries: 0,
        };
        let blank_model = ProviderTarget::new(
            "http://provider.local",
            " \t ",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::Responses,
            deadlines,
        )
        .expect_err("blank provider model must fail closed");
        assert!(blank_model.to_string().contains("model.model"));

        let zero_deadline = ProviderTarget::new(
            "http://provider.local",
            "configured-model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 0,
                ..deadlines
            },
        )
        .expect_err("zero response-start deadline must fail closed");
        assert!(
            zero_deadline
                .to_string()
                .contains("model.request_timeout_ms")
        );
    }
}
