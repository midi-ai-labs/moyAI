//! Application-owned TLS material for explicitly paired Desktop connections.

use std::io::{self, Read};
use std::net::IpAddr;
use std::sync::Arc;

use camino::Utf8Path;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{TlsAcceptor, rustls};

use super::{PublishProfileId, PublishTls};

const MAX_PEM_BYTES: u64 = 64 * 1024;

/// Public configuration accepts one certificate, never a combined key file.
/// Parse DER here: reqwest's rustls-only Certificate::from_pem defers validation.
pub(crate) fn public_certificate(pem: &str) -> io::Result<CertificateDer<'static>> {
    let text = pem.trim();
    if pem.len() as u64 > MAX_PEM_BYTES
        || !text.starts_with("-----BEGIN CERTIFICATE-----")
        || !text.ends_with("-----END CERTIFICATE-----")
        || text.matches("-----BEGIN ").count() != 1
        || text.matches("-----END ").count() != 1
    {
        return Err(io::Error::other("expected one public certificate"));
    }
    let certificate = CertificateDer::from_pem_slice(text.as_bytes())
        .map_err(|_| io::Error::other("invalid public certificate"))?;
    rustls::RootCertStore::empty()
        .add(certificate.clone())
        .map_err(|_| io::Error::other("invalid public certificate"))?;
    Ok(certificate)
}

#[derive(Debug, Serialize)]
pub struct PublishCertificateReceipt {
    pub tls: PublishTls,
    pub certificate_pem: String,
    pub sha256: String,
}

/// Called only under the stopped profile's command lane. Private material never
/// enters a receipt or a model-visible projection.
pub(crate) fn create_certificate(
    directory: &Utf8Path,
    profile_id: PublishProfileId,
    ip: IpAddr,
) -> io::Result<PublishCertificateReceipt> {
    if ip.is_unspecified() || ip.is_multicast() {
        return Err(io::Error::other("select a concrete IP address"));
    }
    let key = rcgen::generate_simple_self_signed(vec![ip.to_string()])
        .map_err(|_| io::Error::other("certificate generation failed"))?;
    let certificate_pem = key.cert.pem();
    let sha256 = format!("{:x}", Sha256::digest(key.cert.der().as_ref()));
    std::fs::create_dir_all(directory)?;
    // Unique filenames leave a previously saved certificate usable if the user
    // discards this draft. Profile validation bounds who can request generation.
    let stem = format!("{}-{}", profile_id.0, ulid::Ulid::new());
    let certificate_path = directory.join(format!("{stem}.pem"));
    let private_key_path = directory.join(format!("{stem}.key"));
    write_new(
        &private_key_path,
        key.signing_key.serialize_pem().as_bytes(),
    )?;
    write_new(&certificate_path, certificate_pem.as_bytes())?;
    Ok(PublishCertificateReceipt {
        tls: PublishTls {
            certificate_path,
            private_key_path,
        },
        certificate_pem,
        sha256,
    })
}

fn write_new(path: &Utf8Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn read_bounded(path: &Utf8Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        return Err(io::Error::other("TLS file exceeds size limit"));
    }
    Ok(bytes)
}

