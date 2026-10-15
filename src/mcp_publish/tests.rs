use std::fs::OpenOptions;

use camino::Utf8PathBuf;
use serde_json::json;
use ulid::Ulid;

use super::*;
use crate::session::{ProjectId, SessionId};
use crate::tool::ToolName;
use crate::tool::registry::ToolRegistry;

fn target() -> PublishTarget {
    PublishTarget::Project {
        project_id: ProjectId::new(),
        workspace_root: Utf8PathBuf::from_path_buf(std::env::temp_dir()).expect("UTF-8 temp path"),
    }
}

fn paired_profile() -> PublishProfile {
    let mut profile = PublishProfile::new("Workspace search".to_string(), target());
    profile.tools = vec![ToolName::List, ToolName::Read];
    profile.authentication = PublishAuthentication::LocalCredential {
        credential_id: Ulid::new(),
    };
    profile
}

fn profile_set(profile: PublishProfile) -> PublishProfileSet {
    PublishProfileSet {
        profiles: vec![profile],
        ..Default::default()
    }
}

fn store_fixture() -> (tempfile::TempDir, Utf8PathBuf, PublishProfileStore) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = Utf8PathBuf::from_path_buf(directory.path().join("mcp-publish.json"))
        .expect("UTF-8 profile path");
    let store = PublishProfileStore::new(path.clone());
    (directory, path, store)
}

#[test]
fn new_and_legacy_missing_opt_in_fields_are_disabled_and_unpaired() {
    let profile = PublishProfile::new("Research".to_string(), target());
    assert!(!profile.enabled);
    assert_eq!(profile.authentication, PublishAuthentication::Unpaired {});
    assert_eq!(
        profile.background,
        PublishBackgroundPolicy::StopWhenWindowCloses
    );
    profile.validate().expect("disabled draft is valid");
    let mut value = serde_json::to_value(&profile).expect("profile JSON");
    for name in ["enabled", "authentication", "background", "mode", "tls"] {
        value.as_object_mut().expect("object").remove(name);
    }
    let loaded: PublishProfile = serde_json::from_value(value).expect("safe field defaults");
    assert_eq!(loaded, profile);
}

#[test]
fn multiple_profiles_round_trip_rename_and_toggle_by_stable_identity() {
    let (_directory, _path, store) = store_fixture();
    let mut profiles = store.load().expect("new store is empty");
    assert!(profiles.profiles.is_empty());
    let first = paired_profile();
    let first_id = first.id;
    let second = PublishProfile::new(first.label.clone(), target());
    let second_id = second.id;
    assert_ne!(first_id, second_id);
    profiles.profiles = vec![first, second];
    let mut saved = store.save(&profiles).expect("first save");
    assert_eq!(saved.revision, 1);
    saved.profiles[0].label = "Renamed publication".to_string();
    saved.profiles[0].enabled = true;
    let saved = store.save(&saved).expect("save independent enable");
    assert_eq!(store.load().expect("reload"), saved);
    assert_eq!(saved.profiles[0].id, first_id);
    assert_eq!(saved.profiles[1].id, second_id);
    assert!(!saved.profiles[1].enabled);
}

#[test]
fn public_bind_and_empty_port_are_rejected_even_for_disabled_profiles() {
    let mut profile = paired_profile();
    for bind in [
        "0.0.0.0:7332",
        "[::]:7332",
        "192.168.1.2:7332",
        "127.0.0.1:0",
    ] {
        profile.bind = bind.parse().expect("socket address");
        assert!(matches!(
            profile.validate(),
            Err(PublishError::InvalidConfiguration(_))
        ));
    }
    profile.bind = "[::1]:7332".parse().expect("IPv6 loopback");
    profile
        .validate()
        .expect("explicit authenticated loopback profile");
}

#[test]
fn enabled_profile_requires_pairing_and_explicit_tools() {
    let mut profile = paired_profile();
    profile.enabled = true;
    profile.authentication = PublishAuthentication::Unpaired {};
    assert!(matches!(profile.validate(), Err(PublishError::Unpaired)));
    profile.authentication = PublishAuthentication::LocalCredential {
        credential_id: Ulid::new(),
    };
    profile.tools.clear();
    assert!(matches!(
        profile.validate(),
        Err(PublishError::InvalidConfiguration(_))
    ));
}

