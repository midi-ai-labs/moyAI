use super::*;
use crate::hub::{HubRouteMode, HubSelection, HubSettings, HubSettingsStore, HubWaitPolicy};
use serde_json::json;

fn catalog(revision: u64) -> HubCatalog {
    HubCatalog {
        hub_id: "hub-a".into(),
        revision: CatalogRevision::new(revision).unwrap(),
        software_version: "0.1.0".into(),
        changes: Vec::new(),
        models: ["keep", "remove", "rename"]
            .into_iter()
            .map(|id| HubModel {
                id: id.into(),
                label: format!("Model {id}"),
                capabilities: ["chat".into()].into(),
            })
            .collect(),
    }
}

fn review(catalog: &HubCatalog) -> ReviewedHubSelection {
    ReviewedHubSelection::review(
        catalog,
        &catalog.hub_id,
        catalog.revision,
        HubSelection {
            allowed_model_ids: catalog
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect(),
            preferred_model_id: catalog.models[0].id.clone(),
            required_capabilities: Default::default(),
            wait_policy: HubWaitPolicy::WaitForPreferred,
            affinity_turns: 1,
        },
    )
    .unwrap()
}

fn settings(catalog: &HubCatalog) -> HubSettings {
    let mut settings = HubSettings::default();
    settings.endpoint = "http://127.0.0.1:9470/".into();
    settings.hub_id = Some(catalog.hub_id.clone());
    settings.main_review = Some(review(catalog));
    settings.main_catalog_baseline = Some(HubCatalogBaseline::capture(catalog));
    settings
}

fn store(temp: &tempfile::TempDir) -> HubSettingsStore {
    HubSettingsStore::new(camino::Utf8PathBuf::from_path_buf(temp.path().join("hub.json")).unwrap())
}

#[test]
fn comparison_returns_public_before_after_and_never_invents_missing_or_invalid_baselines() {
    use HubCatalogComparisonStatus as Status;
    let old = catalog(1);
    let reviewed = review(&old);
    let baseline = HubCatalogBaseline::capture(&old);
    let mut current = catalog(3);
    current.models.retain(|model| model.id != "remove");
    current.models[1].label = "New label".into();
    current.models[1].capabilities.insert("vision".into());
    current.models.push(HubModel {
        id: "new".into(),
        label: "Added".into(),
        capabilities: ["chat".into()].into(),
    });
    current.software_version = "0.2.0".into();
    let diff = HubCatalogComparison::between(Some(&reviewed), Some(&baseline), Some(&current));
    assert_eq!(diff.status, Status::Compared);
    assert_eq!(diff.software_before.as_deref(), Some("0.1.0"));
    assert_eq!(diff.software_after.as_deref(), Some("0.2.0"));
    assert_eq!(
        diff.models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["new", "remove", "rename"]
    );
    assert!(diff.models[0].before.is_none());
    assert!(diff.models[1].after.is_none());
    assert_eq!(
        diff.models[2].before.as_ref().unwrap().label,
        "Model rename"
    );
    assert_eq!(diff.models[2].after.as_ref().unwrap().label, "New label");
    assert!(
        diff.models[2]
            .after
            .as_ref()
            .unwrap()
            .capabilities
            .contains("vision")
    );
    assert_eq!(reviewed.reviewed_revision, old.revision);
    assert_eq!(
        HubCatalogComparison::between(Some(&reviewed), None, Some(&current)).status,
        Status::BaselineUnavailable
    );
    assert_eq!(
        HubCatalogComparison::between(None, None, Some(&current)).status,
        Status::FirstReview
    );
    assert_eq!(
        HubCatalogComparison::between(Some(&reviewed), Some(&baseline), None).status,
        Status::CurrentUnavailable
    );
    for invalid in [
        {
            let mut value = current.clone();
            value.revision = old.revision;
            value
        },
        {
            let mut value = current.clone();
            value.hub_id = "another-hub".into();
            value
        },
        {
            let mut value = old.clone();
            value.models[0].label.clear();
            value
        },
    ] {
        assert_eq!(
            HubCatalogComparison::between(Some(&reviewed), Some(&baseline), Some(&invalid)).status,
            Status::Invalid
        );
    }
    let mut with_journal = old.clone();
    with_journal.changes.push(crate::hub::CatalogChange {
        revision: old.revision,
        at_ms: 1,
        summary: "Initial catalog".into(),
    });
    let unchanged =
        HubCatalogComparison::between(Some(&reviewed), Some(&baseline), Some(&with_journal));
    assert_eq!(unchanged.status, Status::Compared);
    assert!(unchanged.models.is_empty());
}

