//! Actual TLS transport for the receiver runtime tests' Hub receipt peer.
use super::*;
use sha2::{Digest, Sha256};

pub(super) struct ReceiptPeer {
    task: tokio::task::JoinHandle<()>,
}
impl Drop for ReceiptPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Listener {
    tcp: tokio::net::TcpListener,
    tls: tokio_rustls::TlsAcceptor,
}
impl axum::serve::Listener for Listener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (socket, addr) = self.tcp.accept().await.unwrap();
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(3), self.tls.accept(socket)).await
            {
                return (stream, addr);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

pub(super) async fn receipt_peer(
    path: &camino::Utf8Path,
    settings: &mut crate::device_network::DeviceSettings,
) -> (crate::device_network::SharedHubConfig, ReceiptPeer) {
    let identity = crate::device_network::DeviceIdentityStore::new(path.join("identity.json"))
        .load_or_create()
        .unwrap();
    let device_key = rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap();
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let device = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&device_key, &issuer)
        .unwrap();
    settings.certificate_pem = Some(device.pem());
    settings.certificate_sha256 = Some(format!("{:x}", Sha256::digest(device.der())));
    let expires = crate::runtime::SystemClock::now_ms() as u64 + 30 * 86_400_000;
    settings.expires_at_ms = Some(expires.to_string());
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server =
        rcgen::CertificateParams::new(vec!["127.0.0.1".into(), "hub.moyai.invalid".into()])
            .unwrap()
            .signed_by(&server_key, &issuer)
            .unwrap();
    let tls = crate::mcp_publish::tls::load_mtls_acceptor(
        &server.pem(),
        &server_key.serialize_pem(),
        &ca.pem(),
    )
    .unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("https://{}/", tcp.local_addr().unwrap());
    let own = json!({"hub_id":"hub","device_id":"device-b","label":settings.label,"groups":[],"revision":"1","certificate_sha256":settings.certificate_sha256,"expires_at_ms":expires,"cancelled_lineages":[]});
    let router = axum::Router::new()
        .route("/v1/network/presence", axum::routing::post(|| async { axum::Json(json!({})) }))
        .route("/v1/network/self", axum::routing::get(move || { let own = own.clone(); async move { axum::Json(own) } }))
        .route("/v1/network/directory", axum::routing::get(|| async { axum::Json(json!({"hub_id":"hub","revision":"1","peers":[]})) }))
        .route("/v1/network/publish", axum::routing::post(|| async { axum::Json(json!({})) }))
        .route("/v1/network/jobs/accepted", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move { axum::Json(json!({"accepted":true,"grant_id":body["grant_id"],"job_id":body["job_id"]})) }))
        .route("/v1/network/jobs/settled", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move { axum::Json(json!({"settled":true,"grant_id":body["grant_id"],"job_id":body["job_id"],"state":body["state"]})) }));
    let task = tokio::spawn(async move {
        axum::serve(Listener { tcp, tls }, router).await.unwrap();
    });
    (
        crate::device_network::SharedHubConfig {
            hub_url: endpoint,
            ca_certificate_pem: ca.pem(),
        },
        ReceiptPeer { task },
    )
}
