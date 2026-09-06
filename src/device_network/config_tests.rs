use super::{DeviceError, SharedHubConfig};

fn public_config() -> (SharedHubConfig, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["hub.test".into()]).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let cert = params.self_signed(&key).unwrap();
    (
        SharedHubConfig {
            hub_url: "https://hub.test:9471".into(),
            ca_certificate_pem: cert.pem(),
        },
        key.serialize_pem(),
    )
}

fn document(shared: &SharedHubConfig) -> String {
    format!("[device_network]\n{}", toml::to_string(shared).unwrap())
}

#[test]
fn import_takes_only_shared_public_trust_and_never_adopts_other_machine_authority() {
    let (shared, _) = public_config();
    let text = format!(
        "[model]\nmodel = 'foreign-model'\n[permissions]\naccess_mode = 'full_access'\n[mcp]\nservers_json = 'foreign-secret'\n{}",
        document(&shared)
    );
    let imported = SharedHubConfig::import(&text).unwrap();
    assert_eq!(imported, shared);
    let round_trip = toml::to_string(&imported).unwrap();
    assert!(!round_trip.contains("foreign-secret"));
    assert!(!round_trip.contains("full_access"));
    assert_eq!(
        SharedHubConfig::import(&document(&imported)).unwrap(),
        shared
    );
}

#[test]
fn shared_import_rejects_unknown_private_fields_within_the_shared_section() {
    let (shared, _) = public_config();
    for key in [
        "private_key_pem",
        "token",
        "device_id",
        "receiver",
        "trusted_certificate_path",
    ] {
        let text = format!("{}\n{key} = 'must-not-be-shared'\n", document(&shared));
        assert_eq!(
            SharedHubConfig::import(&text),
            Err(DeviceError::InvalidConfiguration),
            "{key}"
        );
    }
    assert_eq!(
        SharedHubConfig::import("[model]\nmodel = 'only-model'"),
        Err(DeviceError::InvalidConfiguration)
    );
}

#[test]
fn shared_url_requires_an_https_origin_without_credentials_paths_or_query() {
    let (mut shared, _) = public_config();
    for url in [
        "http://hub.test:9471",
        "https://user@hub.test:9471",
        "https://user:secret@hub.test:9471",
        "https://hub.test/network",
        "https://hub.test/?token=x",
        "https://hub.test/#fragment",
        "not-a-url",
        "",
    ] {
        shared.hub_url = url.into();
        assert_eq!(
            shared.validate(),
            Err(DeviceError::InvalidConfiguration),
            "{url}"
        );
    }
    shared.hub_url = format!("https://{}.test", "a".repeat(2048));
    assert_eq!(shared.validate(), Err(DeviceError::InvalidConfiguration));
}

#[test]
fn shared_import_rejects_absent_malformed_and_non_certificate_trust() {
    let (shared, key) = public_config();
    let cases = [
        ("empty", "".to_string()),
        ("garbage", "not a certificate".to_string()),
        (
            "invalid_der",
            "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n".to_string(),
        ),
        ("private_key_only", key),
        ("oversized", "x".repeat(65537)),
    ];
    let accepted = cases
        .into_iter()
        .filter_map(|(label, pem)| {
            let candidate = SharedHubConfig {
                ca_certificate_pem: pem,
                ..shared.clone()
            };
            SharedHubConfig::import(&document(&candidate))
                .is_ok()
                .then_some(label)
        })
        .collect::<Vec<_>>();
    assert!(accepted.is_empty(), "invalid trust accepted: {accepted:?}");
    assert_eq!(
        SharedHubConfig::import(&" ".repeat(256 * 1024 + 1)),
        Err(DeviceError::InvalidConfiguration)
    );
}

#[test]
fn shared_import_rejects_private_key_material_appended_to_public_certificate() {
    let (shared, key) = public_config();
    for combined in [
        format!("{}\n{key}", shared.ca_certificate_pem),
        format!("{key}\n{}", shared.ca_certificate_pem),
    ] {
        let candidate = SharedHubConfig {
            ca_certificate_pem: combined,
            ..shared.clone()
        };
        assert_eq!(
            SharedHubConfig::import(&document(&candidate)).err(),
            Some(DeviceError::InvalidConfiguration)
        );
    }
}

#[test]
fn omitted_shared_configuration_remains_unconfigured_and_partial_configuration_is_invalid() {
    let defaults = SharedHubConfig::default();
    assert!(!defaults.configured());
    assert!(defaults.validate().is_err());
    let (shared, _) = public_config();
    for partial in [
        SharedHubConfig {
            hub_url: shared.hub_url.clone(),
            ..Default::default()
        },
        SharedHubConfig {
            ca_certificate_pem: shared.ca_certificate_pem.clone(),
            ..Default::default()
        },
    ] {
        assert!(partial.configured());
        assert_eq!(partial.validate(), Err(DeviceError::InvalidConfiguration));
    }
}
