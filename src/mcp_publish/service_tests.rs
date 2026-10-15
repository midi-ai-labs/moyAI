use super::*;

use serde_json::{Value, json};

use crate::mcp_publish::dispatch::tests::{fixture as canonical_fixture, legacy_target_fields};
use crate::mcp_publish::transport::PROTOCOL_VERSION;
use crate::protocol::ProtocolEventStore;

struct Fixture {
    _directory: tempfile::TempDir,
    path: Utf8PathBuf,
    store: StoreBundle,
    profile: PublishProfile,
    service: PublishService,
    http: reqwest::Client,
}

impl Fixture {
    async fn new() -> Self {
        let (directory, store, profile) = canonical_fixture().await;
        let path =
            Utf8PathBuf::from_path_buf(directory.path().join("application-config/publishing.json"))
                .unwrap();
        let service = PublishService::new(path.clone(), store.clone(), ResolvedConfig::default());
        service.refresh().await.unwrap();
        Self {
            _directory: directory,
            path,
            store,
            profile,
            service,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
        }
    }

    fn draft(&self, label: &str, background: PublishBackgroundPolicy) -> PublishDraft {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        PublishDraft {
            label: label.to_string(),
            bind: reservation.local_addr().unwrap(),
            tls: None,
            mode: PublishMode::ReadTools {},
            target: PublishTarget::Project {
                project_id: legacy_target_fields(&self.profile).0,
                workspace_root: legacy_target_fields(&self.profile).2,
            },
            tools: vec![ToolName::Read, ToolName::CurrentTime],
            max_concurrent_calls: 1,
            background,
        }
    }

    async fn save(&self, label: &str, background: PublishBackgroundPolicy) -> PublishProfileId {
        let previous = self.service.projection_now();
        let saved = self
            .service
            .save(
                None,
                self.draft(label, background),
                &previous.revision,
                &previous.generation,
            )
            .await
            .unwrap();
        saved
            .profiles
            .iter()
            .find(|row| {
                !previous
                    .profiles
                    .iter()
                    .any(|old| old.profile.id == row.profile.id)
            })
            .unwrap()
            .profile
            .id
    }

    async fn save_legacy(&self, label: &str, target: PublishTarget) -> PublishProfileId {
        let mut document = self.service.profiles.load().unwrap();
        let mut profile = self.profile.clone();
        profile.id = PublishProfileId(ulid::Ulid::new());
        profile.label = label.to_string();
        profile.target = target;
        profile.enabled = false;
        profile.authentication = PublishAuthentication::Unpaired {};
        profile.bind = self.draft(label, profile.background).bind;
        let id = profile.id;
        document.profiles.push(profile);
        self.service.profiles.save(&document).unwrap();
        self.service.refresh().await.unwrap();
        id
    }

    async fn issue(&self, id: PublishProfileId) -> PublishTokenReceipt {
        let state = self.service.projection_now();
        self.service
            .issue_token(id, &state.revision, &state.generation)
            .await
            .unwrap()
    }

    async fn start(&self, id: PublishProfileId) -> PublishProjection {
        let state = self.service.projection_now();
        let running = self
            .service
            .start(id, &state.revision, &state.generation)
            .await
            .unwrap();
        assert_eq!(row(&running, id).status, PublishStatus::Running);
        running
    }

    async fn stop(&self, id: PublishProfileId) -> PublishProjection {
        let state = self.service.projection_now();
        self.service
            .stop(id, &state.revision, &state.generation)
            .await
            .unwrap()
    }

    fn post(
        &self,
        endpoint: &str,
        token: &str,
        session: Option<&str>,
        message: Value,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(endpoint)
            .bearer_auth(token)
            .header("accept", "application/json, text/event-stream")
            .json(&message);
        if let Some(session) = session {
            request = request
                .header("mcp-session-id", session)
                .header("mcp-protocol-version", PROTOCOL_VERSION);
        }
        request
    }

    async fn session(&self, endpoint: &str, token: &str) -> String {
        let response = self
            .post(endpoint, token, None, initialize())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let id = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_string();
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            self.post(
                endpoint,
                token,
                Some(&id),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .send()
            .await
            .unwrap()
            .status(),
            202
        );
        id
    }
}

fn row(state: &PublishProjection, id: PublishProfileId) -> &PublishProfileRow {
    state
        .profiles
        .iter()
        .find(|row| row.profile.id == id)
        .unwrap()
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"service-test","version":"1"}
    }})
}

fn assert_plaintext_absent(directory: &camino::Utf8Path, token: &str) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = Utf8PathBuf::from_path_buf(entry.path()).unwrap();
        if entry.file_type().unwrap().is_dir() {
            assert_plaintext_absent(&path, token);
        } else {
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(token.len())
                    .any(|bytes| bytes == token.as_bytes())
            );
        }
    }
}

