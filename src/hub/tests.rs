use super::*;
use axum::{Router, routing::get};

fn catalog(revision: u64) -> HubCatalog {
    HubCatalog {
        hub_id: "hub-fixture".into(),
        software_version: "0.1.0".into(),
        revision: CatalogRevision::new(revision).unwrap(),
        models: vec![HubModel {
            id: "coding".into(),
            label: "Coding".into(),
            capabilities: BTreeSet::from(["tools".into(), "text".into()]),
        }],
        changes: vec![],
    }
}

fn selection() -> HubSelection {
    HubSelection {
        allowed_model_ids: BTreeSet::from(["coding".into()]),
        preferred_model_id: "coding".into(),
        required_capabilities: BTreeSet::from(["tools".into()]),
        wait_policy: HubWaitPolicy::WaitForPreferred,
        affinity_turns: 3,
    }
}

fn review(catalog: &HubCatalog) -> ReviewedHubSelection {
    ReviewedHubSelection::review(catalog, &catalog.hub_id, catalog.revision, selection()).unwrap()
}

#[test]
fn revision_wire_is_lossless_and_rejects_noncanonical_or_numeric_values() {
    let revision = CatalogRevision::new(u64::MAX).unwrap();
    let encoded = serde_json::to_string(&revision).unwrap();
    assert_eq!(encoded, "\"18446744073709551615\"");
    assert_eq!(
        serde_json::from_str::<CatalogRevision>(&encoded).unwrap(),
        revision
    );
    for raw in [
        "1",
        "0",
        "\"0\"",
        "\"01\"",
        "\"+1\"",
        "\"-1\"",
        "\"18446744073709551616\"",
    ] {
        assert!(serde_json::from_str::<CatalogRevision>(raw).is_err());
    }
}

#[test]
fn revision_refresh_never_reviews_and_old_admission_capture_is_immutable() {
    let old = catalog(1);
    let reviewed = review(&old);
    let mut updated = catalog(2);
    updated.software_version = "0.2.0".into();
    assert!(updated.diff(&old).unwrap().software_changed);
    assert_eq!(
        reviewed.check_admission(&updated),
        Err(HubError::ReviewRequired)
    );
    // A completed capture is not mutated by the next observed catalog.
    assert_eq!(reviewed.check_admission(&old), Ok(()));
    assert_eq!(reviewed.reviewed_revision, old.revision);
    assert_eq!(
        ReviewedHubSelection::review(&updated, &old.hub_id, old.revision, selection()),
        Err(HubError::ReviewRequired)
    );
    assert_eq!(review(&updated).check_admission(&updated), Ok(()));
}

#[test]
fn identity_revision_rollback_and_same_revision_mutation_are_distinct() {
    let old = catalog(3);
    let reviewed = review(&old);
    assert_eq!(
        reviewed.check_admission(&catalog(2)),
        Err(HubError::RevisionRollback)
    );
    let mut other = old.clone();
    other.hub_id = "other-hub".into();
    assert_eq!(
        reviewed.check_admission(&other),
        Err(HubError::DifferentHub)
    );
    other = old.clone();
    other.models[0].label = "Changed without revision".into();
    assert_eq!(other.diff(&old), Err(HubError::InvalidCatalog));
}

#[test]
fn selection_never_substitutes_removed_unselected_or_incapable_models() {
    let mut current = catalog(1);
    let selected = selection();
    current.models[0].id = "different".into();
    assert_eq!(selected.validate(&current), Err(HubError::ModelRemoved));
    current = catalog(1);
    current.models[0].capabilities.clear();
    assert_eq!(
        selected.validate(&current),
        Err(HubError::CapabilityMismatch)
    );
    let mut invalid = selected;
    invalid.allowed_model_ids.clear();
    assert_eq!(
        invalid.validate(&catalog(1)),
        Err(HubError::InvalidSelection)
    );
    invalid = selection();
    invalid.preferred_model_id = "not-selected".into();
    assert_eq!(
        invalid.validate(&catalog(1)),
        Err(HubError::InvalidSelection)
    );
}

#[test]
fn selection_capability_intersection_matches_hub_wait_and_fallback_policies() {
    let mut current = catalog(1);
    current.models.push(HubModel {
        id: "text-only".into(),
        label: "Text only".into(),
        capabilities: BTreeSet::from(["text".into()]),
    });
    let mut selected = selection();
    selected.allowed_model_ids.insert("text-only".into());
    // An incompatible alternative does not invalidate a compatible preferred model.
    assert_eq!(selected.validate(&current), Ok(()));
    selected.preferred_model_id = "text-only".into();
    assert_eq!(
        selected.validate(&current),
        Err(HubError::CapabilityMismatch)
    );
    selected.wait_policy = HubWaitPolicy::AllowSelectedFallback;
    assert_eq!(selected.validate(&current), Ok(()));
    current.models[0].capabilities.remove("tools");
    assert_eq!(
        selected.validate(&current),
        Err(HubError::CapabilityMismatch)
    );
}

