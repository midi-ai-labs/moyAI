//! Process-local reuse of already negotiated, exactly authority-bound MCP sessions.
//! Grants remain request scoped; this cache never owns a bearer credential.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::DeviceGrant;
use crate::config::{McpConfig, McpServerConfig};
use crate::error::ToolError;
use crate::mcp::{McpClient, McpOperationResult};

const MAX_CONNECTIONS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PeerConnectionKey([u8; 32]);

impl std::fmt::Debug for PeerConnectionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PeerConnectionKey([redacted])")
    }
}

impl PeerConnectionKey {
    pub(super) fn new(
        grant: &DeviceGrant,
        hub_url: &str,
        ca_pem: &str,
        local_certificate_pem: &str,
        local_private_key_pem: &str,
    ) -> Result<Self, ToolError> {
        // The receiver binds its session to ALL claims, including root,
        // request key and ancestry. Tokens and expiry can rotate within that
        // authority; no broader peer-only session is valid for another task.
        let claims = serde_json::to_vec(&grant.claims)
            .map_err(|_| ToolError::Message("invalid MCP peer authority".into()))?;
        let mut hash = Sha256::new();
        for value in [
            claims.as_slice(),
            hub_url.as_bytes(),
            ca_pem.as_bytes(),
            local_certificate_pem.as_bytes(),
            local_private_key_pem.as_bytes(),
            grant.peer.device_id.as_bytes(),
            grant.peer.profile_id.as_bytes(),
            grant.peer.scope_id.as_bytes(),
            grant.peer.mode.as_bytes(),
            grant.peer.endpoint.as_bytes(),
            grant.peer.certificate_pem.as_bytes(),
            grant.peer.certificate_sha256.as_bytes(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        Ok(Self(hash.finalize().into()))
    }
}

struct Entry {
    retired: AtomicBool,
    client: tokio::sync::Mutex<Option<McpClient>>,
}

#[derive(Default)]
pub(super) struct PeerConnections {
    entries: Mutex<VecDeque<(PeerConnectionKey, Arc<Entry>)>>,
}

fn unavailable() -> ToolError {
    ToolError::Message("managed MCP connection changed or is unavailable".into())
}

impl PeerConnections {
    fn entry(&self, key: PeerConnectionKey) -> Result<Arc<Entry>, ToolError> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        if let Some(index) = entries.iter().position(|(old, _)| *old == key) {
            let entry = entries.remove(index).unwrap();
            let handle = entry.1.clone();
            entries.push_back(entry);
            return Ok(handle);
        }
        if entries.len() >= MAX_CONNECTIONS {
            // A leased entry must not be evicted: a second owner could then
            // initialize the same authority while its first request is active.
            let Some(index) = entries
                .iter()
                .position(|(_, entry)| Arc::strong_count(entry) == 1)
            else {
                return Err(ToolError::Message(
                    "managed MCP connection capacity is busy".into(),
                ));
            };
            let (_, old) = entries.remove(index).unwrap();
            old.retired.store(true, Ordering::Release);
        }
        let entry = Arc::new(Entry {
            retired: AtomicBool::new(false),
            client: tokio::sync::Mutex::new(None),
        });
        entries.push_back((key, entry.clone()));
        Ok(entry)
    }

    /// Clear only process-local connections. Existing operations still pass
    /// their caller-owned checkpoint at each send; queued old leases fail.
    pub(super) fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            for (_, entry) in entries.drain(..) {
                entry.retired.store(true, Ordering::Release);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn operate(
        &self,
        key: PeerConnectionKey,
        mut server: McpServerConfig,
        fresh_token: &str,
        make_http: impl FnOnce() -> Result<reqwest::Client, ToolError>,
        name: Option<&str>,
        arguments: Value,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        checkpoint()?;
        let entry = self.entry(key)?;
        let mut cached = tokio::time::timeout(
            Duration::from_millis(server.timeout_ms),
            entry.client.lock(),
        )
        .await
        .map_err(|_| unavailable())?;
        if entry.retired.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let id = server.id.clone();
        if cached.is_none() {
            // No operation credential may survive in the cached configuration.
            server
                .headers
                .retain(|header, _| !header.eq_ignore_ascii_case("authorization"));
            *cached = Some(
                McpClient::new(McpConfig {
                    enabled: true,
                    servers: vec![server],
                })
                .with_runtime_http(&id, make_http()?),
            );
        }
        let client = cached
            .as_ref()
            .unwrap()
            .with_runtime_authorization(&id, fresh_token)?;
        let mut guarded = || {
            if entry.retired.load(Ordering::Acquire) {
                return Err(unavailable());
            }
            checkpoint()
        };
        let result = match name {
            Some(name) => client.call_tool(&id, name, arguments, &mut guarded).await,
            None => client.list_tools(&id, &mut guarded).await,
        };
        if result.is_err() {
            // Cleanup is not a replay of tools/call. Even an ambiguous effectful
            // failure is returned unchanged; only a later explicit operation
            // can negotiate again. Other peer/authority entries stay intact.
            let _ = client.close_session(&id, &mut guarded).await;
            *cached = None;
        }
        result
    }

    /// Inspect-only and terminal task sessions should be released while their
    /// current grant is available, instead of occupying receiver slots to TTL.
    pub(super) async fn retire(
        &self,
        key: PeerConnectionKey,
        fresh_token: &str,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<(), ToolError> {
        let entry = {
            let entries = self.entries.lock().map_err(|_| unavailable())?;
            entries
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, entry)| entry.clone())
        };
        let Some(entry) = entry else {
            return Ok(());
        };
        let mut cached = tokio::time::timeout(Duration::from_secs(3), entry.client.lock())
            .await
            .map_err(|_| unavailable())?;
        if entry.retired.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = if let Some(client) = cached.take() {
            let id = client.config().servers[0].id.clone();
            client
                .with_runtime_authorization(&id, fresh_token)?
                .close_session(&id, &mut checkpoint)
                .await
        } else {
            Ok(())
        };
        // Keep this empty entry if another operation already leased it. That
        // operation reinitializes under the same mutex, avoiding two owners.
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        if Arc::strong_count(&entry) == 2 {
            entries.retain(|(_, current)| !Arc::ptr_eq(current, &entry));
            entry.retired.store(true, Ordering::Release);
        }
        result
    }
}

#[cfg(test)]
#[path = "peer_connections_tests.rs"]
mod tests;