#[test]
fn stable_profile_identity_is_unique_but_saved_intent_does_not_reserve_a_bind() {
    let mut profile = paired_profile();
    profile.enabled = true;
    let mut profiles = profile_set(profile.clone());
    profiles.profiles.push(profile);
    assert!(profiles.validate().is_err());
    profiles.profiles[1].id = PublishProfileId(Ulid::new());
    profiles
        .validate()
        .expect("persisted enable intent does not prove either listener is running");
    profiles.profiles[1].bind = "0.0.0.0:7332".parse().unwrap();
    assert!(
        profiles.validate().is_err(),
        "each profile still validates its bind boundary"
    );
}

#[test]
fn read_effect_does_not_implicitly_publish_bookkeeping_or_client_mcp() {
    let mut profile = paired_profile();
    let registry = ToolRegistry::core_agent();
    for tool in [
        ToolName::Write,
        ToolName::Shell,
        ToolName::UpdatePlan,
        ToolName::GetGoal,
        ToolName::CreateGoal,
        ToolName::McpCall,
        ToolName::Invalid,
    ] {
        profile.tools = vec![tool];
        assert!(matches!(
            profile.preview_tool_specs(&registry),
            Err(PublishError::ToolUnavailable)
        ));
    }
    profile.tools = vec![ToolName::Read, ToolName::Read];
    assert!(matches!(
        profile.validate(),
        Err(PublishError::ToolUnavailable)
    ));
}

#[test]
fn preview_uses_current_registry_and_rejects_unavailable_selection() {
    let profile = paired_profile();
    let registry = ToolRegistry::core_agent();
    let specs = profile
        .preview_tool_specs(&registry)
        .expect("configured tool preview");
    assert_eq!(
        specs.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        profile.tools
    );
    assert_eq!(
        specs[1].input_schema,
        registry
            .specs()
            .into_iter()
            .find(|spec| spec.name == ToolName::Read)
            .expect("read spec")
            .input_schema
    );
    let mut restricted = registry.clone();
    restricted.retain_tools(|name| name != "read");
    assert!(matches!(
        profile.preview_tool_specs(&restricted),
        Err(PublishError::ToolUnavailable)
    ));
}

#[test]
fn configuration_preflight_validates_without_opening_a_listener() {
    let (_directory, _path, store) = store_fixture();
    let mut profile = paired_profile();
    let available_target = profile.target.clone();
    let profile_id = profile.id;
    let registry = ToolRegistry::core_agent();
    let disabled = profile_set(profile.clone());
    assert!(matches!(
        validate_profile_start(&disabled, profile_id, &registry, &available_target),
        Err(PublishError::Disabled)
    ));
    profile.enabled = true;
    store
        .save(&profile_set(profile))
        .expect("save enabled intent");
    let loaded = store.load().expect("restart load");
    assert!(matches!(
        validate_profile_start(&loaded, profile_id, &registry, &available_target),
        Ok(())
    ));
    assert!(matches!(
        validate_profile_start(
            &loaded,
            PublishProfileId(Ulid::new()),
            &registry,
            &available_target
        ),
        Err(PublishError::UnknownProfile)
    ));
    let wrong_target = PublishTarget::Temp {};
    assert!(matches!(
        validate_profile_start(&loaded, profile_id, &registry, &wrong_target),
        Err(PublishError::TargetMismatch)
    ));
}

#[test]
fn unknown_schema_fields_and_secret_material_fail_closed_without_echoing_values() {
    let (_directory, path, store) = store_fixture();
    let profiles = profile_set(paired_profile());
    let mut value = serde_json::to_value(&profiles).expect("JSON");
    value["profiles"][0]["authentication"]["token"] = json!("private-test-credential");
    std::fs::write(&path, serde_json::to_vec(&value).expect("JSON bytes")).expect("fixture");
    let error = store.load().expect_err("unknown secret field rejected");
    assert!(matches!(error, PublishError::InvalidDocument));
    assert!(!error.to_string().contains("private-test-credential"));
    let mut unknown = profiles;
    unknown.schema_version += 1;
    std::fs::write(&path, serde_json::to_vec(&unknown).expect("JSON")).expect("fixture");
    assert!(matches!(
        store.load(),
        Err(PublishError::InvalidConfiguration(_))
    ));
}