#[test]
fn selection_limit_matches_the_hub_core_limit() {
    let mut current = catalog(1);
    let mut selected = selection();
    for index in 1..128 {
        let id = format!("alternative-{index}");
        current.models.push(HubModel {
            id: id.clone(),
            label: id.clone(),
            capabilities: BTreeSet::from(["tools".into()]),
        });
        selected.allowed_model_ids.insert(id);
    }
    assert_eq!(selected.allowed_model_ids.len(), 128);
    assert_eq!(selected.validate(&current), Ok(()));
    selected.allowed_model_ids.insert("one-too-many".into());
    assert_eq!(selected.validate(&current), Err(HubError::InvalidSelection));
    current.models.push(HubModel {
        id: "one-too-many".into(),
        label: "One too many".into(),
        capabilities: BTreeSet::new(),
    });
    assert_eq!(current.validate(), Err(HubError::InvalidCatalog));
}

#[test]
fn catalog_tokens_match_hub_vocabulary_while_labels_allow_unicode() {
    let mut current = catalog(1);
    current.models[0].label = "日本語のモデル".into();
    current.models[0].capabilities.insert("a".repeat(64));
    assert_eq!(current.validate(), Ok(()));
    for token in ["model/name", "space separated", "能力", "cap:tools"] {
        let mut invalid = current.clone();
        invalid.models[0].id = token.into();
        assert_eq!(invalid.validate(), Err(HubError::InvalidCatalog));
        invalid = current.clone();
        invalid.models[0].capabilities.insert(token.into());
        assert_eq!(invalid.validate(), Err(HubError::InvalidCatalog));
    }
    current.models[0].capabilities.insert("a".repeat(65));
    assert_eq!(current.validate(), Err(HubError::InvalidCatalog));
    let mut selected = selection();
    selected.required_capabilities.insert("a".repeat(65));
    assert_eq!(
        selected.validate(&catalog(1)),
        Err(HubError::InvalidSelection)
    );
    selected = selection();
    selected.allowed_model_ids.insert("invalid/id".into());
    assert_eq!(
        selected.validate(&catalog(1)),
        Err(HubError::InvalidSelection)
    );
}

#[test]
fn maximum_public_catalog_fits_the_one_megabyte_transport_budget() {
    let mut current = catalog(u64::MAX);
    current.hub_id = "h".repeat(128);
    current.software_version = "\"".repeat(128);
    current.models = (0..128)
        .map(|index| HubModel {
            id: format!("{index:03}{}", "m".repeat(125)),
            // Quotes need escaping in JSON, so this exercises the worst label expansion.
            label: "\"".repeat(256),
            capabilities: (0..32)
                .map(|index| format!("{index:02}{}", "c".repeat(62)))
                .collect(),
        })
        .collect();
    current.changes = (u64::MAX - 63..=u64::MAX)
        .map(|revision| CatalogChange {
            revision: CatalogRevision::new(revision).unwrap(),
            at_ms: u64::MAX,
            summary: "\"".repeat(2048),
        })
        .collect();
    current.validate().unwrap();
    let wire = serde_json::to_vec(&current).unwrap();
    assert!(wire.len() <= 1024 * 1024);
    assert_eq!(
        serde_json::from_slice::<HubCatalog>(&wire).unwrap(),
        current
    );
}

#[test]
fn diff_includes_add_remove_and_capability_change() {
    let old = catalog(1);
    let mut new = catalog(2);
    new.models[0].capabilities.insert("vision".into());
    new.models.push(HubModel {
        id: "small".into(),
        label: "Small".into(),
        capabilities: BTreeSet::new(),
    });
    let diff = new.diff(&old).unwrap();
    assert_eq!(diff.added, ["small"]);
    assert_eq!(diff.changed, ["coding"]);
    let mut removed = catalog(3);
    removed.models.clear();
    assert_eq!(removed.diff(&new).unwrap().removed, ["coding", "small"]);
}

#[test]
fn direct_is_default_and_main_side_review_are_independent() {
    let mut routes: DesktopHubRoutes = serde_json::from_str("{}").unwrap();
    assert_eq!(routes, DesktopHubRoutes::default());
    let old = catalog(1);
    routes.main = ModelRoute::Hub {
        reviewed: review(&old),
    };
    let encoded = serde_json::to_string(&routes).unwrap();
    let reopened: DesktopHubRoutes = serde_json::from_str(&encoded).unwrap();
    assert_eq!(reopened, routes);
    assert_eq!(
        reopened.main.check_admission(None),
        Err(HubError::Unavailable)
    );
    assert_eq!(
        reopened.main.check_admission(Some(&catalog(2))),
        Err(HubError::ReviewRequired)
    );
    assert_eq!(reopened.side_chat.check_admission(None), Ok(()));
    assert_eq!(
        reopened.side_chat.check_admission(Some(&catalog(2))),
        Ok(())
    );
    assert!(!encoded.contains("endpoint"));
    assert!(!encoded.contains("permit"));
}

