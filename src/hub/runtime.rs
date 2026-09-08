use super::*;
use crate::config::{ProviderDeadlines, ProviderProfile, ProviderTarget, ResolvedConfig};
use crate::error::LlmError;
use crate::hub::client::PreparedRequest;
use crate::llm::{ChatRequest, LlmClient, LlmEventSink, LlmResponseSummary};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubRouteMode {
    #[default]
    Direct,
    Hub,
}

#[derive(Debug, Clone, Serialize)]
pub struct HubActiveTurnProjection {
    pub turn_id: String,
    pub phase: &'static str,
    pub logical_model_id: Option<String>,
}

pub(super) struct ActiveHubTurn {
    pub display: HubActiveTurnProjection,
    pub cancel: CancellationToken,
}

struct TurnRouteInner {
    connection: HubConnection,
    client: RegisteredHubClient,
    generation: u64,
    context: HubReviewContext,
    turn_id: String,
    review: ReviewedHubSelection,
    catalog: HubCatalog,
    endpoint: String,
    cancel: CancellationToken,
    finished: std::sync::atomic::AtomicBool,
    used_models: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

#[derive(Clone)]
pub struct HubTurnRoute {
    inner: Arc<TurnRouteInner>,
}

impl std::fmt::Debug for HubConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HubConnection(<runtime>)")
    }
}
impl std::fmt::Debug for HubTurnRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HubTurnRoute(<runtime>)")
    }
}

impl HubReviewContext {
    fn mode(self, settings: &HubSettings) -> HubRouteMode {
        match self {
            Self::Main => settings.main_mode,
            Self::SideChat => settings.side_chat_mode,
        }
    }
}

impl HubConnection {
    pub async fn set_route_mode(
        &self,
        context: HubReviewContext,
        mode: HubRouteMode,
        expected_settings_revision: String,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        self.set_route_mode_now(
            context,
            mode,
            expected_settings_revision,
            expected_connection_generation,
        )
    }
    pub(crate) fn set_route_mode_now(
        &self,
        context: HubReviewContext,
        mode: HubRouteMode,
        expected_settings_revision: String,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        state.check_generation(&expected_connection_generation)?;
        state.check_settings(&expected_settings_revision)?;
        if state.active[context.index()].is_some() {
            return Err(HubError::RouteBusy);
        }
        if mode == HubRouteMode::Hub
            && state.confirmation(context) != HubReviewConfirmation::Confirmed
        {
            return Err(HubError::ReviewRequired);
        }
        let mut proposed = state.settings.clone();
        match context {
            HubReviewContext::Main => proposed.main_mode = mode,
            HubReviewContext::SideChat => proposed.side_chat_mode = mode,
        }
        state.settings = self.persisted_store()?.save(&proposed)?;
        Ok(state.projection())
    }

    /// Capture a user-turn owner synchronously, before starting a detached Desktop worker.
    /// The mode and reviewed identities cannot change under an admitted request.
    pub fn begin_turn(
        &self,
        context: HubReviewContext,
        cancel: CancellationToken,
    ) -> Result<Option<HubTurnRoute>, HubError> {
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        if context.mode(&state.settings) == HubRouteMode::Direct {
            return Ok(None);
        }
        if state.active[context.index()].is_some() {
            return Err(HubError::RouteBusy);
        }
        if state.status != HubConnectionStatus::Connected {
            return Err(HubError::Unavailable);
        }
        if state.confirmation(context) != HubReviewConfirmation::Confirmed {
            return Err(HubError::ReviewRequired);
        }
        let review = context
            .review(&state.settings)
            .clone()
            .ok_or(HubError::ReviewRequired)?;
        let client = state.client.clone().ok_or(HubError::Unavailable)?;
        let catalog = state.catalog.clone().ok_or(HubError::Unavailable)?;
        let turn_id = ulid::Ulid::new().to_string();
        state.active[context.index()] = Some(ActiveHubTurn {
            display: HubActiveTurnProjection {
                turn_id: turn_id.clone(),
                phase: "waiting",
                logical_model_id: None,
            },
            cancel: cancel.clone(),
        });
        Ok(Some(HubTurnRoute {
            inner: Arc::new(TurnRouteInner {
                connection: self.clone(),
                client,
                generation: state.generation,
                context,
                turn_id,
                review,
                catalog,
                endpoint: state.settings.endpoint.clone(),
                cancel,
                finished: std::sync::atomic::AtomicBool::new(false),
                used_models: std::sync::Mutex::new(Default::default()),
            }),
        }))
    }
}