pub(crate) fn load_acceptor(config: &PublishTls) -> io::Result<TlsAcceptor> {
    let cert = read_bounded(&config.certificate_path)?;
    let key = read_bounded(&config.private_key_path)?;
    let certificates = CertificateDer::pem_slice_iter(&cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::other("invalid certificate"))?;
    let key =
        PrivateKeyDer::from_pem_slice(&key).map_err(|_| io::Error::other("invalid private key"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| io::Error::other("certificate and key do not match"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Hub-managed TLS accepts only clients signed by this application's Hub CA.
/// Material stays in the device identity owner; no OS trust store is changed.
pub(crate) fn load_mtls_acceptor(
    certificate_pem: &str,
    private_key_pem: &str,
    ca_certificate_pem: &str,
) -> io::Result<TlsAcceptor> {
    if [certificate_pem, private_key_pem, ca_certificate_pem]
        .iter()
        .any(|pem| pem.is_empty() || pem.len() as u64 > MAX_PEM_BYTES)
    {
        return Err(io::Error::other("invalid managed TLS material size"));
    }
    let certificates = CertificateDer::pem_slice_iter(certificate_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::other("invalid device certificate"))?;
    let key = PrivateKeyDer::from_pem_slice(private_key_pem.as_bytes())
        .map_err(|_| io::Error::other("invalid device private key"))?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(ca_certificate_pem.as_bytes()) {
        roots
            .add(certificate.map_err(|_| io::Error::other("invalid Hub CA"))?)
            .map_err(|_| io::Error::other("invalid Hub CA"))?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|_| io::Error::other("invalid Hub trust"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, key)
        .map_err(|_| io::Error::other("device certificate and key do not match"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// A shared CA authenticates membership; the directory's exact leaf identifies
/// the selected receiver. Check both during TLS, before any task body is sent.
pub(crate) fn managed_peer_http(
    certificate_pem: &str,
    private_key_pem: &str,
    ca_certificate_pem: &str,
    peer_certificate_pem: &str,
) -> io::Result<reqwest::Client> {
    if [
        certificate_pem,
        private_key_pem,
        ca_certificate_pem,
        peer_certificate_pem,
    ]
    .iter()
    .any(|pem| pem.is_empty() || pem.len() as u64 > MAX_PEM_BYTES)
    {
        return Err(io::Error::other("invalid managed TLS material size"));
    }
    let certificates = CertificateDer::pem_slice_iter(certificate_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::other("invalid device certificate"))?;
    let key = PrivateKeyDer::from_pem_slice(private_key_pem.as_bytes())
        .map_err(|_| io::Error::other("invalid device private key"))?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(ca_certificate_pem.as_bytes()) {
        roots
            .add(certificate.map_err(|_| io::Error::other("invalid Hub CA"))?)
            .map_err(|_| io::Error::other("invalid Hub CA"))?;
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|_| io::Error::other("invalid Hub trust"))?;
    let peer = CertificateDer::from_pem_slice(peer_certificate_pem.as_bytes())
        .map_err(|_| io::Error::other("invalid peer certificate"))?;
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedPeerVerifier {
            inner: verifier,
            identity: ManagedServerIdentity::Peer(Sha256::digest(peer.as_ref()).into()),
        }))
        .with_client_auth_cert(certificates, key)
        .map_err(|_| io::Error::other("device certificate and key do not match"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(4))
        .timeout(std::time::Duration::from_secs(60))
        .use_preconfigured_tls(config)
        .build()
        .map_err(|_| io::Error::other("managed peer client could not be created"))
}

/// Only Hub listeners carry this reserved SAN; enrollment replaces all device
/// CSR SANs with the observed device IP. Validate the endpoint IP AND this role.
pub(crate) const HUB_TLS_ROLE_NAME: &str = "hub.moyai.invalid";

pub(crate) fn managed_hub_http(
    identity: Option<(&str, &str)>,
    ca_certificate_pem: &str,
) -> io::Result<reqwest::Client> {
    // Control calls and inference keep their existing per-operation deadlines;
    // a client-wide timeout must not truncate an otherwise healthy model stream.
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(ca_certificate_pem.as_bytes()) {
        roots
            .add(certificate.map_err(|_| io::Error::other("invalid Hub CA"))?)
            .map_err(|_| io::Error::other("invalid Hub CA"))?;
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|_| io::Error::other("invalid Hub trust"))?;
    let builder = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedPeerVerifier {
            inner: verifier,
            identity: ManagedServerIdentity::Hub,
        }));
    let mut config = if let Some((certificate_pem, private_key_pem)) = identity {
        let certificates = CertificateDer::pem_slice_iter(certificate_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io::Error::other("invalid device certificate"))?;
        let key = PrivateKeyDer::from_pem_slice(private_key_pem.as_bytes())
            .map_err(|_| io::Error::other("invalid device private key"))?;
        builder
            .with_client_auth_cert(certificates, key)
            .map_err(|_| io::Error::other("device certificate and key do not match"))?
    } else {
        builder.with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(4))
        .use_preconfigured_tls(config)
        .build()
        .map_err(|_| io::Error::other("managed Hub client could not be created"))
}

#[derive(Debug)]
enum ManagedServerIdentity {
    Peer([u8; 32]),
    Hub,
}

#[derive(Debug)]
struct PinnedPeerVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    identity: ManagedServerIdentity,
}

impl rustls::client::danger::ServerCertVerifier for PinnedPeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self.identity {
            ManagedServerIdentity::Peer(expected) => {
                let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
                if actual != expected {
                    return Err(rustls::Error::InvalidCertificate(
                        rustls::CertificateError::ApplicationVerificationFailure,
                    ));
                }
            }
            ManagedServerIdentity::Hub => {
                self.inner.verify_server_cert(
                    end_entity,
                    intermediates,
                    &rustls::pki_types::ServerName::try_from(HUB_TLS_ROLE_NAME)
                        .expect("fixed DNS role"),
                    ocsp_response,
                    now,
                )?;
            }
        }
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

pub(crate) fn certificate_receipt(config: &PublishTls) -> io::Result<PublishCertificateReceipt> {
    let bytes = read_bounded(&config.certificate_path)?;
    let certificate = CertificateDer::from_pem_slice(&bytes)
        .map_err(|_| io::Error::other("invalid certificate"))?;
    let sha256 = format!("{:x}", Sha256::digest(certificate.as_ref()));
    let certificate_pem =
        String::from_utf8(bytes).map_err(|_| io::Error::other("invalid certificate encoding"))?;
    Ok(PublishCertificateReceipt {
        tls: config.clone(),
        certificate_pem,
        sha256,
    })
}
