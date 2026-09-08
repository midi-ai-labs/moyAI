use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    CatalogRevision, HubCatalog, HubError, HubReviewContext, ReviewedHubSelection, valid_id,
};
use crate::config::ProviderEndpoint;

const MAX_CATALOG_BYTES: usize = 1024 * 1024;

pub(super) fn validated_endpoint(endpoint: &str) -> Result<reqwest::Url, HubError> {
    if endpoint.len() > 2048 {
        return Err(HubError::InvalidConnection);
    }
    let endpoint = ProviderEndpoint::parse(endpoint).map_err(|_| HubError::InvalidConnection)?;
    let url = reqwest::Url::parse(endpoint.as_str()).map_err(|_| HubError::InvalidConnection)?;
    if url.path() != "/"
        || (url.scheme() == "http"
            && !url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }))
    {
        return Err(HubError::InvalidConnection);
    }
    Ok(url)
}

/// The ID and credential are scoped to one Hub server process. Neither is serializable.
#[derive(Clone)]
pub(super) struct RegisteredHubClient {
    connection: HubCatalogClient,
    id: String,
    pub hub_id: String,
    pub registered_revision: CatalogRevision,
    pub heartbeat_interval_ms: u64,
}

#[derive(Deserialize)]
struct Registration {
    id: String,
    client_token: String,
    hub_id: String,
    revision: CatalogRevision,
    heartbeat_interval_ms: u64,
    identity_scope: String,
}

#[derive(Deserialize)]
struct Heartbeat {
    id: String,
    revision: CatalogRevision,
}

#[derive(Deserialize)]
struct ReviewAck {
    id: String,
    context: HubReviewContext,
    reviewed_revision: CatalogRevision,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum PreparedRequest {
    Waiting {
        reason: String,
        retry_after_ms: u64,
    },
    Ready {
        lease_id: String,
        permit_id: String,
        logical_model_id: String,
        gateway_base_url: String,
        request_token: String,
        expires_at_ms: String,
        provider_profile: String,
        provider_api_mode: String,
    },
}

/// An authenticated read-only connection to the Hub management-preview catalog API.
/// Runtime credentials are not serializable and are never included in Debug output.
#[derive(Clone)]
pub struct HubCatalogClient {
    http: reqwest::Client,
    managed_http: Option<crate::device_network::ManagedHubHttp>,
    catalog_url: reqwest::Url,
    authorization: HeaderValue,
    deadline: Duration,
    observed: Arc<tokio::sync::Mutex<Option<HubCatalog>>>,
}

impl std::fmt::Debug for HubCatalogClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HubCatalogClient")
            .field("connection", &"<redacted>")
            .field("deadline", &self.deadline)
            .finish()
    }
}