impl HubTurnRoute {
    pub fn logical_model(&self) -> &str {
        &self.inner.review.selection.preferred_model_id
    }
    pub fn hub_endpoint(&self) -> &str {
        &self.inner.endpoint
    }
    pub fn metrics_model(&self) -> String {
        let models = self
            .inner
            .used_models
            .lock()
            .expect("Hub model identity lock poisoned");
        if models.is_empty() {
            "Hub (unallocated)".into()
        } else {
            models.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    }

    /// Runtime policy only; the caller retains the original persisted Direct configuration.
    pub fn runtime_config(&self, direct: &ResolvedConfig) -> ResolvedConfig {
        let mut config = direct.clone();
        config.model.model = self.logical_model().into();
        config.model.base_url = self.hub_endpoint().into();
        config.model.provider_profile = ProviderProfile::OpenAiCompatible;
        config.model.api_key_env = None;
        config.model.extra_headers.clear();
        config.model.clear_legacy_generation_settings();
        // All selectable fallback models must support the advertised turn envelope.
        let models: Vec<_> = self
            .inner
            .catalog
            .models
            .iter()
            .filter(|model| {
                self.inner
                    .review
                    .selection
                    .allowed_model_ids
                    .contains(&model.id)
            })
            .collect();
        config.model.supports_tools = models
            .iter()
            .all(|model| model.capabilities.contains("tools"));
        config.model.supports_images = models
            .iter()
            .all(|model| model.capabilities.contains("vision"));
        config.model.supports_reasoning = false;
        config.multi_agent.enabled = false;
        config
    }

    pub fn client(&self) -> Arc<dyn LlmClient> {
        Arc::new(HubRoutedClient {
            route: self.clone(),
        })
    }

    fn ensure_current(&self) -> Result<(), LlmError> {
        if self.inner.cancel.is_cancelled() {
            return Err(LlmError::Message("Hub request cancelled".into()));
        }
        let state = self
            .inner
            .connection
            .inner
            .state
            .lock()
            .expect("Hub state lock poisoned");
        if state.generation != self.inner.generation
            || state.status != HubConnectionStatus::Connected
        {
            return Err(LlmError::Message(HubError::ConnectionChanged.code().into()));
        }
        self.inner
            .review
            .check_admission(
                state
                    .catalog
                    .as_ref()
                    .ok_or_else(|| LlmError::Message("Hub unavailable".into()))?,
            )
            .map_err(|error| LlmError::Message(error.code().into()))
    }

    fn phase(&self, phase: &'static str, model: Option<String>) {
        let mut state = self
            .inner
            .connection
            .inner
            .state
            .lock()
            .expect("Hub state lock poisoned");
        if let Some(active) = state.active[self.inner.context.index()]
            .as_mut()
            .filter(|active| active.display.turn_id == self.inner.turn_id)
        {
            active.display.phase = phase;
            active.display.logical_model_id = model;
        }
    }

    fn review_rejected(&self, error: HubError) {
        let mut state = self
            .inner
            .connection
            .inner
            .state
            .lock()
            .expect("Hub state lock poisoned");
        if state.generation == self.inner.generation {
            state.record_review_rejection(self.inner.review.reviewed_revision, error);
        }
    }

    pub async fn finish(&self) {
        if self
            .inner
            .finished
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let cancelled = self.inner.cancel.is_cancelled()
            || self
                .inner
                .connection
                .inner
                .state
                .lock()
                .expect("Hub state lock poisoned")
                .active[self.inner.context.index()]
            .as_ref()
            .filter(|active| active.display.turn_id == self.inner.turn_id)
            .is_some_and(|active| active.cancel.is_cancelled());
        self.inner
            .client
            .close_turn(self.inner.context, &self.inner.turn_id, cancelled)
            .await;
        self.clear_active();
        if self.inner.connection.inner.store.is_none() {
            self.inner.client.disconnect().await;
        }
    }

    fn clear_active(&self) {
        let mut state = self
            .inner
            .connection
            .inner
            .state
            .lock()
            .expect("Hub state lock poisoned");
        if state.active[self.inner.context.index()]
            .as_ref()
            .is_some_and(|active| active.display.turn_id == self.inner.turn_id)
        {
            state.active[self.inner.context.index()] = None;
        }
    }
}

impl Drop for TurnRouteInner {
    fn drop(&mut self) {
        if self
            .finished
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        self.cancel.cancel();
        {
            let mut state = self
                .connection
                .inner
                .state
                .lock()
                .expect("Hub state lock poisoned");
            if state.active[self.context.index()]
                .as_ref()
                .is_some_and(|active| active.display.turn_id == self.turn_id)
            {
                if let Some(active) = &state.active[self.context.index()] {
                    active.cancel.cancel();
                }
                state.active[self.context.index()] = None;
            }
        }
        let client = self.client.clone();
        let context = self.context;
        let turn_id = self.turn_id.clone();
        let dedicated = self.connection.inner.store.is_none();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                client.close_turn(context, &turn_id, true).await;
                if dedicated {
                    client.disconnect().await;
                }
            });
        }
    }
}

