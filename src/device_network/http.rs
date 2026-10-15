use std::sync::{Arc, RwLock};

/// One private HTTP identity shared by a device's control requests and model turns.
/// Rotation swaps credentials only between requests; credentials are never projected.
#[derive(Clone)]
pub(crate) struct ManagedHubHttp {
    client: Arc<RwLock<reqwest::Client>>,
    requests: Arc<tokio::sync::RwLock<()>>,
}

pub(crate) struct ManagedHttpLease {
    pub http: reqwest::Client,
    _request: tokio::sync::OwnedRwLockReadGuard<()>,
}

impl ManagedHubHttp {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            client: Arc::new(RwLock::new(http)),
            requests: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    pub async fn acquire(&self) -> ManagedHttpLease {
        let request = self.requests.clone().read_owned().await;
        ManagedHttpLease {
            http: self.snapshot(),
            _request: request,
        }
    }

    pub fn snapshot(&self) -> reqwest::Client {
        self.client
            .read()
            .expect("device HTTP identity lock poisoned")
            .clone()
    }

    /// Do not queue a writer behind a healthy model stream: that would block
    /// heartbeat reads. The existing heartbeat tries again between requests.
    pub fn try_rotate(&self) -> Option<ManagedHttpRotation> {
        let rotation = self.requests.clone().try_write_owned().ok()?;
        Some(ManagedHttpRotation {
            owner: self.clone(),
            _rotation: rotation,
        })
    }
}

pub(crate) struct ManagedHttpRotation {
    owner: ManagedHubHttp,
    _rotation: tokio::sync::OwnedRwLockWriteGuard<()>,
}
impl ManagedHttpRotation {
    pub fn replace(&self, client: reqwest::Client) {
        *self
            .owner
            .client
            .write()
            .expect("device HTTP identity lock poisoned") = client;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn tls_rotation_waits_for_the_current_request_and_uses_the_new_leaf_next_time() {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::new(params, ca_key);
        let leaf = |names: Vec<String>| {
            let key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::new(names).unwrap();
            params.extended_key_usages = vec![
                rcgen::ExtendedKeyUsagePurpose::ClientAuth,
                rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            ];
            let certificate = params.signed_by(&key, &issuer).unwrap();
            let fingerprint = format!("{:x}", Sha256::digest(certificate.der().as_ref()));
            (certificate.pem(), key.serialize_pem(), fingerprint)
        };
        let (server_cert, server_key, _) = leaf(vec![
            "127.0.0.1".into(),
            crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
        ]);
        let (old_cert, old_key, old_fingerprint) = leaf(vec!["127.0.0.1".into()]);
        let (new_cert, new_key, new_fingerprint) = leaf(vec!["127.0.0.1".into()]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}/v1/request", listener.local_addr().unwrap());
        let acceptor =
            crate::mcp_publish::tls::load_mtls_acceptor(&server_cert, &server_key, &ca).unwrap();
        let current = Arc::new(std::sync::Mutex::new(old_fingerprint.clone()));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let server = {
            let current = current.clone();
            let observed = observed.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                for index in 0..3 {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let mut stream = acceptor.accept(tcp).await.unwrap();
                    let fingerprint = format!(
                        "{:x}",
                        Sha256::digest(stream.get_ref().1.peer_certificates().unwrap()[0].as_ref())
                    );
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        header.push(stream.read_u8().await.unwrap());
                        assert!(header.len() < 8192);
                    }
                    observed.lock().unwrap().push(fingerprint.clone());
                    if index == 0 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    let status = if fingerprint == *current.lock().unwrap() {
                        "200 OK"
                    } else {
                        "403 Forbidden"
                    };
                    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            })
        };
        let old_http =
            crate::mcp_publish::tls::managed_hub_http(Some((&old_cert, &old_key)), &ca).unwrap();
        let http = ManagedHubHttp::new(old_http.clone());
        let lease = http.acquire().await;
        let first_url = url.clone();
        let first = tokio::spawn(async move {
            let response = lease.http.get(first_url).send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.text().await.unwrap(), "ok");
            drop(lease);
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        assert!(http.try_rotate().is_none());
        let heartbeat = tokio::time::timeout(std::time::Duration::from_millis(100), http.acquire())
            .await
            .unwrap();
        drop(heartbeat);
        release.notify_one();
        first.await.unwrap();
        let rotation = http.try_rotate().unwrap();
        *current.lock().unwrap() = new_fingerprint.clone();
        rotation.replace(
            crate::mcp_publish::tls::managed_hub_http(Some((&new_cert, &new_key)), &ca).unwrap(),
        );
        drop(rotation);
        let next = http.acquire().await;
        assert_eq!(next.http.get(&url).send().await.unwrap().status(), 200);
        drop(next);
        assert_eq!(old_http.get(&url).send().await.unwrap().status(), 403);
        server.await.unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            vec![old_fingerprint.clone(), new_fingerprint, old_fingerprint]
        );
    }
}
