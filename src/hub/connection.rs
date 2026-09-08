use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::client::{RegisteredHubClient, validated_endpoint};
use super::settings::decimal;
use super::{
    CatalogRevision, HubCatalog, HubCatalogClient, HubError, HubSelection, HubSettings,
    HubSettingsStore, ReviewedHubSelection, bounded_text,
};

#[path = "runtime.rs"]
mod runtime;
use runtime::{ActiveHubTurn, HubActiveTurnProjection};
pub use runtime::{HubRouteMode, HubTurnRoute};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubReviewContext {
    Main,
    SideChat,
}

impl HubReviewContext {
    fn index(self) -> usize {
        match self {
            Self::Main => 0,
            Self::SideChat => 1,
        }
    }
    fn review(self, settings: &HubSettings) -> &Option<ReviewedHubSelection> {
        match self {
            Self::Main => &settings.main_review,
            Self::SideChat => &settings.side_chat_review,
        }
    }
    fn set(self, settings: &mut HubSettings, review: ReviewedHubSelection) {
        match self {
            Self::Main => settings.main_review = Some(review),
            Self::SideChat => settings.side_chat_review = Some(review),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HubConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Stale,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HubReviewConfirmation {
    Unconfirmed,
    Confirmed,
    ReviewRequired,
}

/// Management and active route projection from the same application-owned connection.
#[derive(Debug, Clone, Serialize)]
pub struct HubConnectionProjection {
    pub settings_revision: String,
    pub connection_generation: String,
    pub status: HubConnectionStatus,
    pub endpoint: String,
    pub label: String,
    pub hub_id: Option<String>,
    pub catalog: Option<HubCatalog>,
    pub main_review: Option<ReviewedHubSelection>,
    #[serde(default)]
    pub recommended_main_selection: Option<HubSelection>,
    pub side_chat_review: Option<ReviewedHubSelection>,
    pub main_confirmation: HubReviewConfirmation,
    pub side_chat_confirmation: HubReviewConfirmation,
    pub error: Option<&'static str>,
    pub main_mode: HubRouteMode,
    pub side_chat_mode: HubRouteMode,
    pub active_main: Option<HubActiveTurnProjection>,
    pub active_side_chat: Option<HubActiveTurnProjection>,
    pub can_enable_main_hub: bool,
    pub can_enable_side_chat_hub: bool,
    pub can_change_main_mode: bool,
    pub can_change_side_chat_mode: bool,
}

struct ConnectionState {
    settings: HubSettings,
    storage_error: Option<HubError>,
    generation: u64,
    status: HubConnectionStatus,
    client: Option<RegisteredHubClient>,
    catalog: Option<HubCatalog>,
    recommendation: Option<ReviewedHubSelection>,
    // These are remote acknowledgements for the current registration, not extra durable truth.
    confirmed: [Option<ReviewedHubSelection>; 2],
    cancellation: CancellationToken,
    error: Option<HubError>,
    active: [Option<ActiveHubTurn>; 2],
}

impl ConnectionState {
    fn record_review_rejection(&mut self, revision: CatalogRevision, error: HubError) {
        if error == HubError::ReviewRequired {
            for confirmed in &mut self.confirmed {
                if confirmed
                    .as_ref()
                    .is_some_and(|value| value.reviewed_revision <= revision)
                {
                    *confirmed = None;
                }
            }
            if self
                .catalog
                .as_ref()
                .is_none_or(|catalog| catalog.revision <= revision)
            {
                self.error = Some(error);
            }
        } else {
            self.error = Some(error);
        }
        if matches!(error, HubError::Unauthorized | HubError::DifferentHub) {
            self.status = HubConnectionStatus::Stale;
            self.confirmed = [None, None];
            self.cancellation.cancel();
            HubConnection::revoke(self.client.take());
        }
    }
    fn check_generation(&self, expected: &str) -> Result<(), HubError> {
        if decimal(expected) != Some(self.generation) {
            return Err(HubError::ConnectionChanged);
        }
        Ok(())
    }
    fn check_settings(&self, expected: &str) -> Result<(), HubError> {
        if let Some(error) = self.storage_error {
            return Err(error);
        }
        if expected != self.settings.revision {
            return Err(HubError::SettingsChanged);
        }
        Ok(())
    }
    fn advance(&mut self) -> Result<Option<RegisteredHubClient>, HubError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(HubError::ConnectionChanged)?;
        self.cancellation.cancel();
        for active in self.active.iter().flatten() {
            active.cancel.cancel();
        }
        self.cancellation = CancellationToken::new();
        self.confirmed = [None, None];
        self.error = self.storage_error;
        Ok(self.client.take())
    }
    fn confirmation(&self, context: HubReviewContext) -> HubReviewConfirmation {
        let Some(review) = context.review(&self.settings).as_ref() else {
            return HubReviewConfirmation::Unconfirmed;
        };
        if matches!(
            self.error,
            Some(HubError::ReviewRequired | HubError::DifferentHub)
        ) {
            return HubReviewConfirmation::ReviewRequired;
        }
        if self
            .catalog
            .as_ref()
            .is_some_and(|catalog| review.check_admission(catalog).is_err())
        {
            return HubReviewConfirmation::ReviewRequired;
        }
        if self.status == HubConnectionStatus::Connected
            && self.confirmed[context.index()].as_ref() == Some(review)
        {
            HubReviewConfirmation::Confirmed
        } else {
            HubReviewConfirmation::Unconfirmed
        }
    }
    fn projection(&self) -> HubConnectionProjection {
        HubConnectionProjection {
            settings_revision: self.settings.revision.clone(),
            connection_generation: self.generation.to_string(),
            status: self.status,
            endpoint: self.settings.endpoint.clone(),
            label: self.settings.label.clone(),
            hub_id: self.settings.hub_id.clone(),
            catalog: self.catalog.clone(),
            main_review: self.settings.main_review.clone(),
            recommended_main_selection: self
                .recommendation
                .as_ref()
                .filter(|review| {
                    self.status == HubConnectionStatus::Connected
                        && self
                            .catalog
                            .as_ref()
                            .is_some_and(|catalog| review.check_admission(catalog).is_ok())
                })
                .map(|review| review.selection.clone()),
            side_chat_review: self.settings.side_chat_review.clone(),
            main_confirmation: self.confirmation(HubReviewContext::Main),
            side_chat_confirmation: self.confirmation(HubReviewContext::SideChat),
            error: self.error.map(HubError::code),
            main_mode: self.settings.main_mode,
            side_chat_mode: self.settings.side_chat_mode,
            active_main: self.active[0].as_ref().map(|active| active.display.clone()),
            active_side_chat: self.active[1].as_ref().map(|active| active.display.clone()),
            can_enable_main_hub: self.active[0].is_none()
                && self.confirmation(HubReviewContext::Main) == HubReviewConfirmation::Confirmed,
            can_enable_side_chat_hub: self.active[1].is_none()
                && self.confirmation(HubReviewContext::SideChat)
                    == HubReviewConfirmation::Confirmed,
            can_change_main_mode: self.active[0].is_none() && self.storage_error.is_none(),
            can_change_side_chat_mode: self.active[1].is_none() && self.storage_error.is_none(),
        }
    }
}

struct ConnectionInner {
    state: std::sync::Mutex<ConnectionState>,
    store: Option<HubSettingsStore>,
    network_http: std::sync::Mutex<Option<crate::device_network::ManagedHubHttp>>,
    // A later save in the same context must not reach Hub before an earlier request completes.
    reviews: [Mutex<()>; 2],
}

impl Drop for ConnectionInner {
    fn drop(&mut self) {
        self.state
            .get_mut()
            .expect("Hub state lock poisoned")
            .cancellation
            .cancel();
    }
}

/// Sole owner for a Desktop's management registration. Startup only loads preferences and
/// never contacts Hub. No credential is retained in settings, Debug, or the web projection.
#[derive(Clone)]
pub struct HubConnection {
    inner: Arc<ConnectionInner>,
}

impl HubConnection {
    pub fn new(store: HubSettingsStore) -> Self {
        let (settings, storage_error) = match store.load() {
            Ok(settings) => (settings, None),
            Err(error) => (HubSettings::default(), Some(error)),
        };
        Self {
            inner: Arc::new(ConnectionInner {
                state: std::sync::Mutex::new(ConnectionState {
                    settings,
                    storage_error,
                    generation: 0,
                    status: if storage_error.is_some() {
                        HubConnectionStatus::Error
                    } else {
                        HubConnectionStatus::Disconnected
                    },
                    client: None,
                    catalog: None,
                    recommendation: None,
                    confirmed: [None, None],
                    cancellation: CancellationToken::new(),
                    error: storage_error,
                    active: [None, None],
                }),
                store: Some(store),
                network_http: std::sync::Mutex::new(None),
                reviews: [Mutex::new(()), Mutex::new(())],
            }),
        }
    }

    pub async fn projection(&self) -> HubConnectionProjection {
        self.projection_now()
    }

    fn persisted_store(&self) -> Result<&HubSettingsStore, HubError> {
        self.inner.store.as_ref().ok_or(HubError::SettingsInvalid)
    }

    /// A remote job receives a separate model session and active-turn owner.
    /// Its reviewed Hub default is runtime-only and never overwrites Main/Side.
    pub(crate) async fn device_worker_route(
        endpoint: &str,
        http: crate::device_network::ManagedHubHttp,
        cancel: CancellationToken,
    ) -> Result<HubTurnRoute, HubError> {
        let (client, selection) =
            super::client::HubCatalogClient::device_session(endpoint, http.clone()).await?;
        let catalog = match client.catalog().await {
            Ok(catalog) => catalog,
            Err(error) => {
                client.disconnect().await;
                return Err(error);
            }
        };
        let review = match selection {
            Some(selection) => {
                ReviewedHubSelection::review(&catalog, &catalog.hub_id, catalog.revision, selection)
            }
            None => Err(HubError::CapabilityMismatch),
        };
        let review = match review {
            Ok(review) => review,
            Err(error) => {
                client.disconnect().await;
                return Err(error);
            }
        };
        if let Err(error) = client.review(HubReviewContext::Main, &review).await {
            client.disconnect().await;
            return Err(error);
        }
        let mut settings = HubSettings::default();
        settings.endpoint = endpoint.to_string();
        settings.hub_id = Some(catalog.hub_id.clone());
        settings.main_review = Some(review.clone());
        settings.main_mode = HubRouteMode::Hub;
        let cancellation = CancellationToken::new();
        let interval = client.heartbeat_interval_ms;
        let owner = Self {
            inner: Arc::new(ConnectionInner {
                state: std::sync::Mutex::new(ConnectionState {
                    settings,
                    storage_error: None,
                    generation: 0,
                    status: HubConnectionStatus::Connected,
                    client: Some(client),
                    catalog: Some(catalog),
                    recommendation: None,
                    confirmed: [Some(review), None],
                    cancellation: cancellation.clone(),
                    error: None,
                    active: [None, None],
                }),
                store: None,
                network_http: std::sync::Mutex::new(Some(http)),
                reviews: [Mutex::new(()), Mutex::new(())],
            }),
        };
        let route = owner
            .begin_turn(HubReviewContext::Main, cancel)?
            .ok_or(HubError::Unavailable)?;
        owner.start_heartbeat(0, interval, cancellation);
        Ok(route)
    }

    /// Adopt the authenticated device model session without exposing its credentials.
    /// Existing route choices survive; only a first Hub setup may opt into its default.
    pub(crate) async fn connect_device(
        &self,
        endpoint: &str,
        http: crate::device_network::ManagedHubHttp,
        use_default: bool,
    ) -> Result<(), HubError> {
        let endpoint = validated_endpoint(endpoint)?.to_string();
        let generation = {
            let mut state = self.inner.state.lock().unwrap();
            if state.active.iter().any(Option::is_some) {
                return Err(HubError::RouteBusy);
            }
            Self::revoke(state.advance()?);
            state.status = HubConnectionStatus::Connecting;
            state.generation
        };
        let (client, default) =
            match super::client::HubCatalogClient::device_session(&endpoint, http.clone()).await {
                Ok(value) => value,
                Err(error) => {
                    self.fail(generation, error).await;
                    return Err(error);
                }
            };
        let result = async {
            let catalog = client.catalog().await?;
            let mut proposed = self.inner.state.lock().unwrap().settings.clone();
            if proposed
                .hub_id
                .as_ref()
                .is_some_and(|id| id != &catalog.hub_id)
            {
                return Err(HubError::DifferentHub);
            }
            proposed.endpoint = endpoint;
            proposed.hub_id = Some(catalog.hub_id.clone());
            let mut confirmed = [None, None];
            let recommendation = default
                .map(|selection| {
                    ReviewedHubSelection::review(
                        &catalog,
                        &catalog.hub_id,
                        catalog.revision,
                        selection,
                    )
                })
                .transpose()?;
            for context in [HubReviewContext::Main, HubReviewContext::SideChat] {
                let review = context.review(&proposed).clone();
                if let Some(review) =
                    review.filter(|review| review.check_admission(&catalog).is_ok())
                {
                    client.review(context, &review).await?;
                    confirmed[context.index()] = Some(review);
                }
            }
            if use_default && proposed.main_review.is_none() {
                if let Some(review) = recommendation.clone() {
                    client.review(HubReviewContext::Main, &review).await?;
                    proposed.main_review = Some(review.clone());
                    proposed.main_mode = HubRouteMode::Hub;
                    confirmed[0] = Some(review);
                }
            }
            let mut state = self.inner.state.lock().unwrap();
            if state.generation != generation {
                return Err(HubError::ConnectionChanged);
            }
            if proposed != state.settings {
                state.settings = self.persisted_store()?.save(&proposed)?;
            }
            state.catalog = Some(catalog);
            state.recommendation = recommendation;
            state.client = Some(client.clone());
            state.confirmed = confirmed;
            state.status = HubConnectionStatus::Connected;
            state.error = None;
            *self.inner.network_http.lock().unwrap() = Some(http);
            Ok(state.cancellation.clone())
        }
        .await;
        match result {
            Ok(cancel) => {
                self.start_heartbeat(generation, client.heartbeat_interval_ms, cancel);
                Ok(())
            }
            Err(error) => {
                client.disconnect().await;
                self.fail(generation, error).await;
                Err(error)
            }
        }
    }

    pub fn projection_now(&self) -> HubConnectionProjection {
        self.inner
            .state
            .lock()
            .expect("Hub state lock poisoned")
            .projection()
    }

    fn revoke(client: Option<RegisteredHubClient>) {
        if let Some(client) = client {
            tokio::spawn(async move {
                client.disconnect().await;
            });
        }
    }

    async fn fail(&self, generation: u64, error: HubError) {
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        if state.generation != generation {
            return;
        }
        state.status = HubConnectionStatus::Error;
        state.error = Some(error);
        state.confirmed = [None, None];
        state.cancellation.cancel();
        Self::revoke(state.client.take());
    }

    pub async fn connect(
        &self,
        endpoint: String,
        token: String,
        label: String,
        expected_settings_revision: String,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        if !bounded_text(&label, 256) {
            return Err(HubError::InvalidConnection);
        }
        let endpoint = validated_endpoint(&endpoint)?.to_string();
        let bootstrap = HubCatalogClient::new(&endpoint, &token, 5_000)?;
        drop(token);
        let generation = {
            let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
            state.check_generation(&expected_connection_generation)?;
            state.check_settings(&expected_settings_revision)?;
            if self.persisted_store()?.load()? != state.settings {
                return Err(HubError::SettingsChanged);
            }
            Self::revoke(state.advance()?);
            state.status = HubConnectionStatus::Connecting;
            state.catalog = None;
            *self.inner.network_http.lock().unwrap() = None;
            state.generation
        };
        let client = match bootstrap.register(&label).await {
            Ok(client) => client,
            Err(error) => {
                self.fail(generation, error).await;
                return Err(error);
            }
        };
        if self
            .inner
            .state
            .lock()
            .expect("Hub state lock poisoned")
            .generation
            != generation
        {
            Self::revoke(Some(client));
            return Err(HubError::ConnectionChanged);
        }
        let catalog = match client.catalog().await {
            Ok(catalog) => catalog,
            Err(error) => {
                Self::revoke(Some(client));
                self.fail(generation, error).await;
                return Err(error);
            }
        };
        let result = {
            let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
            (|| -> Result<_, HubError> {
                state.check_generation(&generation.to_string())?;
                state.check_settings(&expected_settings_revision)?;
                if self.persisted_store()?.load()? != state.settings {
                    return Err(HubError::SettingsChanged);
                }
                // A server replacement at the saved endpoint requires explicit user action; it
                // cannot silently inherit old reviews. An explicit endpoint switch clears both.
                if state.settings.endpoint == endpoint
                    && state
                        .settings
                        .hub_id
                        .as_ref()
                        .is_some_and(|id| id != &catalog.hub_id)
                {
                    return Err(HubError::DifferentHub);
                }
                if state.settings.hub_id.as_ref() == Some(&catalog.hub_id)
                    && [
                        &state.settings.main_review,
                        &state.settings.side_chat_review,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|review| review.reviewed_revision > catalog.revision)
                {
                    return Err(HubError::RevisionRollback);
                }
                let mut proposed = state.settings.clone();
                if proposed.endpoint != endpoint
                    || proposed.hub_id.as_ref() != Some(&catalog.hub_id)
                {
                    proposed.main_review = None;
                    proposed.side_chat_review = None;
                    proposed.main_mode = HubRouteMode::Direct;
                    proposed.side_chat_mode = HubRouteMode::Direct;
                }
                proposed.endpoint = endpoint;
                proposed.hub_id = Some(catalog.hub_id.clone());
                proposed.label = label;
                if proposed != state.settings {
                    state.settings = self.persisted_store()?.save(&proposed)?;
                }
                state.client = Some(client.clone());
                state.catalog = Some(catalog);
                state.status = HubConnectionStatus::Connected;
                state.error = None;
                Ok((state.projection(), state.cancellation.clone()))
            })()
        };
        match result {
            Ok((projection, cancellation)) => {
                self.start_heartbeat(generation, client.heartbeat_interval_ms, cancellation);
                Ok(projection)
            }
            Err(error) => {
                Self::revoke(Some(client));
                self.fail(generation, error).await;
                Err(error)
            }
        }
    }

    pub async fn disconnect(
        &self,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        state.check_generation(&expected_connection_generation)?;
        Self::revoke(state.advance()?);
        state.status = if state.storage_error.is_some() {
            HubConnectionStatus::Error
        } else {
            HubConnectionStatus::Disconnected
        };
        Ok(state.projection())
    }

    /// Window close and app exit always invalidate pending work before revoking the registration.
    pub async fn shutdown(&self) {
        let client = {
            let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
            let client = state.advance().ok().flatten();
            state.status = HubConnectionStatus::Disconnected;
            client
        };
        if let Some(client) = client {
            client.disconnect().await;
        }
    }

    pub async fn refresh(
        &self,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        let (client, generation) = {
            let state = self.inner.state.lock().expect("Hub state lock poisoned");
            state.check_generation(&expected_connection_generation)?;
            (
                state.client.clone().ok_or(HubError::Unavailable)?,
                state.generation,
            )
        };
        let result = client.catalog().await;
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        state.check_generation(&generation.to_string())?;
        match result {
            Ok(catalog) => {
                // An explicit refresh and the heartbeat can finish projection out of order.
                // The HTTP client already validated these serialized observations; retain the
                // newer accepted catalog instead of treating delayed delivery as server rollback.
                if state
                    .catalog
                    .as_ref()
                    .is_some_and(|current| current.revision > catalog.revision)
                {
                    return Ok(state.projection());
                }
                if let Some(previous) = &state.catalog {
                    catalog.diff(previous)?;
                }
                state.catalog = Some(catalog);
                state.error = None;
                Ok(state.projection())
            }
            Err(error) => {
                state.error = Some(error);
                state.status = HubConnectionStatus::Stale;
                state.confirmed = [None, None];
                state.cancellation.cancel();
                Self::revoke(state.client.take());
                Err(error)
            }
        }
    }

    pub async fn save_review(
        &self,
        context: HubReviewContext,
        selection: HubSelection,
        expected_hub_id: String,
        expected_catalog_revision: CatalogRevision,
        expected_settings_revision: String,
        expected_connection_generation: String,
    ) -> Result<HubConnectionProjection, HubError> {
        let _context_owner = self.inner.reviews[context.index()].lock().await;
        let (client, review, generation) = {
            let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
            state.check_generation(&expected_connection_generation)?;
            state.check_settings(&expected_settings_revision)?;
            if state.active[context.index()].is_some() {
                return Err(HubError::RouteBusy);
            }
            if state.status != HubConnectionStatus::Connected {
                return Err(HubError::Unavailable);
            }
            let client = state.client.clone().ok_or(HubError::Unavailable)?;
            let catalog = state.catalog.as_ref().ok_or(HubError::Unavailable)?;
            let review = ReviewedHubSelection::review(
                catalog,
                &expected_hub_id,
                expected_catalog_revision,
                selection,
            )?;
            let mut proposed = state.settings.clone();
            context.set(&mut proposed, review.clone());
            // Local durability precedes remote confirmation. Failure never rolls a newer save
            // back; the durable choice remains visibly unconfirmed and can be explicitly retried.
            state.settings = self.persisted_store()?.save(&proposed)?;
            state.confirmed[context.index()] = None;
            (client, review, state.generation)
        };
        let result = client.review(context, &review).await;
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        state.check_generation(&generation.to_string())?;
        if context.review(&state.settings).as_ref() != Some(&review) {
            return Err(HubError::SettingsChanged);
        }
        match result {
            Ok(()) => {
                review.check_admission(state.catalog.as_ref().ok_or(HubError::Unavailable)?)?;
                if state.status != HubConnectionStatus::Connected {
                    return Err(HubError::Unavailable);
                }
                state.confirmed[context.index()] = Some(review);
                state.error = None;
                Ok(state.projection())
            }
            Err(error) => {
                state.record_review_rejection(review.reviewed_revision, error);
                Err(error)
            }
        }
    }

    fn start_heartbeat(&self, generation: u64, interval_ms: u64, cancellation: CancellationToken) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {}
                }
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let service = HubConnection { inner };
                let (client, previous_revision, activity) = {
                    let state = service.inner.state.lock().expect("Hub state lock poisoned");
                    if state.generation != generation {
                        break;
                    }
                    let Some(client) = state.client.clone() else {
                        break;
                    };
                    (
                        client,
                        state.catalog.as_ref().map(|catalog| catalog.revision),
                        if state
                            .active
                            .iter()
                            .flatten()
                            .any(|active| active.display.phase == "running")
                        {
                            "running"
                        } else if state.active.iter().any(Option::is_some) {
                            "waiting"
                        } else {
                            "idle"
                        },
                    )
                };
                let result = client.heartbeat(activity).await;
                match result {
                    Ok(revision)
                        if previous_revision.is_some_and(|previous| revision < previous) =>
                    {
                        service
                            .mark_stale(generation, HubError::RevisionRollback)
                            .await;
                        break;
                    }
                    Ok(revision) if Some(revision) != previous_revision => {
                        let needs_refresh = {
                            let mut state =
                                service.inner.state.lock().expect("Hub state lock poisoned");
                            if state.generation != generation {
                                break;
                            }
                            if state
                                .catalog
                                .as_ref()
                                .is_none_or(|catalog| revision > catalog.revision)
                            {
                                state.confirmed = [None, None];
                                state.error = Some(HubError::ReviewRequired);
                                true
                            } else {
                                false
                            }
                        };
                        // Invalidate review immediately, then fetch the canonical catalog:
                        // presence revision alone is not authority for its model contents.
                        if needs_refresh && service.refresh(generation.to_string()).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        service.mark_stale(generation, error).await;
                        break;
                    }
                }
            }
        });
    }

    async fn mark_stale(&self, generation: u64, error: HubError) {
        let mut state = self.inner.state.lock().expect("Hub state lock poisoned");
        if state.generation != generation {
            return;
        }
        state.error = Some(error);
        state.status = HubConnectionStatus::Stale;
        state.confirmed = [None, None];
        state.cancellation.cancel();
        Self::revoke(state.client.take());
    }
}

#[cfg(test)]
mod tests;