struct HubRoutedClient {
    route: HubTurnRoute,
}

struct HubTraceSink<'a> {
    inner: &'a mut dyn LlmEventSink,
    endpoint: &'a str,
}
impl LlmEventSink for HubTraceSink<'_> {
    fn push(&mut self, event: crate::llm::LlmEvent) -> Result<(), LlmError> {
        self.inner.push(event)
    }
    fn provider_phase(
        &mut self,
        mut event: crate::llm::ProviderPhaseEvent,
    ) -> Result<(), LlmError> {
        event.endpoint = self.endpoint.into();
        if let Some(failure) = &mut event.failure {
            failure.endpoint = self.endpoint.into();
        }
        self.inner.provider_phase(event)
    }
}

fn public_route_error(mut error: LlmError, endpoint: &str) -> LlmError {
    if let LlmError::ProviderFailure { failure, source } = &mut error {
        failure.endpoint = endpoint.into();
        if let LlmError::ProviderFailure { failure, .. } = source.as_mut() {
            failure.endpoint = endpoint.into();
        }
    }
    error
}

fn gateway_review_rejection(error: &LlmError) -> Option<HubError> {
    match error {
        LlmError::ProviderFailure { source, .. } => gateway_review_rejection(source),
        LlmError::ProviderRejected {
            status: Some(409),
            message,
            ..
        } => {
            let value: serde_json::Value = serde_json::from_str(message).ok()?;
            match value.get("error")?.as_str()? {
                "review_required" => Some(HubError::ReviewRequired),
                "different_hub" => Some(HubError::DifferentHub),
                _ => None,
            }
        }
        _ => None,
    }
}