impl HubCatalogClient {
    pub(super) async fn device_session(
        endpoint: &str,
        http: crate::device_network::ManagedHubHttp,
    ) -> Result<(RegisteredHubClient, Option<super::HubSelection>), HubError> {
        #[derive(Deserialize)]
        struct Receipt {
            id: String,
            client_token: String,
            hub_id: String,
            revision: CatalogRevision,
            heartbeat_interval_ms: u64,
            identity_scope: String,
            default_selection: Option<super::HubSelection>,
        }
        let mut url = validated_endpoint(endpoint)?;
        url.set_path("/v1/network/model-session");
        let receipt: Receipt = tokio::time::timeout(Duration::from_secs(10), async {
            let lease = http.acquire().await;
            let response = lease
                .http
                .post(url)
                .json(&serde_json::json!({}))
                .send()
                .await
                .map_err(|_| HubError::Unavailable)?;
            if !response.status().is_success() {
                return Err(HubError::Unavailable);
            }
            let mut bytes = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|_| HubError::Unavailable)?;
                if bytes.len().saturating_add(chunk.len()) > 65536 {
                    return Err(HubError::ResponseLimit);
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| HubError::InvalidCatalog)
        })
        .await
        .map_err(|_| HubError::Deadline)??;
        if receipt.identity_scope != "device_session"
            || !valid_id(&receipt.id)
            || !valid_id(&receipt.hub_id)
            || !(100..=60000).contains(&receipt.heartbeat_interval_ms)
        {
            return Err(HubError::InvalidCatalog);
        }
        let mut connection = Self::new(endpoint, &receipt.client_token, 10000)?;
        connection.http = http.snapshot();
        connection.managed_http = Some(http);
        let selection = receipt.default_selection;
        if let Some(selection) = &selection {
            selection.validate_shape()?;
        }
        Ok((
            RegisteredHubClient {
                connection,
                id: receipt.id,
                hub_id: receipt.hub_id,
                registered_revision: receipt.revision,
                heartbeat_interval_ms: receipt.heartbeat_interval_ms,
            },
            selection,
        ))
    }
    pub fn new(endpoint: &str, token: &str, deadline_ms: u64) -> Result<Self, HubError> {
        if !(1..=60_000).contains(&deadline_ms)
            || endpoint.len() > 2048
            || !(32..=256).contains(&token.len())
            || !token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            })
        {
            return Err(HubError::InvalidConnection);
        }
        let mut url = validated_endpoint(endpoint)?;
        url.set_path("/v1/catalog");
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| HubError::InvalidConnection)?;
        authorization.set_sensitive(true);
        let deadline = Duration::from_millis(deadline_ms);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(deadline)
            .build()
            .map_err(|_| HubError::InvalidConnection)?;
        Ok(Self {
            http,
            managed_http: None,
            catalog_url: url,
            authorization,
            deadline,
            observed: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, HubError> {
        tokio::time::timeout(self.deadline, async {
            let lease = match &self.managed_http {
                Some(http) => Some(http.acquire().await),
                None => None,
            };
            self.post_with_http(
                path,
                body,
                lease
                    .as_ref()
                    .map(|lease| &lease.http)
                    .unwrap_or(&self.http),
            )
            .await
        })
        .await
        .map_err(|_| HubError::Deadline)?
    }
    async fn post_with_http<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
        http: &reqwest::Client,
    ) -> Result<T, HubError> {
        let mut url = self.catalog_url.clone();
        url.set_path(path);
        tokio::time::timeout(self.deadline, async {
            let response = http
                .post(url)
                .header(AUTHORIZATION, self.authorization.clone())
                .json(body)
                .send()
                .await
                .map_err(|_| HubError::Unavailable)?;
            let status = response.status();
            if status.is_redirection() {
                return Err(HubError::Redirect);
            }
            if matches!(status.as_u16(), 401 | 403) {
                return Err(HubError::Unauthorized);
            }
            if response
                .content_length()
                .is_some_and(|bytes| bytes > 16_384)
            {
                return Err(HubError::ResponseLimit);
            }
            let mut bytes = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|_| HubError::Unavailable)?;
                if bytes.len().saturating_add(chunk.len()) > 16_384 {
                    return Err(HubError::ResponseLimit);
                }
                bytes.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                #[derive(Deserialize)]
                struct Failure {
                    error: String,
                }
                let failure = serde_json::from_slice::<Failure>(&bytes).ok();
                return Err(
                    match failure.as_ref().map(|failure| failure.error.as_str()) {
                        Some("review_required") => HubError::ReviewRequired,
                        Some("different_hub") => HubError::DifferentHub,
                        Some("invalid_selection") => HubError::InvalidSelection,
                        Some("model_removed") => HubError::ModelRemoved,
                        Some("capability_mismatch") => HubError::CapabilityMismatch,
                        Some("active_turn") => HubError::RouteBusy,
                        Some(
                            "gateway_unavailable" | "gateway_quarantined" | "upstream_uncertain",
                        ) => HubError::GatewayUnavailable,
                        _ => HubError::Unavailable,
                    },
                );
            }
            serde_json::from_slice(&bytes).map_err(|_| HubError::InvalidCatalog)
        })
        .await
        .map_err(|_| HubError::Deadline)?
    }

    /// Consume the bootstrap connection. Only the freshly issued client token survives.
    pub(super) async fn register(self, label: &str) -> Result<RegisteredHubClient, HubError> {
        let registration: Registration = self
            .post("/v1/clients/register", &serde_json::json!({"label":label}))
            .await?;
        if !valid_id(&registration.id)
            || !valid_id(&registration.hub_id)
            || registration.identity_scope != "server_session"
            || !(100..=60_000).contains(&registration.heartbeat_interval_ms)
        {
            return Err(HubError::InvalidCatalog);
        }
        let mut endpoint = self.catalog_url.clone();
        endpoint.set_path("/");
        let connection = Self::new(
            endpoint.as_str(),
            &registration.client_token,
            self.deadline.as_millis() as u64,
        )?;
        Ok(RegisteredHubClient {
            connection,
            id: registration.id,
            hub_id: registration.hub_id,
            registered_revision: registration.revision,
            heartbeat_interval_ms: registration.heartbeat_interval_ms,
        })
    }

    pub async fn catalog(&self) -> Result<HubCatalog, HubError> {
        tokio::time::timeout(self.deadline, async {
            // Serialize observations across clones so a slow response cannot replace a newer
            // accepted catalog. Waiting for this owner is covered by the same total deadline.
            let mut observed = self.observed.lock().await;
            let lease = match &self.managed_http {
                Some(http) => Some(http.acquire().await),
                None => None,
            };
            let http = lease
                .as_ref()
                .map(|lease| &lease.http)
                .unwrap_or(&self.http);
            let response = http
                .get(self.catalog_url.clone())
                .header(AUTHORIZATION, self.authorization.clone())
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|_| HubError::Unavailable)?;
            let status = response.status();
            if status.is_redirection() {
                return Err(HubError::Redirect);
            }
            if matches!(status.as_u16(), 401 | 403) {
                return Err(HubError::Unauthorized);
            }
            if !status.is_success() {
                return Err(HubError::Unavailable);
            }
            if response
                .content_length()
                .is_some_and(|bytes| bytes > MAX_CATALOG_BYTES as u64)
            {
                return Err(HubError::ResponseLimit);
            }
            let mut body = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|_| HubError::Unavailable)?;
                if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
                    return Err(HubError::ResponseLimit);
                }
                body.extend_from_slice(&chunk);
            }
            let catalog: HubCatalog =
                serde_json::from_slice(&body).map_err(|_| HubError::InvalidCatalog)?;
            catalog.validate()?;
            if let Some(previous) = observed.as_ref() {
                catalog.diff(previous)?;
            }
            *observed = Some(catalog.clone());
            Ok(catalog)
        })
        .await
        .map_err(|_| HubError::Deadline)?
    }
}

