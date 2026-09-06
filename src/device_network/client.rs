use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{DeviceError, DeviceIdentity, SharedHubConfig, stable_id};

#[derive(Clone)]
pub(crate) struct DeviceClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    pub(crate) device_id: String,
}
impl std::fmt::Debug for DeviceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceClient(<authenticated>)")
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollmentReceipt {
    pub hub_id: String,
    pub device_id: String,
    pub label: String,
    pub certificate_pem: String,
    pub ca_certificate_pem: String,
    pub certificate_sha256: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryPeer {
    pub device_id: String,
    pub label: String,
    pub profile_id: String,
    pub name: String,
    pub endpoint: String,
    pub mode: String,
    pub scope_id: String,
    pub certificate_pem: String,
    pub certificate_sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Directory {
    pub hub_id: String,
    pub revision: String,
    pub peers: Vec<DirectoryPeer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantClaims {
    pub hub_id: String,
    pub origin_device_id: String,
    pub actor_device_id: String,
    pub audience_device_id: String,
    pub profile_id: String,
    pub mode: String,
    pub scope_id: String,
    pub root_task_id: String,
    pub request_key: String,
    pub parent_job_id: Option<String>,
    pub depth: u8,
    pub device_path: Vec<String>,
}

/// Opaque credentials never implement Serialize/Debug or enter model configuration.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceGrant {
    pub grant_id: String,
    pub token: String,
    pub expires_at_ms: u64,
    pub claims: GrantClaims,
    pub peer: DirectoryPeer,
}

/// Constructed only after receiver-authenticated Hub introspection.
#[derive(Clone)]
pub struct VerifiedGrant {
    pub(crate) grant_id: String,
    pub(crate) claims: GrantClaims,
}
impl VerifiedGrant {
    pub fn claims(&self) -> &GrantClaims {
        &self.claims
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntrospectionReply {
    active: bool,
    grant_id: String,
    expires_at_ms: u64,
    claims: GrantClaims,
}

impl IntrospectionReply {
    fn verified(self, receiver_device_id: &str) -> Result<VerifiedGrant, DeviceError> {
        // The first direct delegation is depth 1; Hub permits up to three onward hops.
        if !self.active
            || self.claims.audience_device_id != receiver_device_id
            || self.claims.depth > 4
            || self.claims.device_path.len() > 5
            || self.expires_at_ms == 0
        {
            return Err(DeviceError::GrantDenied);
        }
        Ok(VerifiedGrant {
            grant_id: self.grant_id,
            claims: self.claims,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelfStatus {
    pub hub_id: String,
    pub device_id: String,
    pub label: String,
    pub groups: Vec<String>,
    pub revision: String,
    pub certificate_sha256: String,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub cancelled_lineages: Vec<CancelledLineage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelledLineage {
    pub origin_device_id: String,
    pub root_task_id: String,
}

impl DeviceClient {
    pub(crate) fn new(
        config: &SharedHubConfig,
        identity: Option<(&DeviceIdentity, &str)>,
        device_id: String,
    ) -> Result<Self, DeviceError> {
        let endpoint = config.validate()?;
        let http = crate::mcp_publish::tls::managed_hub_http(
            identity.map(|(key, certificate)| (certificate, key.private_key_pem())),
            &config.ca_certificate_pem,
        )
        .map_err(|_| DeviceError::InvalidConfiguration)?;
        Ok(Self {
            http,
            endpoint,
            device_id,
        })
    }
    pub(crate) fn http(&self) -> reqwest::Client {
        self.http.clone()
    }
    pub(crate) fn endpoint(&self) -> String {
        self.endpoint.as_str().trim_end_matches('/').to_string()
    }

    pub(crate) async fn route_ip(&self) -> Result<Ipv4Addr, DeviceError> {
        let host = self
            .endpoint
            .host_str()
            .ok_or(DeviceError::InvalidConfiguration)?;
        let port = self
            .endpoint
            .port_or_known_default()
            .ok_or(DeviceError::InvalidConfiguration)?;
        let addresses = tokio::time::timeout(
            Duration::from_secs(4),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| DeviceError::Unavailable)?
        .map_err(|_| DeviceError::Unavailable)?;
        for address in addresses.take(16) {
            let SocketAddr::V4(address) = address else {
                continue;
            };
            let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
                .await
                .map_err(|_| DeviceError::Unavailable)?;
            socket
                .connect(address)
                .await
                .map_err(|_| DeviceError::Unavailable)?;
            if let SocketAddr::V4(local) =
                socket.local_addr().map_err(|_| DeviceError::Unavailable)?
            {
                if !local.ip().is_unspecified() {
                    return Ok(*local.ip());
                }
            }
        }
        Err(DeviceError::Unavailable)
    }

    pub(crate) async fn request<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, DeviceError> {
        let mut url = self.endpoint.clone();
        url.set_path(path);
        tokio::time::timeout(Duration::from_secs(10), async {
            let request = match body {
                Some(body) => self.http.post(url).json(body),
                None => self.http.get(url),
            };
            let response = request.send().await.map_err(|_| DeviceError::Unavailable)?;
            let status = response.status();
            if status.is_redirection() {
                return Err(DeviceError::InvalidResponse);
            }
            let mut bytes = Vec::new();
            if response
                .content_length()
                .is_some_and(|length| length > 4 * 1024 * 1024)
            {
                return Err(DeviceError::InvalidResponse);
            }
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| DeviceError::Unavailable)?;
                if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                    return Err(DeviceError::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                #[derive(Deserialize)]
                struct Failure {
                    error: String,
                }
                let code = serde_json::from_slice::<Failure>(&bytes)
                    .map(|value| value.error)
                    .unwrap_or_default();
                return Err(match code.as_str() {
                    "enrollment_denied" => DeviceError::EnrollmentDenied,
                    "device_revoked" | "unauthorized" => DeviceError::Revoked,
                    "policy_denied" => DeviceError::PolicyDenied,
                    "grant_denied" => DeviceError::GrantDenied,
                    _ => DeviceError::Unavailable,
                });
            }
            serde_json::from_slice(&bytes).map_err(|_| DeviceError::InvalidResponse)
        })
        .await
        .map_err(|_| DeviceError::Unavailable)?
    }
    pub(crate) async fn enroll(
        &self,
        code: &str,
        csr_pem: &str,
    ) -> Result<EnrollmentReceipt, DeviceError> {
        self.request(
            "/v1/network/enroll",
            Some(&json!({"code":code,"csr_pem":csr_pem})),
        )
        .await
    }
    pub(crate) async fn renew(&self, csr_pem: &str) -> Result<EnrollmentReceipt, DeviceError> {
        self.request("/v1/network/renew", Some(&json!({"csr_pem":csr_pem})))
            .await
    }
    pub(crate) async fn self_status(&self) -> Result<SelfStatus, DeviceError> {
        self.request("/v1/network/self", None).await
    }
    pub(crate) async fn presence(&self) -> Result<(), DeviceError> {
        let _: serde_json::Value = self
            .request("/v1/network/presence", Some(&json!({})))
            .await?;
        Ok(())
    }
    pub(crate) async fn directory(&self, parent: Option<&str>) -> Result<Directory, DeviceError> {
        let directory: Directory = if let Some(parent) = parent {
            self.request(
                "/v1/network/directory/query",
                Some(&json!({"parent_grant_id":parent})),
            )
            .await?
        } else {
            self.request("/v1/network/directory", None).await?
        };
        if directory.peers.len() > 1024
            || !stable_id(&directory.hub_id)
            || super::canonical_revision(&directory.revision).is_none()
        {
            return Err(DeviceError::InvalidResponse);
        }
        let mut keys = std::collections::BTreeSet::new();
        for peer in &directory.peers {
            if !stable_id(&peer.device_id)
                || !stable_id(&peer.profile_id)
                || !keys.insert((&peer.device_id, &peer.profile_id))
                || peer.endpoint.len() > 2048
                || peer.label.len() > 256
                || peer.name.len() > 256
                || peer.certificate_pem.len() > 65536
                || !matches!(peer.mode.as_str(), "agent" | "read_tools")
            {
                return Err(DeviceError::InvalidResponse);
            }
            let url =
                reqwest::Url::parse(&peer.endpoint).map_err(|_| DeviceError::InvalidResponse)?;
            if url.scheme() != "https"
                || url.path() != "/mcp"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(DeviceError::InvalidResponse);
            }
        }
        Ok(directory)
    }
    pub(crate) async fn publish(
        &self,
        profile_id: &str,
        name: &str,
        endpoint: &str,
        scope_id: &str,
        enabled: bool,
    ) -> Result<(), DeviceError> {
        let _: serde_json::Value = self.request("/v1/network/publish", Some(&json!({
            "profile_id":profile_id,"name":name,"endpoint":endpoint,"mode":"agent","scope_id":scope_id,"enabled":enabled
        }))).await?;
        Ok(())
    }
    pub(crate) async fn grant(
        &self,
        peer: &DirectoryPeer,
        root: &str,
        key: &str,
        parent: Option<(&str, &str)>,
        action: &str,
    ) -> Result<DeviceGrant, DeviceError> {
        self.request("/v1/network/grants", Some(&json!({"audience_device_id":peer.device_id,
            "profile_id":peer.profile_id,"root_task_id":root,"request_key":key,
            "parent_grant_id":parent.map(|value|value.0),"parent_job_id":parent.map(|value|value.1),"action":action}))).await
    }
    pub(crate) async fn introspect(
        &self,
        token: &str,
        actor_certificate_sha256: &str,
        action: &str,
    ) -> Result<VerifiedGrant, DeviceError> {
        let reply: IntrospectionReply = self
            .request(
                "/v1/network/introspect",
                Some(&json!({"token":token,
            "actor_certificate_sha256":actor_certificate_sha256,"action":action})),
            )
            .await?;
        reply.verified(&self.device_id)
    }
    pub(crate) async fn cancel_lineage(&self, root: &str) -> Result<(), DeviceError> {
        let _: serde_json::Value = self
            .request(
                "/v1/network/cancel-lineage",
                Some(&json!({"root_task_id":root})),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_accepts_three_onward_hops_and_rejects_overbound_paths() {
        let receipt = json!({
            "active":true,"grant_id":"grant-4","expires_at_ms":123456,
            "claims":{
                "hub_id":"hub","origin_device_id":"win00","actor_device_id":"win19",
                "audience_device_id":"win20","profile_id":"profile","mode":"agent",
                "scope_id":"scope","root_task_id":"task","request_key":"key",
                "parent_job_id":"parent-job","depth":4,
                "device_path":["win00","win17","win18","win19","win20"]
            }
        });
        let accepted: IntrospectionReply = serde_json::from_value(receipt.clone()).unwrap();
        let grant = accepted.verified("win20").unwrap();
        assert_eq!(grant.claims().depth, 4);
        assert_eq!(grant.claims().device_path.len(), 5);
        assert_eq!(grant.claims().origin_device_id, "win00");

        let mut over_depth = receipt.clone();
        over_depth["claims"]["depth"] = json!(5);
        let mut over_path = receipt.clone();
        over_path["claims"]["device_path"] =
            json!(["win00", "win16", "win17", "win18", "win19", "win20"]);
        let mut inactive = receipt.clone();
        inactive["active"] = json!(false);
        let mut wrong_audience = receipt.clone();
        wrong_audience["claims"]["audience_device_id"] = json!("win21");
        let mut invalid_expiry = receipt;
        invalid_expiry["expires_at_ms"] = json!(0);
        for rejected in [
            over_depth,
            over_path,
            inactive,
            wrong_audience,
            invalid_expiry,
        ] {
            let reply: IntrospectionReply = serde_json::from_value(rejected).unwrap();
            assert!(matches!(
                reply.verified("win20"),
                Err(DeviceError::GrantDenied)
            ));
        }
    }
}