#[async_trait::async_trait(?Send)]
impl LlmClient for HubRoutedClient {
    async fn stream_chat(
        &self,
        mut request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        self.route.ensure_current()?;
        {
            let mut state = self
                .route
                .inner
                .connection
                .inner
                .state
                .lock()
                .expect("Hub state lock poisoned");
            if state.generation != self.route.inner.generation {
                return Err(LlmError::Message(HubError::ConnectionChanged.code().into()));
            }
            if let Some(active) = state.active[self.route.inner.context.index()]
                .as_mut()
                .filter(|active| active.display.turn_id == self.route.inner.turn_id)
            {
                // Main replaces its outer admission control with the admitted turn control.
                // Disconnect and /turns/cancel must observe that current request owner.
                active.cancel = cancel.clone();
            }
        }
        let request_id = ulid::Ulid::new().to_string();
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(request.provider_target().deadlines().request_timeout_ms);
        self.route.phase("waiting", None);
        let network_http = self
            .route
            .inner
            .connection
            .inner
            .network_http
            .lock()
            .unwrap()
            .clone();
        let (prepared, transport_lease) = loop {
            self.route.ensure_current()?;
            let lease = match &network_http {
                Some(http) => Some(tokio::select! {
                    _ = cancel.cancelled() => return Err(LlmError::Message("Hub request cancelled".into())),
                    _ = tokio::time::sleep_until(deadline) => return Err(LlmError::Message("Hub allocation wait timed out".into())),
                    lease = http.acquire() => lease,
                }),
                None => None,
            };
            let result = tokio::select! {
                _ = cancel.cancelled() => return Err(LlmError::Message("Hub request cancelled".into())),
                _ = tokio::time::sleep_until(deadline) => return Err(LlmError::Message("Hub allocation wait timed out".into())),
                result = self.route.inner.client.prepare(self.route.inner.context, &self.route.inner.turn_id, &request_id, &self.route.inner.review) => result,
            }.map_err(|error| { self.route.review_rejected(error); LlmError::Message(error.code().into()) })?;
            match result {
                PreparedRequest::Waiting {
                    reason,
                    retry_after_ms,
                } => {
                    // No permit exists while waiting. Let identity renewal run between polls.
                    drop(lease);
                    if !matches!(
                        reason.as_str(),
                        "busy"
                            | "global_capacity"
                            | "target_unavailable"
                            | "maintenance"
                            | "jit_pending"
                    ) || !(100..=10_000).contains(&retry_after_ms)
                    {
                        return Err(LlmError::Message("Hub wait response is invalid".into()));
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(LlmError::Message("Hub request cancelled".into())),
                        _ = tokio::time::sleep_until(deadline) => return Err(LlmError::Message("Hub allocation wait timed out".into())),
                        _ = tokio::time::sleep(Duration::from_millis(retry_after_ms)) => {}
                    }
                }
                ready => break (ready, lease),
            }
        };
        let PreparedRequest::Ready {
            lease_id,
            permit_id,
            logical_model_id,
            gateway_base_url,
            request_token,
            expires_at_ms,
            provider_profile,
            provider_api_mode,
        } = prepared
        else {
            unreachable!()
        };
        if !crate::hub::valid_id(&lease_id)
            || !crate::hub::valid_id(&permit_id)
            || !self
                .route
                .inner
                .review
                .selection
                .allowed_model_ids
                .contains(&logical_model_id)
            || (self.route.inner.review.selection.wait_policy
                == crate::hub::HubWaitPolicy::WaitForPreferred
                && self.route.inner.review.selection.preferred_model_id != logical_model_id)
            || !self.route.inner.catalog.models.iter().any(|model| {
                model.id == logical_model_id
                    && self
                        .route
                        .inner
                        .review
                        .selection
                        .required_capabilities
                        .is_subset(&model.capabilities)
            })
            || decimal(&expires_at_ms).is_none()
            || provider_profile != "openai_compatible_chat"
            || provider_api_mode != "chat_completions"
        {
            return Err(LlmError::Message("Hub route response is invalid".into()));
        }
        let url = reqwest::Url::parse(&gateway_base_url)
            .map_err(|_| LlmError::Message("Hub gateway target is invalid".into()))?;
        let transport_allowed = if network_http.is_some() {
            let hub = reqwest::Url::parse(self.route.hub_endpoint())
                .map_err(|_| LlmError::Message("Hub gateway target is invalid".into()))?;
            url.scheme() == "https" && url.host_str() == hub.host_str()
        } else {
            url.scheme() == "http"
                && url.host_str().is_some_and(|host| {
                    host.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                })
        };
        if !transport_allowed
            || url.path() != format!("/r/{permit_id}/v1")
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || !(32..=256).contains(&request_token.len())
            || !request_token.bytes().all(|value| {
                value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.' | b'~')
            })
        {
            return Err(LlmError::Message("Hub gateway target is invalid".into()));
        }
        self.route.ensure_current()?;
        let deadlines = request.provider_target().deadlines();
        let target = ProviderTarget::new(
            &gateway_base_url,
            &logical_model_id,
            ProviderProfile::OpenAiCompatible,
            ProviderDeadlines {
                max_connect_retries: 0,
                ..deadlines
            },
        )
        .map_err(|_| LlmError::Message("Hub gateway target is invalid".into()))?;
        request.route_through_hub(target, request_token);
        self.route
            .inner
            .used_models
            .lock()
            .expect("Hub model identity lock poisoned")
            .insert(logical_model_id.clone());
        self.route.phase("running", Some(logical_model_id));
        // The gateway is a separate process. It claims once, substitutes the real provider
        // model, and owns upstream completion/settlement even after downstream Stop.
        let mut trace = HubTraceSink {
            inner: sink,
            endpoint: self.route.hub_endpoint(),
        };
        let gateway = match &transport_lease {
            Some(lease) => {
                crate::llm::OpenAiCompatClient::new(None).with_runtime_http(lease.http.clone())
            }
            None => crate::llm::OpenAiCompatClient::new(None),
        };
        let result = gateway
            .stream_chat(request, cancel, &mut trace)
            .await
            .map_err(|error| {
                if let Some(rejected) = gateway_review_rejection(&error) {
                    self.route.review_rejected(rejected);
                    LlmError::Message(rejected.code().into())
                } else {
                    public_route_error(error, self.route.hub_endpoint())
                }
            });
        drop(transport_lease);
        result
    }
}