#[test]
fn corrupt_existing_document_is_not_reset_or_overwritten_on_save() {
    let (_directory, path, store) = store_fixture();
    std::fs::write(&path, b"{incomplete").expect("corrupt document");
    assert!(matches!(store.load(), Err(PublishError::InvalidDocument)));
    assert!(matches!(
        store.save(&PublishProfileSet::default()),
        Err(PublishError::InvalidDocument)
    ));
    assert_eq!(
        std::fs::read(path).expect("preserved bytes"),
        b"{incomplete"
    );
}

#[test]
fn stale_or_invalid_save_preserves_the_current_profiles() {
    let (_directory, _path, first) = store_fixture();
    let second = first.clone();
    let initial = profile_set(paired_profile());
    let winner = first.save(&initial).expect("winner");
    assert!(matches!(
        second.save(&initial),
        Err(PublishError::StaleRevision)
    ));
    let mut invalid = winner.clone();
    invalid.profiles[0].max_concurrent_calls = 0;
    assert!(matches!(
        first.save(&invalid),
        Err(PublishError::InvalidConfiguration(_))
    ));
    assert_eq!(first.load().expect("unchanged"), winner);
}

#[test]
fn competing_writer_is_nonblocking_and_retains_last_valid_document() {
    let (_directory, path, store) = store_fixture();
    let saved = store
        .save(&profile_set(paired_profile()))
        .expect("initial save");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("{path}.lock"))
        .expect("lock");
    fs2::FileExt::try_lock_exclusive(&lock).expect("hold writer lock");
    assert!(matches!(store.save(&saved), Err(PublishError::StoreBusy)));
    assert_eq!(store.load().expect("read last complete snapshot"), saved);
    drop(lock);
    store.save(&saved).expect("save after writer releases lock");
}

#[test]
fn oversized_document_and_relative_workspace_are_rejected() {
    let (_directory, path, store) = store_fixture();
    std::fs::write(path, vec![b' '; 256 * 1024 + 1]).expect("bounded fixture");
    assert!(matches!(store.load(), Err(PublishError::InvalidDocument)));
    let mut profile = paired_profile();
    profile.target = PublishTarget::Project {
        project_id: ProjectId::new(),
        workspace_root: Utf8PathBuf::from("relative/workspace"),
    };
    assert!(matches!(
        profile.validate(),
        Err(PublishError::InvalidConfiguration(_))
    ));
    profile.target = PublishTarget::Project {
        project_id: ProjectId::new(),
        workspace_root: target()
            .workspace_root()
            .unwrap()
            .join("..")
            .join("elsewhere"),
    };
    assert!(matches!(
        profile.validate(),
        Err(PublishError::InvalidConfiguration(_))
    ));
}

#[test]
fn temp_round_trip_has_no_project_or_folder_and_rejects_filesystem_tools() {
    let (_directory, _path, store) = store_fixture();
    let mut profile = PublishProfile::new("Temporary queries".into(), PublishTarget::Temp {});
    assert_eq!(profile.target.workspace_root(), None);
    profile.tools = vec![ToolName::CurrentTime];
    let saved = store
        .save(&profile_set(profile.clone()))
        .expect("save temp");
    assert_eq!(store.load().expect("reload temp"), saved);
    assert_eq!(
        serde_json::to_value(&profile.target).unwrap(),
        json!({"kind":"temp"})
    );
    for tool in [
        ToolName::List,
        ToolName::Glob,
        ToolName::Grep,
        ToolName::Read,
        ToolName::InspectDirectory,
    ] {
        profile.tools = vec![tool];
        assert!(matches!(
            profile.validate(),
            Err(PublishError::ToolUnavailable)
        ));
    }
    for target in [
        json!({"kind":"temp","project_id":ProjectId::new()}),
        json!({"kind":"temp","workspace_root":"C:/"}),
        json!({"kind":"project","project_id":ProjectId::new()}),
    ] {
        assert!(serde_json::from_value::<PublishTarget>(target).is_err());
    }
}