impl RegisteredHubClient {
    pub async fn prepare(
        &self,
        context: HubReviewContext,
        turn_id: &str,
        request_id: &str,
        review: &ReviewedHubSelection,
    ) -> Result<PreparedRequest, HubError> {
        self.connection
            .post(
                "/v1/requests/prepare",
                &serde_json::json!({
                    "id":self.id, "context":context, "turn_id":turn_id, "request_id":request_id,
                    "expected_hub_id":review.hub_id, "reviewed_revision":review.reviewed_revision,
                }),
            )
            .await
    }

    pub async fn close_turn(&self, context: HubReviewContext, turn_id: &str, cancelled: bool) {
        let path = if cancelled {
            "/v1/turns/cancel"
        } else {
            "/v1/turns/finish"
        };
        let _: Result<serde_json::Value, _> = self
            .connection
            .post(
                path,
                &serde_json::json!({
                    "id":self.id,"context":context,"turn_id":turn_id,
                }),
            )
            .await;
    }
    pub async fn catalog(&self) -> Result<HubCatalog, HubError> {
        let catalog = self.connection.catalog().await?;
        if catalog.hub_id != self.hub_id {
            return Err(HubError::DifferentHub);
        }
        if catalog.revision < self.registered_revision {
            return Err(HubError::RevisionRollback);
        }
        Ok(catalog)
    }

    pub async fn heartbeat(&self, activity: &'static str) -> Result<CatalogRevision, HubError> {
        let heartbeat: Heartbeat = self
            .connection
            .post(
                "/v1/clients/heartbeat",
                &serde_json::json!({"id":self.id,"activity":activity}),
            )
            .await?;
        if heartbeat.id != self.id {
            return Err(HubError::InvalidCatalog);
        }
        Ok(heartbeat.revision)
    }

    pub async fn review(
        &self,
        context: HubReviewContext,
        review: &ReviewedHubSelection,
    ) -> Result<(), HubError> {
        let ack: ReviewAck = self
            .connection
            .post(
                "/v1/clients/review",
                &serde_json::json!({
                    "id":self.id, "context":context, "expected_hub_id":review.hub_id,
                    "reviewed_revision":review.reviewed_revision, "selection":review.selection,
                }),
            )
            .await?;
        if ack.id != self.id
            || ack.context != context
            || ack.reviewed_revision != review.reviewed_revision
        {
            return Err(HubError::InvalidCatalog);
        }
        Ok(())
    }

