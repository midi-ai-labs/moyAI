use super::*;
use serde_json::json;

#[tokio::test]
async fn folder_change_status_covers_active_unknown_retained_and_local_processes() {
    use crate::runner::shared::{
        RetainedServiceProjection, SharedAttemptProjection, SharedProjection,
    };
    use crate::runner::{Execution, LocalRunRequest};
    use crate::runtime::RunControl;
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
    use tokio_util::sync::CancellationToken;

    let base = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("project_sandbox/pre-physical-winab-20260927/folder-projection");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process = crate::app::AppBootstrap::create_process_runtime(StoreBundle::new(sqlite))
        .await
        .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let idle = host.operations_projection().unwrap();
    assert!(!idle.folder_change_blocked);
    let mut legacy = serde_json::to_value(&idle).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("folder_change_blocked");
    assert!(
        !serde_json::from_value::<RunnerOperationsProjection>(legacy)
            .unwrap()
            .folder_change_blocked
    );

    let attempt = SharedAttemptProjection {
        attempt_id: "attempt".into(),
        generation: 1,
        job_id: "job".into(),
        project_id: "project".into(),
        environment_id: "environment".into(),
        run_id: Ulid::new(),
        state: "executing".into(),
        local_state: None,
    };
    for label in ["executing", "unknown"] {
        host.inner.state.lock().unwrap().shared_projection = Some(SharedProjection {
            attempts: vec![SharedAttemptProjection {
                state: label.into(),
                ..attempt.clone()
            }],
            ..Default::default()
        });
        assert!(
            host.operations_projection().unwrap().folder_change_blocked,
            "{label}"
        );
    }
    host.inner.state.lock().unwrap().shared_projection = Some(SharedProjection {
        retained_services: vec![RetainedServiceProjection {
            service_id: "service".into(),
            attempt_id: "attempt".into(),
            generation: 1,
            project_id: "project".into(),
            conversation_id: "conversation".into(),
            environment_id: "environment".into(),
            expires_at_ms: Some(1),
            local_state: "retained".into(),
            uncertain: false,
        }],
        ..Default::default()
    });
    assert!(host.operations_projection().unwrap().folder_change_blocked);
    host.inner.state.lock().unwrap().shared_projection = Some(SharedProjection::default());
    assert!(!host.operations_projection().unwrap().folder_change_blocked);

    let id = Ulid::new();
    host.inner.state.lock().unwrap().runs.insert(
        id,
        Execution {
            managed_scope_id: id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Task".into(),
                session_id: None,
                title: None,
                single_agent: true,
            },
            session_id: None,
            control: RunControl::new(),
            process_lifetime: CancellationToken::new(),
            processes_drained: false,
            service: None,
            worker: None,
            result: None,
            response: None,
            shared: false,
            resource: None,
        },
    );
    assert!(host.operations_projection().unwrap().folder_change_blocked);
    host.inner
        .state
        .lock()
        .unwrap()
        .runs
        .get_mut(&id)
        .unwrap()
        .processes_drained = true;
    assert!(!host.operations_projection().unwrap().folder_change_blocked);
    host.inner.state.lock().unwrap().runs.clear();
}

#[test]
fn saved_desktop_consent_follows_identity_but_rejects_ambiguous_legacy_bindings() {
    let hash = "a".repeat(64);
    let old = format!("hub|device|https://old.example:9471|{hash}");
    let new = format!("hub|device|https://new.example:9471|{hash}");
    assert!(desktop_consent_matches(&old, &new));
    for invalid in [
        format!("other|device|https://new.example:9471|{hash}"),
        format!("hub|other|https://new.example:9471|{hash}"),
        format!("hub|device|https://new.example:9471|{}", "b".repeat(64)),
        format!("hub|device|http://new.example:9471|{hash}"),
        format!("hub|device|https://new.example:9471/path|{hash}"),
        format!("hub|device|https://new.example:9471|{hash}|extra"),
        "hub/device/trust".into(),
    ] {
        assert!(!desktop_consent_matches(&old, &invalid), "{invalid}");
    }
}

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