fn legacy_document() -> (serde_json::Value, PublishProfile) {
    let mut profile = paired_profile();
    profile.enabled = true;
    profile.background = PublishBackgroundPolicy::KeepWhileApplicationRunning;
    profile.target = PublishTarget::LegacySession {
        project_id: ProjectId::new(),
        root_session_id: SessionId::new(),
        workspace_root: target()
            .workspace_root()
            .unwrap()
            .join("project")
            .join("subfolder"),
    };
    let mut document = serde_json::to_value(profile_set(profile.clone())).unwrap();
    document["schema_version"] = json!(1);
    document["revision"] = json!(41);
    for field in ["mode", "tls"] {
        document["profiles"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    document["profiles"][0]["target"]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    (document, profile)
}

#[test]
fn schema_one_migration_preserves_authority_credentials_and_cas_until_explicit_save() {
    let (_directory, path, store) = store_fixture();
    let (document, profile) = legacy_document();
    let original = serde_json::to_vec(&document).unwrap();
    std::fs::write(&path, &original).unwrap();
    let migrated = store.load().expect("read legacy schema");
    assert_eq!(migrated.schema_version, 3);
    assert_eq!(migrated.revision, 41);
    assert_eq!(migrated.profiles, vec![profile]);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "read never rewrites user settings"
    );
    let saved = store
        .save(&migrated)
        .expect("explicit atomic migration save");
    assert_eq!(saved.revision, 42);
    assert_eq!(saved.profiles, migrated.profiles);
    let disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(disk["schema_version"], json!(3));
    assert_eq!(
        disk["profiles"][0]["target"]["kind"],
        json!("legacy_session")
    );
    assert_eq!(store.load().unwrap(), saved);
    assert!(matches!(
        store.save(&migrated),
        Err(PublishError::StaleRevision)
    ));
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn schema_one_safe_defaults_migrate_but_invalid_or_ambiguous_targets_do_not() {
    let (_directory, path, store) = store_fixture();
    let (mut document, _) = legacy_document();
    for name in ["enabled", "authentication", "background"] {
        document["profiles"][0]
            .as_object_mut()
            .unwrap()
            .remove(name);
    }
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let loaded = store.load().unwrap();
    assert!(!loaded.profiles[0].enabled);
    assert_eq!(
        loaded.profiles[0].authentication,
        PublishAuthentication::Unpaired {}
    );
    assert_eq!(
        loaded.profiles[0].background,
        PublishBackgroundPolicy::StopWhenWindowCloses
    );
    for field in ["kind", "token", "extra"] {
        let mut invalid = document.clone();
        invalid["profiles"][0]["target"][field] = json!("private-invalid-value");
        let bytes = serde_json::to_vec(&invalid).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let error = store.load().expect_err("unknown legacy field");
        assert!(!error.to_string().contains("private-invalid-value"));
        assert!(store.save(&loaded).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    let mut missing = document.clone();
    missing["profiles"][0]["target"]
        .as_object_mut()
        .unwrap()
        .remove("root_session_id");
    std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    assert!(
        store.load().is_err(),
        "cannot reinterpret a partial old target as a project"
    );
    document["profiles"][0]["target"]["workspace_root"] = json!("relative/subfolder");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(store.load().is_err());
}

#[test]
fn duplicate_json_owners_are_rejected_during_legacy_and_current_load() {
    let (_directory, path, store) = store_fixture();
    for version in [1, 2, 3] {
        let bytes = format!(
            "{{\"schema_version\":{version},\"revision\":1,\"revision\":2,\"profiles\":[]}}"
        );
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(store.load(), Err(PublishError::InvalidDocument)));
    }
}

#[test]
fn unpaired_profiles_never_accept_secret_fields_in_legacy_or_current_documents() {
    let (_directory, path, store) = store_fixture();
    for version in [1, 2, 3] {
        let (mut document, _) = legacy_document();
        if version != 1 {
            document["schema_version"] = json!(version);
            document["profiles"][0]["target"]["kind"] = json!("legacy_session");
        }
        document["profiles"][0]["enabled"] = json!(false);
        document["profiles"][0]["authentication"] =
            json!({"kind":"unpaired", "token":"must-not-enter-profile"});
        let original = serde_json::to_vec(&document).unwrap();
        std::fs::write(&path, &original).unwrap();
        let error = store
            .load()
            .expect_err("unpaired auth rejects unknown secrets");
        assert!(matches!(error, PublishError::InvalidDocument));
        assert!(!error.to_string().contains("must-not-enter-profile"));
        assert!(store.save(&PublishProfileSet::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[test]
fn schema_two_migration_retains_read_authority_and_rejects_injected_agent_fields() {
    let (_directory, path, store) = store_fixture();
    let profile = paired_profile();
    let mut document = serde_json::to_value(profile_set(profile.clone())).unwrap();
    document["schema_version"] = json!(2);
    document["revision"] = json!(17);
    for field in ["mode", "tls"] {
        document["profiles"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    let bytes = serde_json::to_vec(&document).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let migrated = store.load().unwrap();
    assert_eq!(migrated.schema_version, 3);
    assert_eq!(migrated.revision, 17);
    assert_eq!(migrated.profiles, vec![profile]);
    assert_eq!(migrated.profiles[0].mode, PublishMode::ReadTools {});
    assert_eq!(migrated.profiles[0].tls, None);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let saved = store.save(&migrated).unwrap();
    assert_eq!(saved.revision, 18);
    assert_eq!(store.load().unwrap(), saved);

    for version in [1, 2] {
        let mut invalid = if version == 1 {
            legacy_document().0
        } else {
            document.clone()
        };
        invalid["profiles"][0]["mode"] = json!({"kind":"agent","access_mode":"full_access"});
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            matches!(store.load(), Err(PublishError::InvalidDocument)),
            "schema {version} must not acquire agent authority"
        );
    }
}

#[test]
fn agent_profiles_use_explicit_grants_and_never_reuse_read_descriptors() {
    let mut profile = paired_profile();
    profile.mode = PublishMode::Agent {
        access_mode: crate::config::AccessMode::Default,
    };
    assert!(
        profile.validate().is_err(),
        "read tool selection is not an agent grant"
    );
    profile.tools.clear();
    profile.enabled = true;
    profile.validate().unwrap();
    assert!(profile.has_public_operations());
    assert!(matches!(
        profile.preview_tool_specs(&ToolRegistry::core_agent()),
        Err(PublishError::ToolUnavailable)
    ));
    profile.target = PublishTarget::Temp {};
    profile.validate().unwrap();
    profile.target = PublishTarget::LegacySession {
        project_id: ProjectId::new(),
        root_session_id: SessionId::new(),
        workspace_root: target().workspace_root().unwrap().clone(),
    };
    assert!(
        profile.validate().is_err(),
        "legacy chat authority cannot become an agent target"
    );
    for mode in [
        json!({"kind":"agent"}),
        json!({"kind":"read_tools","access_mode":"full_access"}),
        json!({"kind":"agent","access_mode":"full_access","extra":true}),
    ] {
        assert!(serde_json::from_value::<PublishMode>(mode).is_err());
    }
}

#[test]
fn tls_requires_explicit_bind_and_absolute_key_material_paths() {
    let mut profile = paired_profile();
    profile.bind = "192.168.1.12:7332".parse().unwrap();
    assert!(profile.validate().is_err());
    let root = target().workspace_root().unwrap().clone();
    profile.tls = Some(PublishTls {
        certificate_path: root.join("certificate.der"),
        private_key_path: root.join("private-key.der"),
    });
    profile.validate().unwrap();
    for bind in [
        "0.0.0.0:7332",
        "[::]:7332",
        "224.0.0.1:7332",
        "192.168.1.12:0",
    ] {
        profile.bind = bind.parse().unwrap();
        assert!(profile.validate().is_err());
    }
    profile.bind = "192.168.1.12:7332".parse().unwrap();
    profile.tls.as_mut().unwrap().private_key_path = "relative/key.der".into();
    assert!(profile.validate().is_err());
}