    pub async fn disconnect(&self) {
        // Revocation is best effort: local disconnection is immediate even if Hub is gone.
        let _: Result<serde_json::Value, _> = self
            .connection
            .post("/v1/clients/disconnect", &serde_json::json!({"id":self.id}))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn device_model_session_and_turn_survive_http_identity_rotation_without_replay() {
        use axum::{Json, http::HeaderMap, routing::post};
        use serde_json::{Value, json};
        let calls = Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let registration_calls = calls.clone();
        let prepare_calls = calls.clone();
        let app = Router::new()
            .route("/v1/network/model-session", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let calls = registration_calls.clone(); async move {
                    calls.lock().unwrap().push((headers["x-device-identity"].to_str().unwrap().into(), body));
                    Json(json!({"id":"stable-model-client","client_token":"model-token-01234567890123456789012345","hub_id":"hub","revision":"1","heartbeat_interval_ms":1000,"identity_scope":"device_session","default_selection":null}))
                }
            }))
            .route("/v1/requests/prepare", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let calls = prepare_calls.clone(); async move {
                    assert_eq!(headers["authorization"], "Bearer model-token-01234567890123456789012345");
                    calls.lock().unwrap().push((headers["x-device-identity"].to_str().unwrap().into(), body));
                    Json(json!({"state":"waiting","reason":"busy","retry_after_ms":1000}))
                }
            }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = |epoch: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert("x-device-identity", HeaderValue::from_static(epoch));
            reqwest::Client::builder()
                .no_proxy()
                .default_headers(headers)
                .build()
                .unwrap()
        };
        let transport = crate::device_network::ManagedHubHttp::new(http("old"));
        let (client, _) = HubCatalogClient::device_session(&endpoint, transport.clone())
            .await
            .unwrap();
        let review = ReviewedHubSelection {
            hub_id: "hub".into(),
            reviewed_revision: CatalogRevision(1),
            selection: super::super::HubSelection {
                allowed_model_ids: ["logical".into()].into(),
                preferred_model_id: "logical".into(),
                required_capabilities: ["tools".into()].into(),
                wait_policy: super::super::HubWaitPolicy::WaitForPreferred,
                affinity_turns: 1,
            },
        };
        client
            .prepare(HubReviewContext::Main, "same-turn", "request-1", &review)
            .await
            .unwrap();
        let rotation = transport.try_rotate().unwrap();
        rotation.replace(http("new"));
        drop(rotation);
        client
            .prepare(HubReviewContext::Main, "same-turn", "request-2", &review)
            .await
            .unwrap();
        server.abort();
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            3,
            "rotation must not register another session or replay a model request"
        );
        assert_eq!(
            calls
                .iter()
                .map(|(epoch, _)| epoch.as_str())
                .collect::<Vec<_>>(),
            vec!["old", "old", "new"]
        );
        for (_, request) in &calls[1..] {
            assert_eq!(request["id"], "stable-model-client");
            assert_eq!(request["turn_id"], "same-turn");
        }
        assert_eq!(calls[1].1["request_id"], "request-1");
        assert_eq!(calls[2].1["request_id"], "request-2");
    }

    #[test]
    fn accepts_the_management_servers_exact_token_alphabet_and_length_range() {
        for length in [32, 256] {
            assert!(
                HubCatalogClient::new("http://127.0.0.1:9470", &"~".repeat(length), 1000).is_ok()
            );
        }
        for length in [0, 31, 257] {
            assert_eq!(
                HubCatalogClient::new("http://127.0.0.1:9470", &"x".repeat(length), 1000)
                    .unwrap_err(),
                HubError::InvalidConnection
            );
        }
    }

    #[tokio::test]
    async fn repeated_observation_rejects_drift_and_preserves_the_last_accepted_owner() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let app = Router::new().route("/v1/catalog", get(move || {
                let call = observed_calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    axum::Json(serde_json::json!({
                        "hub_id": if call == 3 { "different" } else { "hub-one" },
                        "software_version": if call == 1 { "changed-without-revision" } else { "0.1.0" },
                        "revision": if call == 2 { "1" } else { "2" },
                        "models": [], "changes": []
                    }))
                }
            }));
            axum::serve(listener, app).await.unwrap();
        });
        let client =
            HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 1000).unwrap();
        let original = client.catalog().await.unwrap();
        let clone = client.clone();
        let drift = clone.catalog().await;
        let rollback = client.catalog().await;
        let replaced = clone.catalog().await;
        let restored = client.catalog().await;
        server.abort();
        assert_eq!(drift, Err(HubError::InvalidCatalog));
        assert_eq!(rollback, Err(HubError::RevisionRollback));
        assert_eq!(replaced, Err(HubError::DifferentHub));
        assert_eq!(restored, Ok(original));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }
}