#[test]
fn malformed_public_catalog_and_unknown_persisted_selection_fail_closed() {
    let mut value = catalog(1);
    value.models.push(value.models[0].clone());
    assert_eq!(value.validate(), Err(HubError::InvalidCatalog));
    value = catalog(1);
    value.models[0]
        .capabilities
        .insert("bad\ncapability".into());
    assert_eq!(value.validate(), Err(HubError::InvalidCatalog));
    let mut persisted = serde_json::to_value(review(&catalog(1))).unwrap();
    persisted["permit"] = serde_json::json!("must-not-persist");
    assert!(serde_json::from_value::<ReviewedHubSelection>(persisted).is_err());
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, server)
}

#[tokio::test]
async fn catalog_client_uses_authenticated_public_wire_without_generation() {
    let expected = catalog(1);
    let response = expected.clone();
    let (endpoint, server) = serve(Router::new().route(
        "/v1/catalog",
        get(move |headers: axum::http::HeaderMap| {
            let response = response.clone();
            async move {
                assert_eq!(
                    headers["authorization"],
                    "Bearer fixture-secret-0123456789-ABCDEFGH"
                );
                axum::Json(response)
            }
        }),
    ))
    .await;
    let client =
        HubCatalogClient::new(&endpoint, "fixture-secret-0123456789-ABCDEFGH", 1000).unwrap();
    let result = client.catalog().await;
    server.abort();
    assert_eq!(result, Ok(expected));
    assert!(!format!("{client:?}").contains("fixture-secret-0123456789-ABCDEFGH"));
    assert!(!format!("{client:?}").contains(&endpoint));
}

#[tokio::test]
async fn redirects_are_not_followed_with_bearer_credentials() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let target_hits = hits.clone();
    let (target, target_server) = serve(Router::new().route(
        "/capture",
        get(move || {
            target_hits.fetch_add(1, Ordering::SeqCst);
            async { "captured" }
        }),
    ))
    .await;
    let location = format!("{target}/capture");
    let (endpoint, server) = serve(Router::new().route(
        "/v1/catalog",
        get(move || {
            let location = location.clone();
            async move { axum::response::Redirect::temporary(&location) }
        }),
    ))
    .await;
    let result = HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 1000)
        .unwrap()
        .catalog()
        .await;
    server.abort();
    target_server.abort();
    assert_eq!(result, Err(HubError::Redirect));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deadline_bounds_response_and_errors_hide_raw_body() {
    let (endpoint, server) = serve(Router::new().route(
        "/v1/catalog",
        get(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            "raw-provider-secret"
        }),
    ))
    .await;
    let result = HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 10)
        .unwrap()
        .catalog()
        .await;
    server.abort();
    assert_eq!(result, Err(HubError::Deadline));
    let (endpoint, server) =
        serve(Router::new().route("/v1/catalog", get(|| async { "raw-provider-secret" }))).await;
    let result = HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 1000)
        .unwrap()
        .catalog()
        .await;
    server.abort();
    assert_eq!(result, Err(HubError::InvalidCatalog));
    assert!(!format!("{result:?}").contains("raw-provider-secret"));
}

#[tokio::test]
async fn unauthorized_and_large_responses_have_bounded_safe_errors() {
    let (endpoint, server) = serve(Router::new().route(
        "/v1/catalog",
        get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    ))
    .await;
    let result = HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 1000)
        .unwrap()
        .catalog()
        .await;
    server.abort();
    assert_eq!(result, Err(HubError::Unauthorized));
    let (endpoint, server) =
        serve(Router::new().route("/v1/catalog", get(|| async { "x".repeat(1024 * 1024 + 1) })))
            .await;
    let result = HubCatalogClient::new(&endpoint, "test-secret-0123456789-0123456789~", 1000)
        .unwrap()
        .catalog()
        .await;
    server.abort();
    assert_eq!(result, Err(HubError::ResponseLimit));
}

#[test]
fn connection_validation_rejects_secrets_in_url_and_plaintext_remote() {
    for endpoint in [
        "http://user:secret@localhost",
        "http://localhost/?key=secret",
        "http://localhost/#secret",
        "file:///tmp",
        "http://192.0.2.1:9000",
        "http://localhost/custom",
    ] {
        assert_eq!(
            HubCatalogClient::new(endpoint, "test-secret-0123456789-0123456789~", 1000)
                .unwrap_err(),
            HubError::InvalidConnection
        );
    }
    assert_eq!(
        HubCatalogClient::new("http://localhost", "secret\r\nx-header: value", 1000).unwrap_err(),
        HubError::InvalidConnection
    );
}