#[test]
fn legacy_settings_gain_no_invented_baseline_and_upgrade_only_on_explicit_save() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let path = temp.path().join("hub.json");
    for schema in [1, 2] {
        let mut legacy = serde_json::to_value(settings(&catalog(1))).unwrap();
        legacy["schema_version"] = json!(schema);
        let object = legacy.as_object_mut().unwrap();
        object.remove("main_catalog_baseline");
        object.remove("side_chat_catalog_baseline");
        if schema == 1 {
            object.remove("main_mode");
            object.remove("side_chat_mode");
        }
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.schema_version, 3);
        assert_eq!(loaded.main_mode, HubRouteMode::Direct);
        assert!(loaded.main_review.is_some());
        assert!(loaded.main_catalog_baseline.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let saved = store.save(&loaded).unwrap();
        assert_eq!(saved.schema_version, 3);
        assert!(saved.main_catalog_baseline.is_none());
        assert_eq!(store.load().unwrap(), saved);
        legacy["main_catalog_baseline"] = json!(null);
        let invalid = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &invalid).unwrap();
        assert_eq!(store.load(), Err(HubError::SettingsInvalid));
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
}

#[test]
fn baseline_store_rejects_mismatched_identity_revision_and_missing_fields_without_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let path = temp.path().join("hub.json");
    let saved = store.save(&settings(&catalog(1))).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    for proposed in [
        {
            let mut value = saved.clone();
            value.main_catalog_baseline.as_mut().unwrap().hub_id = "hub-b".into();
            value
        },
        {
            let mut value = saved.clone();
            value.main_catalog_baseline.as_mut().unwrap().revision =
                CatalogRevision::new(2).unwrap();
            value
        },
        {
            let mut value = saved.clone();
            value.main_review = None;
            value
        },
        {
            let mut value = saved.clone();
            value.main_catalog_baseline.as_mut().unwrap().models.clear();
            value
        },
    ] {
        assert_eq!(store.save(&proposed), Err(HubError::SettingsInvalid));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, serde_json::to_vec(&proposed).unwrap()).unwrap();
        assert_eq!(store.load(), Err(HubError::SettingsInvalid));
        std::fs::write(&path, &bytes).unwrap();
    }
    let mut missing = serde_json::to_value(&saved).unwrap();
    missing
        .as_object_mut()
        .unwrap()
        .remove("side_chat_catalog_baseline");
    std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    assert_eq!(store.load(), Err(HubError::SettingsInvalid));
}

#[test]
fn settings_store_fits_two_maximum_public_catalogs_and_rejects_over_limit_input() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let path = temp.path().join("hub.json");
    let mut large = catalog(1);
    large.models = (0..128)
        .map(|index| HubModel {
            id: format!("{index:03}{}", "m".repeat(125)),
            label: "L".repeat(256),
            capabilities: (0..32)
                .map(|capability| format!("{capability:02}{}", "c".repeat(62)))
                .collect(),
        })
        .collect();
    large.validate().unwrap();
    let mut proposed = settings(&large);
    proposed.side_chat_review = proposed.main_review.clone();
    proposed.side_chat_catalog_baseline = proposed.main_catalog_baseline.clone();
    let saved = store.save(&proposed).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > 128 * 1024);
    assert!(bytes.len() <= 1024 * 1024);
    assert_eq!(store.load().unwrap(), saved);
    let too_large = vec![b' '; 1024 * 1024 + 1];
    std::fs::write(&path, &too_large).unwrap();
    assert_eq!(store.load(), Err(HubError::SettingsInvalid));
    assert_eq!(store.save(&saved), Err(HubError::SettingsInvalid));
    assert_eq!(std::fs::read(&path).unwrap(), too_large);
}