#[tokio::test]
async fn explicit_save_pair_start_and_real_read_preserve_main_history() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save("Workspace", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let saved = fixture.service.projection_now();
    assert_eq!(row(&saved, id).status, PublishStatus::Stopped);
    assert!(!row(&saved, id).profile.enabled);
    assert!(!row(&saved, id).can_start);
    assert!(
        fixture
            .service
            .start(id, &saved.revision, &saved.generation)
            .await
            .is_err()
    );
    let receipt = fixture.issue(id).await;
    assert!(row(&receipt.projection, id).credential_configured);
    assert!(row(&receipt.projection, id).can_start);
    assert!(
        !serde_json::to_string(&receipt.projection)
            .unwrap()
            .contains(&receipt.token)
    );
    assert_plaintext_absent(fixture.path.parent().unwrap(), &receipt.token);
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.as_deref().unwrap();
    assert!(!row(&running, id).can_edit);
    assert!(!row(&running, id).can_delete);
    assert!(fixture.service.polling_required());
    assert_eq!(
        fixture
            .post(endpoint, "wrong-token", None, initialize())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let session = fixture.session(endpoint, &receipt.token).await;
    let result: Value = fixture.post(endpoint, &receipt.token, Some(&session), json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{"path":"visible.txt"}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(result["result"]["isError"], false);
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("alpha")
    );
    assert!(
        fixture
            .store
            .protocol_event_store()
            .list_history_items_for_session(legacy_target_fields(&fixture.profile).1)
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .store
            .session_repo()
            .has_fresh_run_admission(legacy_target_fields(&fixture.profile).1)
            .await
            .unwrap()
    );
    let stopped = fixture.stop(id).await;
    assert_eq!(row(&stopped, id).status, PublishStatus::Stopped);
    assert!(!row(&stopped, id).profile.enabled);
    assert!(!fixture.service.polling_required());
    assert!(
        fixture
            .post(endpoint, &receipt.token, None, initialize())
            .send()
            .await
            .is_err()
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn saved_enabled_intent_does_not_open_a_listener_on_restart() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Restart",
            PublishBackgroundPolicy::KeepWhileApplicationRunning,
        )
        .await;
    let receipt = fixture.issue(id).await;
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.clone().unwrap();
    assert!(fixture.service.shutdown().await);
    let retained_settings = std::fs::read(&fixture.path).unwrap();
    let restarted = PublishService::new(
        fixture.path.clone(),
        fixture.store.clone(),
        ResolvedConfig::default(),
    );
    let state = restarted.projection_now();
    assert_eq!(row(&state, id).status, PublishStatus::Stopped);
    assert!(row(&state, id).endpoint.is_none());
    assert!(row(&state, id).credential_configured);
    assert!(!restarted.polling_required());
    restarted.refresh().await.unwrap();
    assert_eq!(std::fs::read(&fixture.path).unwrap(), retained_settings);
    assert!(
        fixture
            .post(&endpoint, &receipt.token, None, initialize())
            .send()
            .await
            .is_err()
    );
    assert!(restarted.shutdown().await);
}

#[tokio::test]
async fn token_rotation_rejects_old_token_and_running_revocation_closes_server() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save("Rotation", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let first = fixture.issue(id).await;
    let second = fixture.issue(id).await;
    assert_ne!(first.token, second.token);
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.clone().unwrap();
    assert_eq!(
        fixture
            .post(&endpoint, &first.token, None, initialize())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    fixture.session(&endpoint, &second.token).await;
    let state = fixture.service.projection_now();
    let revoked = fixture
        .service
        .revoke_token(id, &state.revision, &state.generation)
        .await
        .unwrap();
    assert_eq!(row(&revoked, id).status, PublishStatus::Stopped);
    assert_eq!(
        row(&revoked, id).profile.authentication,
        PublishAuthentication::Unpaired {}
    );
    assert!(!row(&revoked, id).credential_configured);
    assert!(!row(&revoked, id).can_start);
    assert!(
        fixture
            .post(&endpoint, &second.token, None, initialize())
            .send()
            .await
            .is_err()
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn automatically_stopped_profile_does_not_reserve_its_bind_for_other_profiles() {
    let fixture = Fixture::new().await;
    let first = fixture
        .save("First", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    fixture.issue(first).await;
    let running = fixture.start(first).await;
    let bind = row(&running, first).profile.bind;
    let mut draft = fixture.draft("Second", PublishBackgroundPolicy::StopWhenWindowCloses);
    draft.bind = bind;
    let saved = fixture
        .service
        .save(None, draft, &running.revision, &running.generation)
        .await
        .unwrap();
    let second = saved
        .profiles
        .iter()
        .find(|row| row.profile.id != first)
        .unwrap()
        .profile
        .id;
    fixture.issue(second).await;

    let ids = fixture.service.window_hide_requested();
    fixture.service.finish_window_hide(ids).await;
    fixture.service.window_shown();
    let stopped = fixture.service.projection_now();
    assert!(!row(&stopped, first).can_stop);
    assert_eq!(row(&stopped, first).status, PublishStatus::Stopped);
    let resumed = fixture.start(second).await;
    assert_eq!(row(&resumed, second).profile.bind, bind);
    assert_eq!(row(&resumed, first).status, PublishStatus::Stopped);
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn real_bind_collision_preserves_running_owner_and_retry_succeeds_after_release() {
    let fixture = Fixture::new().await;
    let first = fixture
        .save(
            "Port owner",
            PublishBackgroundPolicy::KeepWhileApplicationRunning,
        )
        .await;
    let first_token = fixture.issue(first).await.token;
    let running = fixture.start(first).await;
    let endpoint = row(&running, first).endpoint.clone().unwrap();
    let mut draft = fixture.draft(
        "Port contender",
        PublishBackgroundPolicy::KeepWhileApplicationRunning,
    );
    draft.bind = row(&running, first).profile.bind;
    let saved = fixture
        .service
        .save(None, draft, &running.revision, &running.generation)
        .await
        .unwrap();
    let second = saved
        .profiles
        .iter()
        .find(|row| row.profile.id != first)
        .unwrap()
        .profile
        .id;
    let receipt = fixture.issue(second).await;
    let failed = fixture
        .service
        .start(
            second,
            &receipt.projection.revision,
            &receipt.projection.generation,
        )
        .await
        .expect("runtime bind failure is projected");
    assert_eq!(row(&failed, second).status, PublishStatus::Error);
    assert_eq!(row(&failed, first).status, PublishStatus::Running);
    fixture.session(&endpoint, &first_token).await;
    fixture.stop(first).await;
    let retry = fixture.start(second).await;
    let second_endpoint = row(&retry, second).endpoint.as_deref().unwrap();
    assert_eq!(second_endpoint, endpoint);
    fixture.session(second_endpoint, &receipt.token).await;
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn stale_generation_and_noncanonical_revisions_cannot_mutate_running_profile() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save("CAS", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let issued = fixture.issue(id).await;
    let running = fixture.start(id).await;
    assert!(
        fixture
            .service
            .stop(id, &running.revision, &issued.projection.generation)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .stop(id, &format!("0{}", running.revision), &running.generation)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .delete(id, &running.revision, &running.generation)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .issue_token(id, &running.revision, &running.generation)
            .await
            .is_err()
    );
    assert_eq!(
        row(&fixture.service.projection_now(), id).status,
        PublishStatus::Running
    );
    fixture.stop(id).await;
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn external_profile_cas_rejects_token_rotation_without_replacing_current_verifier() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save("Original", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let issued = fixture.issue(id).await;
    let PublishAuthentication::LocalCredential { credential_id } =
        row(&issued.projection, id).profile.authentication
    else {
        panic!("paired")
    };
    let digest = fixture
        .service
        .credentials
        .verifier(id, credential_id)
        .unwrap();
    let separate_store = PublishProfileStore::new(fixture.path.clone());
    let mut external = separate_store.load().unwrap();
    external.profiles[0].label = "External save".to_string();
    separate_store.save(&external).unwrap();
    assert!(
        fixture
            .service
            .issue_token(
                id,
                &issued.projection.revision,
                &issued.projection.generation
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .credentials
            .verifier(id, credential_id)
            .unwrap(),
        digest
    );
    assert_eq!(
        separate_store.load().unwrap().profiles[0].label,
        "External save"
    );
    let refreshed = fixture.service.refresh().await.unwrap();
    assert_eq!(row(&refreshed, id).profile.label, "External save");
    assert!(row(&refreshed, id).credential_configured);
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn hiding_stops_default_profile_but_retains_explicit_background_until_shutdown() {
    let fixture = Fixture::new().await;
    let stop_id = fixture
        .save(
            "Window owned",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    fixture.issue(stop_id).await;
    let stop_running = fixture.start(stop_id).await;
    let stop_endpoint = row(&stop_running, stop_id).endpoint.clone().unwrap();
    let keep_id = fixture
        .save(
            "Application owned",
            PublishBackgroundPolicy::KeepWhileApplicationRunning,
        )
        .await;
    let keep_token = fixture.issue(keep_id).await.token;
    let keep_running = fixture.start(keep_id).await;
    let keep_endpoint = row(&keep_running, keep_id).endpoint.clone().unwrap();
    let ids = fixture.service.window_hide_requested();
    let requested = fixture.service.projection_now();
    assert_eq!(row(&requested, stop_id).status, PublishStatus::Stopping);
    assert_eq!(row(&requested, keep_id).status, PublishStatus::Running);
    fixture.service.finish_window_hide(ids).await;
    let hidden = fixture.service.projection_now();
    assert_eq!(row(&hidden, stop_id).status, PublishStatus::Stopped);
    assert!(!row(&hidden, stop_id).can_start);
    assert!(
        fixture
            .service
            .start(stop_id, &hidden.revision, &hidden.generation)
            .await
            .is_err()
    );
    assert!(
        fixture
            .post(&stop_endpoint, "irrelevant", None, initialize())
            .send()
            .await
            .is_err()
    );
    fixture.session(&keep_endpoint, &keep_token).await;
    fixture.service.window_shown();
    assert!(row(&fixture.service.projection_now(), stop_id).can_start);
    fixture.service.begin_shutdown();
    assert!(!row(&fixture.service.projection_now(), keep_id).can_edit);
    assert!(fixture.service.shutdown().await);
    assert!(
        fixture
            .post(&keep_endpoint, &keep_token, None, initialize())
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn synchronous_shutdown_cancels_admission_while_a_command_waits_for_its_lane() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Queued start",
            PublishBackgroundPolicy::KeepWhileApplicationRunning,
        )
        .await;
    let receipt = fixture.issue(id).await;
    let lane = fixture.service.commands.lock().await;
    let service = fixture.service.clone();
    let state = receipt.projection;
    let pending =
        tokio::spawn(async move { service.start(id, &state.revision, &state.generation).await });
    fixture.service.begin_shutdown();
    assert!(!row(&fixture.service.projection_now(), id).can_start);
    drop(lane);
    assert!(pending.await.unwrap().is_err());
    assert_eq!(
        row(&fixture.service.projection_now(), id).status,
        PublishStatus::Stopped
    );
    assert!(!fixture.service.polling_required());
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn corrupt_profile_document_is_visible_and_not_replaced_by_save() {
    let fixture = Fixture::new().await;
    std::fs::create_dir_all(fixture.path.parent().unwrap()).unwrap();
    let corrupt = b"{broken-publish-document";
    std::fs::write(&fixture.path, corrupt).unwrap();
    let service = PublishService::new(
        fixture.path.clone(),
        fixture.store.clone(),
        ResolvedConfig::default(),
    );
    let state = service.projection_now();
    assert!(state.error.is_some());
    assert!(state.profiles.is_empty());
    assert!(
        service
            .save(
                None,
                fixture.draft(
                    "Cannot overwrite",
                    PublishBackgroundPolicy::StopWhenWindowCloses
                ),
                &state.revision,
                &state.generation
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&fixture.path).unwrap(), corrupt);
    assert!(service.refresh().await.is_err());
    assert!(service.shutdown().await);
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn independent_profile_delete_removes_only_its_credential_and_configuration() {
    let fixture = Fixture::new().await;
    let first = fixture
        .save("Same label", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let first_receipt = fixture.issue(first).await;
    let second = fixture
        .save("Same label", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    fixture.issue(second).await;
    let PublishAuthentication::LocalCredential { credential_id } =
        row(&first_receipt.projection, first).profile.authentication
    else {
        panic!("paired")
    };
    let state = fixture.service.projection_now();
    let deleted = fixture
        .service
        .delete(first, &state.revision, &state.generation)
        .await
        .unwrap();
    assert_eq!(deleted.profiles.len(), 1);
    assert_eq!(deleted.profiles[0].profile.id, second);
    assert!(deleted.profiles[0].credential_configured);
    assert!(
        fixture
            .service
            .credentials
            .verifier(first, credential_id)
            .is_err()
    );
    assert_eq!(
        PublishProfileStore::new(fixture.path.clone())
            .load()
            .unwrap()
            .profiles
            .len(),
        1
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn refresh_retains_a_saved_valid_target_outside_the_recent_session_page() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save_legacy("Older workspace", fixture.profile.target.clone())
        .await;
    // Age this fixture record explicitly so pagination never depends on the
    // wall-clock precision or the ordering of freshly generated session IDs.
    rusqlite::Connection::open(&fixture.store.paths().database_path)
        .unwrap()
        .execute(
            "UPDATE sessions SET created_at_ms = 1, updated_at_ms = 1 WHERE id = ?1",
            [legacy_target_fields(&fixture.profile).1.to_string()],
        )
        .unwrap();
    for index in 0..100 {
        fixture
            .store
            .session_repo()
            .create_session(crate::session::NewSession {
                project_id: legacy_target_fields(&fixture.profile).0,
                title: format!("Newer root chat {index}"),
                cwd: legacy_target_fields(&fixture.profile).2.clone(),
                model: "unused-model".to_string(),
                base_url: "http://127.0.0.1:9/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
                provider_connection: None,
            })
            .await
            .unwrap();
    }
    let recent = fixture
        .store
        .session_repo()
        .list_recent_sessions(100)
        .await
        .unwrap();
    assert_eq!(recent.len(), 100);
    assert!(
        recent
            .iter()
            .all(|session| session.id != legacy_target_fields(&fixture.profile).1)
    );
    validate_target(
        &fixture.store,
        &fixture.profile.target,
        &fixture.service.protected_roots,
    )
    .await
    .expect("the saved workspace and root session still exist and remain authorized");

    let refreshed = fixture.service.refresh().await.unwrap();
    assert_eq!(row(&refreshed, id).profile.target, fixture.profile.target);
    assert!(
        refreshed
            .targets
            .iter()
            .any(|choice| choice.target == fixture.profile.target),
        "a saved valid target must remain selectable so the GUI can save an unrelated label edit"
    );
    let mut renamed = fixture.draft(
        "Renamed workspace",
        PublishBackgroundPolicy::StopWhenWindowCloses,
    );
    renamed.bind = row(&refreshed, id).profile.bind;
    renamed.target = fixture.profile.target.clone();
    let saved = fixture
        .service
        .save(
            Some(id),
            renamed,
            &refreshed.revision,
            &refreshed.generation,
        )
        .await
        .unwrap();
    assert_eq!(row(&saved, id).profile.label, "Renamed workspace");
    assert_eq!(row(&saved, id).profile.target, fixture.profile.target);
    assert_eq!(row(&saved, id).status, PublishStatus::Stopped);
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn refresh_deduplicates_saved_legacy_targets_without_exposing_other_chats() {
    let fixture = Fixture::new().await;
    fixture
        .save_legacy("First", fixture.profile.target.clone())
        .await;
    fixture
        .save_legacy("Second", fixture.profile.target.clone())
        .await;
    let refreshed = fixture.service.refresh().await.unwrap();
    assert_eq!(refreshed.profiles.len(), 2);
    assert_eq!(refreshed.targets.len(), 3);
    assert_eq!(
        refreshed
            .targets
            .iter()
            .filter(|choice| choice.target == fixture.profile.target)
            .count(),
        1
    );
    assert!(
        refreshed
            .targets
            .iter()
            .any(|choice| choice.target == PublishTarget::Temp {})
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn refresh_does_not_restore_deleted_or_changed_saved_targets() {
    for change in ["deleted", "cwd_changed"] {
        let fixture = Fixture::new().await;
        let id = fixture
            .save_legacy("Saved target", fixture.profile.target.clone())
            .await;
        let changed_cwd = legacy_target_fields(&fixture.profile).2.join("nested");
        if change == "deleted" {
            fixture
                .store
                .session_repo()
                .delete_session(legacy_target_fields(&fixture.profile).1)
                .await
                .unwrap();
        } else {
            rusqlite::Connection::open(&fixture.store.paths().database_path)
                .unwrap()
                .execute(
                    "UPDATE sessions SET cwd_path = ?1 WHERE id = ?2",
                    rusqlite::params![
                        changed_cwd.as_str(),
                        legacy_target_fields(&fixture.profile).1.to_string(),
                    ],
                )
                .unwrap();
        }
        assert!(
            validate_target(
                &fixture.store,
                &fixture.profile.target,
                &fixture.service.protected_roots,
            )
            .await
            .is_err(),
            "{change} invalidates the saved authority"
        );
        let refreshed = fixture.service.refresh().await.unwrap();
        assert_eq!(row(&refreshed, id).profile.target, fixture.profile.target);
        assert!(
            refreshed
                .targets
                .iter()
                .all(|choice| choice.target != fixture.profile.target),
            "{change} must not regain authority from a saved profile"
        );
        assert!(
            refreshed
                .targets
                .iter()
                .all(|choice| !matches!(&choice.target,
            PublishTarget::LegacySession { root_session_id, .. }
                if *root_session_id == legacy_target_fields(&fixture.profile).1)),
            "a changed legacy chat must not reappear with automatically widened or rebased authority"
        );
        let mut draft = fixture.draft(
            "Cannot reuse target",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        );
        draft.target = fixture.profile.target.clone();
        assert!(
            fixture
                .service
                .save(Some(id), draft, &refreshed.revision, &refreshed.generation,)
                .await
                .is_err(),
            "{change} must also fail the save-time target check"
        );
        assert!(fixture.service.shutdown().await);
    }
}

#[tokio::test]
async fn saved_legacy_scope_keeps_canonical_chat_workspace_and_excludes_other_chats() {
    let fixture = Fixture::new().await;
    let mut side = ResolvedConfig::default().side_chat;
    side.base_url = "http://127.0.0.1:9/v1".to_string();
    side.model = "unused-side-model".to_string();
    let binding = fixture
        .store
        .side_chat_repo()
        .ensure(
            legacy_target_fields(&fixture.profile).1,
            crate::storage::SideChatProviderTarget::try_from(&side).unwrap(),
        )
        .unwrap();
    let nested = legacy_target_fields(&fixture.profile).2.join("nested");
    let child_workspace_session = fixture
        .store
        .session_repo()
        .create_session(crate::session::NewSession {
            project_id: legacy_target_fields(&fixture.profile).0,
            title: "Nested workspace root chat".to_string(),
            cwd: nested.clone(),
            model: "unused-model".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            access_mode: crate::config::AccessMode::Default,
            provider_connection: None,
        })
        .await
        .unwrap();
    let target = PublishTarget::LegacySession {
        project_id: legacy_target_fields(&fixture.profile).0,
        root_session_id: child_workspace_session.id,
        workspace_root: nested.clone(),
    };
    let id = fixture
        .save_legacy("Nested publication", target.clone())
        .await;
    let state = fixture.service.refresh().await.unwrap();
    assert!(
        state
            .targets
            .iter()
            .all(|choice| choice.target != fixture.profile.target)
    );
    assert!(state.targets.iter().any(|choice| choice.target == target));
    assert!(!state.targets.iter().any(
        |choice| matches!(&choice.target, PublishTarget::LegacySession { root_session_id, .. }
                if *root_session_id == binding.conversation_session_id)
    ));
    let token = fixture.issue(id).await.token;
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.as_deref().unwrap();
    let session = fixture.session(endpoint, &token).await;
    let inside: Value = fixture.post(endpoint, &token, Some(&session), json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{"path":"other.txt"}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(inside["result"]["isError"], false);
    for (request_id, path) in [
        (3, "../visible.txt".to_string()),
        (
            4,
            legacy_target_fields(&fixture.profile)
                .2
                .join("visible.txt")
                .to_string(),
        ),
    ] {
        let outside: Value = fixture.post(endpoint, &token, Some(&session), json!({"jsonrpc":"2.0","id":request_id,"method":"tools/call","params":{"name":"read","arguments":{"path":path}}})).send().await.unwrap().json().await.unwrap();
        assert_eq!(outside["error"]["code"], -32602);
        assert!(outside.get("result").is_none());
    }
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn project_without_chats_can_save_start_and_read_without_creating_a_session() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .session_repo()
        .delete_session(legacy_target_fields(&fixture.profile).1)
        .await
        .unwrap();
    let target = fixture
        .draft("Project", PublishBackgroundPolicy::StopWhenWindowCloses)
        .target;
    let refreshed = fixture.service.refresh().await.unwrap();
    assert!(
        refreshed
            .targets
            .iter()
            .any(|choice| choice.target == target)
    );
    assert!(
        fixture
            .store
            .session_repo()
            .list_recent_sessions(10)
            .await
            .unwrap()
            .is_empty()
    );

    let id = fixture
        .save(
            "Chatless project",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    let saved = fixture.service.projection_now();
    assert_eq!(row(&saved, id).status, PublishStatus::Stopped);
    assert!(!row(&saved, id).profile.enabled);
    assert_eq!(row(&saved, id).profile.target, target);
    let receipt = fixture.issue(id).await;
    assert!(row(&receipt.projection, id).can_start);
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.as_deref().unwrap();
    let session = fixture.session(endpoint, &receipt.token).await;
    let result: Value = fixture.post(endpoint, &receipt.token, Some(&session), json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{"path":"visible.txt"}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(result["result"]["isError"], false);
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("alpha")
    );
    assert!(
        fixture
            .store
            .session_repo()
            .list_recent_sessions(10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .store
            .project_repo()
            .list_projects(10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn temp_can_save_and_serve_only_time_without_a_project_or_target_refresh() {
    let mut fixture = Fixture::new().await;
    fixture
        .store
        .project_repo()
        .delete_project(legacy_target_fields(&fixture.profile).0)
        .await
        .unwrap();
    fixture.service = PublishService::new(
        fixture.path.clone(),
        fixture.store.clone(),
        ResolvedConfig::default(),
    );
    let initial = fixture.service.projection_now();
    assert_eq!(initial.targets.len(), 1);
    assert_eq!(initial.targets[0].target, PublishTarget::Temp {});
    let mut draft = fixture.draft("一時利用", PublishBackgroundPolicy::StopWhenWindowCloses);
    draft.target = PublishTarget::Temp {};
    draft.tools = vec![ToolName::CurrentTime];
    let saved = fixture
        .service
        .save(None, draft.clone(), &initial.revision, &initial.generation)
        .await
        .unwrap();
    let id = saved.profiles[0].profile.id;
    assert_eq!(row(&saved, id).status, PublishStatus::Stopped);
    let token = fixture.issue(id).await;
    assert!(row(&token.projection, id).can_start);
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.as_deref().unwrap();
    let session = fixture.session(endpoint, &token.token).await;
    let list: Value = fixture
        .post(
            endpoint,
            &token.token,
            Some(&session),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(list["result"]["tools"][0]["name"], "current_time");
    let time: Value = fixture.post(endpoint, &token.token, Some(&session), json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"current_time","arguments":{}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(time["result"]["isError"], false);
    let read: Value = fixture.post(endpoint, &token.token, Some(&session), json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read","arguments":{"path":legacy_target_fields(&fixture.profile).2.join("visible.txt")}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(read["error"]["code"], -32602);
    fixture.stop(id).await;
    let stopped = fixture.service.projection_now();
    draft.tools.push(ToolName::Read);
    assert!(
        fixture
            .service
            .save(Some(id), draft, &stopped.revision, &stopped.generation)
            .await
            .is_err()
    );
    assert_eq!(fixture.service.projection_now().revision, stopped.revision);
    assert!(
        fixture
            .store
            .project_repo()
            .list_projects(10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .store
            .session_repo()
            .list_recent_sessions(10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn legacy_target_only_continues_its_own_saved_profile_until_explicit_reselection() {
    let fixture = Fixture::new().await;
    let legacy_id = fixture
        .save_legacy("Legacy", fixture.profile.target.clone())
        .await;
    let project_id = fixture
        .save("Project", PublishBackgroundPolicy::StopWhenWindowCloses)
        .await;
    let mut legacy_draft = fixture.draft(
        "Legacy rename",
        PublishBackgroundPolicy::StopWhenWindowCloses,
    );
    legacy_draft.target = fixture.profile.target.clone();
    for target_id in [None, Some(project_id)] {
        let before = fixture.service.projection_now();
        assert!(
            fixture
                .service
                .save(
                    target_id,
                    legacy_draft.clone(),
                    &before.revision,
                    &before.generation
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.service.projection_now().revision, before.revision);
    }
    let before = fixture.service.projection_now();
    let continued = fixture
        .service
        .save(
            Some(legacy_id),
            legacy_draft.clone(),
            &before.revision,
            &before.generation,
        )
        .await
        .unwrap();
    assert_eq!(
        row(&continued, legacy_id).profile.target,
        fixture.profile.target
    );
    let project_draft = fixture.draft(
        "Explicit project scope",
        PublishBackgroundPolicy::StopWhenWindowCloses,
    );
    let reselected = fixture
        .service
        .save(
            Some(legacy_id),
            project_draft.clone(),
            &continued.revision,
            &continued.generation,
        )
        .await
        .unwrap();
    assert_eq!(
        row(&reselected, legacy_id).profile.target,
        project_draft.target
    );
    assert!(
        fixture
            .service
            .save(
                Some(legacy_id),
                legacy_draft,
                &reselected.revision,
                &reselected.generation
            )
            .await
            .is_err()
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn refresh_excludes_private_desktop_projects_without_filtering_user_folder_names() {
    let fixture = Fixture::new().await;
    for name in [
        "quick-chat-workspace",
        "desktop-workspace",
        "desktop-workspace-after-delete",
        "desktop-workspace-after-delete-2",
    ] {
        let root = fixture.store.paths().data_dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &root,
            &ResolvedConfig::default(),
        )
        .unwrap();
        fixture
            .store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, name, "none")
            .await
            .unwrap();
    }
    let user_root = legacy_target_fields(&fixture.profile)
        .2
        .join("quick-chat-workspace");
    std::fs::create_dir_all(&user_root).unwrap();
    let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
        &user_root,
        &ResolvedConfig::default(),
    )
    .unwrap();
    fixture
        .store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "User project",
            "none",
        )
        .await
        .unwrap();
    let state = fixture.service.refresh().await.unwrap();
    assert!(state.targets.iter().any(|choice| matches!(&choice.target, PublishTarget::Project { project_id, .. } if *project_id == workspace.project_id)));
    assert!(state.targets.iter().all(|choice| {
        choice
            .target
            .workspace_root()
            .is_none_or(|root| !root.starts_with(&fixture.store.paths().data_dir))
    }));
    assert_eq!(
        state
            .targets
            .iter()
            .filter(|choice| choice.target == PublishTarget::Temp {})
            .count(),
        1
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn saved_project_does_not_rebase_after_deletion_or_root_change() {
    for change in ["deleted", "root_changed"] {
        let fixture = Fixture::new().await;
        let id = fixture
            .save("Project", PublishBackgroundPolicy::StopWhenWindowCloses)
            .await;
        fixture.issue(id).await;
        let draft = fixture.draft(
            "Keep exact project",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        );
        if change == "deleted" {
            fixture
                .store
                .project_repo()
                .delete_project(legacy_target_fields(&fixture.profile).0)
                .await
                .unwrap();
        } else {
            rusqlite::Connection::open(&fixture.store.paths().database_path)
                .unwrap()
                .execute(
                    "UPDATE projects SET root_path = ?1 WHERE id = ?2",
                    rusqlite::params![
                        legacy_target_fields(&fixture.profile)
                            .2
                            .join("nested")
                            .as_str(),
                        legacy_target_fields(&fixture.profile).0.to_string()
                    ],
                )
                .unwrap();
        }
        let state = fixture.service.refresh().await.unwrap();
        assert_eq!(row(&state, id).profile.target, draft.target);
        assert!(!row(&state, id).can_start);
        assert!(
            state
                .targets
                .iter()
                .all(|choice| choice.target != draft.target)
        );
        assert!(
            fixture
                .service
                .save(Some(id), draft, &state.revision, &state.generation)
                .await
                .is_err()
        );
        let rejected = fixture
            .service
            .start(id, &state.revision, &state.generation)
            .await
            .unwrap();
        assert_eq!(row(&rejected, id).status, PublishStatus::Error);
        assert!(row(&rejected, id).endpoint.is_none());
        assert!(!row(&rejected, id).profile.enabled);
        assert!(fixture.service.shutdown().await);
    }
}

#[tokio::test]
async fn changing_publication_mode_or_agent_permission_revokes_the_old_credential() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Read profile",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    let receipt = fixture.issue(id).await;
    let mut old_auth = row(&receipt.projection, id).profile.authentication;
    let mut current = receipt.projection;
    for mode in [
        PublishMode::Agent {
            access_mode: crate::config::AccessMode::Default,
        },
        PublishMode::Agent {
            access_mode: crate::config::AccessMode::FullAccess,
        },
        PublishMode::ReadTools {},
    ] {
        let mut draft = fixture.draft(
            "Explicit grant change",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        );
        draft.mode = mode;
        if matches!(mode, PublishMode::Agent { .. }) {
            draft.tools.clear();
        }
        let changed = fixture
            .service
            .save(Some(id), draft, &current.revision, &current.generation)
            .await
            .unwrap();
        assert_eq!(row(&changed, id).profile.mode, mode);
        assert_eq!(
            row(&changed, id).profile.authentication,
            PublishAuthentication::Unpaired {}
        );
        assert!(!row(&changed, id).credential_configured);
        assert!(!row(&changed, id).can_start);
        assert!(!row(&changed, id).profile.enabled);
        let PublishAuthentication::LocalCredential { credential_id } = old_auth else {
            panic!("paired old grant")
        };
        assert!(
            fixture
                .service
                .credentials
                .verifier(id, credential_id)
                .is_err()
        );
        let issued = fixture.issue(id).await;
        assert_ne!(row(&issued.projection, id).profile.authentication, old_auth);
        old_auth = row(&issued.projection, id).profile.authentication;
        current = issued.projection;
    }
    let mut rename = fixture.draft("Only rename", PublishBackgroundPolicy::StopWhenWindowCloses);
    rename.bind = row(&current, id).profile.bind;
    let renamed = fixture
        .service
        .save(Some(id), rename, &current.revision, &current.generation)
        .await
        .unwrap();
    assert_eq!(row(&renamed, id).profile.authentication, old_auth);
    assert!(row(&renamed, id).credential_configured);
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn invalid_or_stale_mode_change_does_not_revoke_the_current_read_token() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Read profile",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    let issued = fixture.issue(id).await;
    let PublishAuthentication::LocalCredential { credential_id } =
        row(&issued.projection, id).profile.authentication
    else {
        panic!("paired")
    };
    let digest = fixture
        .service
        .credentials
        .verifier(id, credential_id)
        .unwrap();
    let mut draft = fixture.draft(
        "Agent profile",
        PublishBackgroundPolicy::StopWhenWindowCloses,
    );
    draft.mode = PublishMode::Agent {
        access_mode: crate::config::AccessMode::FullAccess,
    };
    assert!(
        fixture
            .service
            .save(
                Some(id),
                draft.clone(),
                &issued.projection.revision,
                &issued.projection.generation
            )
            .await
            .is_err(),
        "read selections cannot silently become an agent grant"
    );
    assert_eq!(
        fixture
            .service
            .credentials
            .verifier(id, credential_id)
            .unwrap(),
        digest
    );

    let separate = PublishProfileStore::new(fixture.path.clone());
    let mut external = separate.load().unwrap();
    external.profiles[0].label = "Another window's save".into();
    separate.save(&external).unwrap();
    draft.tools.clear();
    assert!(
        fixture
            .service
            .save(
                Some(id),
                draft,
                &issued.projection.revision,
                &issued.projection.generation
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .credentials
            .verifier(id, credential_id)
            .unwrap(),
        digest
    );
    assert_eq!(
        separate.load().unwrap().profiles[0].mode,
        PublishMode::ReadTools {}
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn certificate_generation_is_an_unsaved_draft_and_keeps_existing_material() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Paired read profile",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    let issued = fixture.issue(id).await;
    let before = fixture.service.projection_now();
    let first = fixture
        .service
        .create_certificate(
            id,
            "127.0.0.1".parse().unwrap(),
            &before.revision,
            &before.generation,
        )
        .await
        .unwrap();
    let after = fixture.service.projection_now();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.generation, before.generation);
    assert_eq!(row(&after, id).profile.tls, None);
    assert_eq!(
        row(&after, id).profile.authentication,
        row(&issued.projection, id).profile.authentication
    );
    assert!(!first.certificate_pem.contains("PRIVATE KEY"));
    assert!(
        first
            .tls
            .private_key_path
            .starts_with(&fixture.service.tls_directory)
    );
    let first_bytes = std::fs::read(&first.tls.certificate_path).unwrap();
    let mut draft = fixture.draft("Saved TLS", PublishBackgroundPolicy::StopWhenWindowCloses);
    draft.tls = Some(first.tls.clone());
    let saved = fixture
        .service
        .save(Some(id), draft, &after.revision, &after.generation)
        .await
        .unwrap();
    let loaded = fixture
        .service
        .certificate(id, &saved.revision, &saved.generation)
        .await
        .unwrap();
    assert_eq!(loaded.sha256, first.sha256);
    let second = fixture
        .service
        .create_certificate(
            id,
            "127.0.0.1".parse().unwrap(),
            &saved.revision,
            &saved.generation,
        )
        .await
        .unwrap();
    assert_ne!(second.tls, first.tls);
    assert_eq!(
        std::fs::read(&first.tls.certificate_path).unwrap(),
        first_bytes
    );
    assert_eq!(fixture.service.projection_now().revision, saved.revision);
    assert_eq!(
        row(&fixture.service.projection_now(), id).profile.tls,
        Some(first.tls)
    );
    assert!(fixture.service.shutdown().await);
}

#[tokio::test]
async fn a_read_profile_cannot_publish_imported_private_keys_from_other_profiles() {
    let fixture = Fixture::new().await;
    let id = fixture
        .save(
            "Workspace reads",
            PublishBackgroundPolicy::StopWhenWindowCloses,
        )
        .await;
    let key_path = legacy_target_fields(&fixture.profile)
        .2
        .join("server-private.key");
    std::fs::write(&key_path, "private-key-must-stay-local").unwrap();
    let mut other = fixture.draft(
        "TLS material owner",
        PublishBackgroundPolicy::StopWhenWindowCloses,
    );
    other.tls = Some(PublishTls {
        certificate_path: legacy_target_fields(&fixture.profile)
            .2
            .join("server-cert.pem"),
        private_key_path: key_path,
    });
    let state = fixture.service.projection_now();
    fixture
        .service
        .save(None, other, &state.revision, &state.generation)
        .await
        .unwrap();
    let issued = fixture.issue(id).await;
    let running = fixture.start(id).await;
    let endpoint = row(&running, id).endpoint.as_deref().unwrap();
    let session = fixture.session(endpoint, &issued.token).await;
    let key: Value = fixture.post(endpoint, &issued.token, Some(&session), json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{"path":"server-private.key"}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(key["error"]["code"], -32602);
    assert!(!key.to_string().contains("private-key-must-stay-local"));
    let visible: Value = fixture.post(endpoint, &issued.token, Some(&session), json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read","arguments":{"path":"visible.txt"}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(visible["result"]["isError"], false);
    assert!(fixture.service.shutdown().await);
}
