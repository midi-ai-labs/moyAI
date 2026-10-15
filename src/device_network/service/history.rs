//! Explicit, read-only Hub administrator queries over the existing authenticated
//! Desktop-to-Hub connection. This path cannot start an agent or read an arbitrary file.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{DeviceClient, DeviceError, DeviceNetworkService};
use crate::remote_agent::{McpHistoryDirection, RemoteJobService};
use crate::runtime::SystemClock;

const HUB_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRequests {
    requests: Vec<HistoryRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRequest {
    request_id: String,
    device_id: String,
    direction: McpHistoryDirection,
    offset: usize,
    anchor: Option<String>,
    id: Option<String>,
    expires_at_ms: u64,
}

impl HistoryRequest {
    fn validate(&self, device: &str, now_ms: u64) -> Result<(), DeviceError> {
        if self.request_id.parse::<ulid::Ulid>().is_err()
            || self.device_id != device
            || self.expires_at_ms <= now_ms
            || self.offset > i64::MAX as usize
            || self
                .anchor
                .as_ref()
                .is_some_and(|anchor| anchor.len() > 128)
            || self
                .id
                .as_ref()
                .is_some_and(|id| id.parse::<ulid::Ulid>().is_err())
            || (self.id.is_some() && (self.offset != 0 || self.anchor.is_some()))
        {
            return Err(DeviceError::InvalidResponse);
        }
        Ok(())
    }
}

impl DeviceNetworkService {
    fn history_connection_current(&self, client: &DeviceClient, generation: u64) -> bool {
        self.inner.state.lock().is_ok_and(|state| {
            !state.closing
                && state.generation == generation
                && state.settings.device_id.as_deref() == Some(client.device_id.as_str())
                && matches!(state.status, "active" | "stopped")
        })
    }

    /// Called only after the ordinary presence and self-identity checks succeed.
    /// A missing endpoint on an older Hub does not interrupt normal connectivity.
    pub(super) async fn serve_history_requests(&self, client: &DeviceClient, generation: u64) {
        if !self.history_connection_current(client, generation) {
            return;
        }
        let Ok(pending) = client
            .request::<HistoryRequests>("/v1/network/history/requests", None)
            .await
        else {
            return;
        };
        if pending.requests.len() > 1 {
            return;
        }
        for query in pending.requests {
            let now = SystemClock::now_ms().max(0) as u64;
            if query.validate(&client.device_id, now).is_err()
                || !self.history_connection_current(client, generation)
            {
                return;
            }
            let body = history_response(&self.inner.jobs, &query).await;
            if !self.history_connection_current(client, generation)
                || query.expires_at_ms <= SystemClock::now_ms().max(0) as u64
            {
                return;
            }
            // No model text, arbitrary target or file path is accepted as a command.
            // The selected local history is returned only to this authenticated Hub.
            let _: Result<Value, _> = client
                .request("/v1/network/history/results", Some(&body))
                .await;
        }
    }
}

async fn history_response(jobs: &RemoteJobService, query: &HistoryRequest) -> Value {
    let response = match query.id.as_deref() {
        Some(id) => jobs
            .history_detail_for_hub(query.direction, id)
            .await
            .map(serde_json::to_value),
        None => jobs
            .history_page(query.direction, query.offset, 20, query.anchor.as_deref())
            .await
            .map(serde_json::to_value),
    };
    let body = match response {
        Ok(Ok(value)) => json!({"request_id":query.request_id,"response":value,"error":null}),
        _ => {
            return json!({"request_id":query.request_id,"response":null,"error":"この端末の対象履歴を取得できません。"});
        }
    };
    if serde_json::to_vec(&body).map_or(true, |bytes| bytes.len() > HUB_RESPONSE_BYTES) {
        json!({"request_id":query.request_id,"response":null,"error":"履歴がHubへの転送上限を超えています。実行端末でMarkdownを保存してください。"})
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> HistoryRequest {
        HistoryRequest {
            request_id: ulid::Ulid::new().to_string(),
            device_id: "device-a".into(),
            direction: McpHistoryDirection::Execution,
            offset: 0,
            anchor: None,
            id: None,
            expires_at_ms: 100,
        }
    }

    #[test]
    fn hub_history_query_is_read_only_exact_audience_and_unexpired() {
        let mut request = query();
        assert!(request.validate("device-a", 99).is_ok());
        assert!(request.validate("device-b", 99).is_err());
        assert!(request.validate("device-a", 100).is_err());
        request.id = Some("../private-key.pem".into());
        assert!(request.validate("device-a", 0).is_err());
        let invalid = json!({"request_id":request.request_id,"device_id":"device-a","direction":"execute_shell","offset":0,"anchor":null,"id":null,"expires_at_ms":100});
        assert!(serde_json::from_value::<HistoryRequest>(invalid).is_err());
    }

    #[tokio::test]
    async fn hub_history_reads_local_offline_store_and_returns_no_cross_target_fallback() {
        let (_root, service) = super::super::lifecycle_tests::fixture().await;
        let mut request = query();
        let page = history_response(&service.inner.jobs, &request).await;
        assert_eq!(page["request_id"], request.request_id);
        assert_eq!(page["response"]["rows"], json!([]));
        assert!(page["error"].is_null());
        request.id = Some(ulid::Ulid::new().to_string());
        let missing = history_response(&service.inner.jobs, &request).await;
        assert!(missing["response"].is_null());
        assert!(missing["error"].is_string());
    }
}
