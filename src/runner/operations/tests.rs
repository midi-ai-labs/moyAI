use super::*;
use serde_json::json;

#[test]
fn desktop_consent_is_forward_compatible_and_scopes_delegation_to_confirmed_provisioning() {
    let mut installed: Installed = serde_json::from_value(json!({
        "version":1,"mode":"available","maintenance_until_ms":null,"settings":null,"templates":[]
    }))
    .unwrap();
    assert!(installed.desktop_binding.is_none());
    let mapping = EnvironmentMapping {
        environment_id: "parent".into(),
        directory: Utf8PathBuf::from("C:/fixture"),
        access_mode: crate::config::AccessMode::Default,
        allowed_child_environments: vec!["explicit".into(), "revoked".into()],
    };
    let allowed = vec!["explicit".into(), "project-child".into()];
    assert_eq!(
        installed.child_environments(&mapping, &allowed),
        vec!["explicit"]
    );
    installed.desktop_binding = Some("hub/device/trust".into());
    for (environment, template, success) in [
        ("other", "desktop-default", true),
        ("parent", "manual", true),
        ("parent", "desktop-default", false),
    ] {
        installed.provisions = vec![serde_json::from_value(json!({"environment_id":environment,"template_id":template,"generation":1,"success":success,"error":null})).unwrap()];
        assert_eq!(
            installed.child_environments(&mapping, &allowed),
            vec!["explicit"]
        );
    }
    installed.provisions = vec![serde_json::from_value(json!({"environment_id":"parent","template_id":"desktop-default","generation":2,"success":true,"error":null})).unwrap()];
    assert_eq!(installed.child_environments(&mapping, &allowed), allowed);
    assert!(installed.child_environments(&mapping, &[]).is_empty());
    let base = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("project_sandbox/project-centered-setup-20260913/operations");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    let mut store = OperationsStore::open(root).unwrap();
    store.update(installed).unwrap();
    let restored = OperationsStore::open(root).unwrap();
    assert_eq!(
        restored.installed.desktop_binding.as_deref(),
        Some("hub/device/trust")
    );
    assert_eq!(
        restored.installed.child_environments(&mapping, &allowed),
        allowed
    );
}
