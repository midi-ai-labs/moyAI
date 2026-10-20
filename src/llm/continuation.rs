//! Private, run-local transport state. Never a canonical conversation item.
use std::{fmt, sync::Arc};

use crate::config::{ProviderApiMode, ProviderTarget};

#[derive(Clone, PartialEq, Eq)]
pub struct ChatContinuationScope(ContinuationOrigin);

#[derive(Clone, PartialEq, Eq)]
enum ContinuationOrigin {
    Direct {
        endpoint: String,
        model: String,
        api: ProviderApiMode,
    },
    Hub {
        endpoint: String,
        hub_id: String,
        catalog_revision: String,
        lease_id: String,
        model: String,
        api: ProviderApiMode,
    },
}

impl ChatContinuationScope {
    pub(crate) fn direct(target: &ProviderTarget) -> Self {
        Self(ContinuationOrigin::Direct {
            endpoint: target.sanitized_endpoint().to_string(),
            model: target.model().to_string(),
            api: target.api_mode(),
        })
    }

    pub(crate) fn hub(
        endpoint: &str,
        hub_id: &str,
        catalog_revision: &str,
        lease_id: &str,
        model: &str,
        api: ProviderApiMode,
    ) -> Self {
        Self(ContinuationOrigin::Hub {
            endpoint: endpoint.to_string(),
            hub_id: hub_id.to_string(),
            catalog_revision: catalog_revision.to_string(),
            lease_id: lease_id.to_string(),
            model: model.to_string(),
            api,
        })
    }
}

impl fmt::Debug for ChatContinuationScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChatContinuationScope(<private>)")
    }
}

/// Complete reasoning for one successfully committed assistant tool response.
/// The run owner binds this value to its canonical ModelResponseId in memory.
#[derive(Clone)]
pub struct ChatToolContinuation {
    scope: ChatContinuationScope,
    reasoning_content: Arc<str>,
}

impl ChatToolContinuation {
    pub(crate) fn new(scope: ChatContinuationScope, reasoning_content: String) -> Self {
        Self {
            scope,
            reasoning_content: Arc::from(reasoning_content),
        }
    }

    pub(crate) fn scope(&self) -> &ChatContinuationScope {
        &self.scope
    }

    pub(crate) fn reasoning_content(&self) -> &str {
        &self.reasoning_content
    }

    pub(crate) fn len(&self) -> usize {
        self.reasoning_content.len()
    }
}

impl fmt::Debug for ChatToolContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatToolContinuation")
            .field("bytes", &self.len())
            .field("content", &"<private>")
            .finish()
    }
}

/// Keeps DTO Debug output from exposing a streamed private reasoning fragment.
#[derive(serde::Deserialize)]
#[serde(transparent)]
pub struct ReasoningFragment(String);

impl ReasoningFragment {
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for ReasoningFragment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReasoningFragment(<private>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_continuation_scope_separates_endpoint_model_and_api() {
        let scope = |endpoint, model, profile| {
            ChatContinuationScope::direct(
                &ProviderTarget::new(
                    endpoint,
                    model,
                    profile,
                    crate::config::ProviderDeadlines {
                        request_timeout_ms: 30_000,
                        connect_timeout_ms: 1_000,
                        max_connect_retries: 0,
                    },
                )
                .unwrap(),
            )
        };
        let original = scope(
            "http://provider/v1",
            "model-a",
            crate::config::ProviderProfile::OpenAiCompatible,
        );
        assert_eq!(
            original,
            scope(
                "http://provider/v1",
                "model-a",
                crate::config::ProviderProfile::OpenAiCompatible
            )
        );
        assert_ne!(
            original,
            scope(
                "http://other/v1",
                "model-a",
                crate::config::ProviderProfile::OpenAiCompatible
            )
        );
        assert_ne!(
            original,
            scope(
                "http://provider/v1",
                "model-b",
                crate::config::ProviderProfile::OpenAiCompatible
            )
        );
        assert_ne!(
            original,
            scope(
                "http://provider/v1",
                "model-a",
                crate::config::ProviderProfile::OpenAiResponses
            )
        );
    }

    #[test]
    fn hub_continuation_scope_follows_the_exact_lease_model_and_api() {
        let scope = |endpoint, hub, revision, lease, model, api| {
            ChatContinuationScope::hub(endpoint, hub, revision, lease, model, api)
        };
        let original = scope(
            "https://hub",
            "hub-a",
            "1",
            "lease-a",
            "model-a",
            ProviderApiMode::ChatCompletions,
        );
        assert_eq!(
            original,
            scope(
                "https://hub",
                "hub-a",
                "1",
                "lease-a",
                "model-a",
                ProviderApiMode::ChatCompletions
            )
        );
        for different in [
            scope(
                "https://other",
                "hub-a",
                "1",
                "lease-a",
                "model-a",
                ProviderApiMode::ChatCompletions,
            ),
            scope(
                "https://hub",
                "hub-b",
                "1",
                "lease-a",
                "model-a",
                ProviderApiMode::ChatCompletions,
            ),
            scope(
                "https://hub",
                "hub-a",
                "2",
                "lease-a",
                "model-a",
                ProviderApiMode::ChatCompletions,
            ),
            scope(
                "https://hub",
                "hub-a",
                "1",
                "lease-b",
                "model-a",
                ProviderApiMode::ChatCompletions,
            ),
            scope(
                "https://hub",
                "hub-a",
                "1",
                "lease-a",
                "model-b",
                ProviderApiMode::ChatCompletions,
            ),
            scope(
                "https://hub",
                "hub-a",
                "1",
                "lease-a",
                "model-a",
                ProviderApiMode::Responses,
            ),
        ] {
            assert_ne!(original, different);
        }
    }

    #[test]
    fn private_fragment_debug_does_not_include_text() {
        let value: ReasoningFragment = serde_json::from_str("\"private-fragment-canary\"").unwrap();
        assert!(!format!("{value:?}").contains("private-fragment-canary"));
    }
}
