use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::routing::any;
use camino::Utf8PathBuf;
use tokio::sync::Notify;

use super::*;
use crate::cli::ConfirmationPrompt;
use crate::config::{AccessMode, ResolvedConfig};
use crate::edit::{ChangeTracker, EditSafety, Formatter};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, HistoryScope, ReviewDecision,
    TurnId,
};
use crate::runtime::RunControl;
use crate::session::{
    AdmissionId, NewSession, ProjectId, ProjectRepository, SessionContext, SessionId,
    SessionRepository, ToolCallId,
};
use crate::storage::{
    BeginPermissionReview, PermissionRetryFenceKey, PermissionRetryFenceOutcome,
    PermissionRetryFenceState, PermissionReviewLease, PermissionReviewTransition, SqliteStore,
    StoragePaths, StoreBundle,
};
use crate::tool::context::{RunMutationFence, ToolServices};
use crate::tool::permission_guardian::{
    PermissionGuardian, PermissionGuardianAssessment, PermissionGuardianError, PermissionRiskLevel,
};
use crate::tool::truncate::ToolTruncator;
use crate::workspace::WorkspaceDiscovery;

#[derive(Default)]
struct HumanPrompt(usize);

impl ConfirmationPrompt for HumanPrompt {
    fn confirm(
        &mut self,
        _request: &crate::tool::PermissionRequest,
    ) -> Result<ReviewDecision, crate::error::CliPromptError> {
        self.0 += 1;
        Ok(ReviewDecision::Denied)
    }
}

fn seed_user(store: &StoreBundle, session_id: SessionId, sequence: u64) -> HistoryItemId {
    let id = HistoryItemId::new();
    store
        .protocol_event_store()
        .seed_history_item_for_test(&HistoryItem {
            id,
            session_id,
            scope: HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: sequence as _,
            created_at_ms: sequence as i64 + 1,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "convert the documents and verify each result".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("seed canonical user authority");
    id
}

#[derive(Clone, Copy)]
enum ReviewMode {
    Allow,
    Deny,
    ChangeAuthority,
    LoseOwnership,
    FailRun,
}

struct Guardian {
    store: StoreBundle,
    key: PermissionRetryFenceKey,
    authority: HistoryItemId,
    control: RunControl,
    mode: ReviewMode,
    reviews: usize,
    lease: Option<PermissionReviewLease>,
    // Keep a worker ticket alive: publication must not rely on its eventual Drop.
    observers: Arc<Mutex<Vec<PermissionReviewLease>>>,
}

#[async_trait(?Send)]
impl PermissionGuardian for Guardian {
    async fn review(
        &mut self,
        request: &crate::tool::PermissionRequest,
        evidence: &PermissionGuardianEvidence,
    ) -> Result<PermissionGuardianAssessment, PermissionGuardianError> {
        assert!(!request.risks.is_empty());
        assert!(matches!(
            evidence,
            PermissionGuardianEvidence::DoclingConvert { .. }
        ));
        self.reviews += 1;
        let lease = match self
            .store
            .permission_retry_fence_store()
            .begin_review(
                self.key.clone(),
                self.authority,
                format!("{:064x}", self.reviews),
            )
            .expect("begin durable review")
        {
            BeginPermissionReview::Claimed(lease) => lease,
            other => return Err(PermissionGuardianError::RetryFenced(format!("{other:?}"))),
        };
        self.observers.lock().unwrap().push(lease.clone());
        if matches!(self.mode, ReviewMode::Deny) {
            assert_eq!(
                lease
                    .mark_denied(PermissionRetryFenceOutcome::GuardianDenied)
                    .unwrap(),
                PermissionReviewTransition::Applied
            );
            return Ok(PermissionGuardianAssessment {
                risk_level: PermissionRiskLevel::Critical,
                rationale: "the fixture denies this effect".to_string(),
            });
        }
        assert_eq!(
            lease.mark_allowed_pending().unwrap(),
            PermissionReviewTransition::Applied
        );
        match self.mode {
            ReviewMode::ChangeAuthority => {
                seed_user(&self.store, self.key.root_session_id(), 1);
            }
            ReviewMode::LoseOwnership => {
                assert_eq!(
                    lease.release().unwrap(),
                    PermissionReviewTransition::Applied
                );
            }
            ReviewMode::FailRun => {
                self.control.fail("fixture run failure before send");
            }
            ReviewMode::Allow | ReviewMode::Deny => {}
        }
        self.lease = Some(lease);
        Ok(PermissionGuardianAssessment {
            risk_level: PermissionRiskLevel::Low,
            rationale: "only the configured conversion is allowed".to_string(),
        })
    }

    fn take_retry_lease(&mut self) -> Option<PermissionReviewLease> {
        self.lease.take()
    }
}

struct Fixture {
    config: ResolvedConfig,
    session: SessionContext,
    services: ToolServices,
    control: RunControl,
    guardian: Guardian,
    prompt: HumanPrompt,
    _temp: tempfile::TempDir,
}

impl Fixture {
    async fn new(base_url: &str, mode: ReviewMode) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("document.md"), "# fixture document").unwrap();
        let data = Utf8PathBuf::from_path_buf(temp.path().join("data")).unwrap();
        let paths = StoragePaths {
            database_path: data.join("moyai.sqlite3"),
            truncation_dir: data.join("truncation"),
            data_dir: data,
        };
        let sqlite = SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &root, "fixture", "none")
            .await
            .unwrap();
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "Docling admission".to_string(),
                cwd: root,
                model: "fixture".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::AutoReview,
                provider_connection: None,
            })
            .await
            .unwrap();
        let authority = seed_user(&store, session.id, 0);
        let key = PermissionRetryFenceKey::new(session.id, 1, "a".repeat(64)).unwrap();
        let mut config = ResolvedConfig::default();
        config.docling.enabled = true;
        config.docling.base_url = base_url.to_string();
        config.docling.timeout_ms = 5_000;
        let workspace = WorkspaceDiscovery::discover_fixed_root(&session.cwd, &config).unwrap();
        let control = RunControl::new();
        let guardian = Guardian {
            store: store.clone(),
            key,
            authority,
            control: control.clone(),
            mode,
            reviews: 0,
            lease: None,
            observers: Default::default(),
        };
        let services = ToolServices {
            edit_safety: EditSafety::default(),
            formatter: Formatter::new(config.format.clone()),
            change_tracker: ChangeTracker,
            store,
            storage_paths: paths,
            truncator: ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
            skills: crate::skill::SkillsService::new(),
            managed_shells: Default::default(),
        };
        Self {
            config,
            session: SessionContext { session, workspace },
            services,
            control,
            guardian,
            prompt: HumanPrompt::default(),
            _temp: temp,
        }
    }

    async fn execute(&mut self, local: bool) -> Result<ToolResult, ToolError> {
        let arguments = if local {
            json!({"path":"document.md"})
        } else {
            json!({"source_url":"https://example.test/document.pdf"})
        };
        DoclingConvertTool
            .execute(
                arguments,
                ToolContext {
                    session: &self.session,
                    workspace: &self.session.workspace,
                    config: &self.config,
                    tool_call_id: ToolCallId::new(),
                    cancel: self.control.token(),
                    run_control: self.control.clone(),
                    run_mutation_fence: RunMutationFence::new(
                        self.services.store.session_repo(),
                        self.session.session.id,
                        AdmissionId::new(),
                        TurnId::new(),
                        self.control.clone(),
                    ),
                    prompt: &mut self.prompt,
                    services: &self.services,
                    agent: None,
                    permission_guardian: Some(&mut self.guardian),
                },
            )
            .await
    }

    fn record(&self) -> Option<crate::storage::PermissionRetryFenceRecord> {
        self.services
            .store
            .permission_retry_fence_store()
            .record(&self.guardian.key)
            .unwrap()
    }
}

struct Server {
    base_url: String,
    count: Arc<AtomicUsize>,
    routes: Arc<Mutex<Vec<String>>>,
    received: Arc<Notify>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn new(
        status: StatusCode,
        body: &'static str,
        hold: bool,
        on_response: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let routes: Arc<Mutex<Vec<String>>> = Default::default();
        let received = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (c, r, n, gate) = (
            count.clone(),
            routes.clone(),
            received.clone(),
            release.clone(),
        );
        let app = Router::new().fallback(any(move |uri: Uri| {
            let (c, r, n, gate, hook) = (
                c.clone(),
                r.clone(),
                n.clone(),
                gate.clone(),
                on_response.clone(),
            );
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                r.lock().unwrap().push(uri.path().to_string());
                n.notify_one();
                if hold {
                    gate.notified().await;
                }
                if let Some(hook) = hook {
                    hook();
                }
                (status, body)
            }
        }));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url,
            count,
            routes,
            received,
            release,
            task,
        }
    }

    fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const SUCCESS: &str = r#"{"document":{"md_content":"converted document"},"status":"success"}"#;

#[tokio::test]
async fn completed_conversions_release_before_result_and_allow_same_user_followups() {
    let server = Server::new(StatusCode::OK, SUCCESS, false, None).await;
    let mut fixture = Fixture::new(&server.base_url, ReviewMode::Allow).await;
    for index in 0..5 {
        let result = fixture
            .execute(index % 2 == 0)
            .await
            .expect("conversion succeeds");
        assert!(result.output_text.contains("converted document"));
        assert!(
            fixture.record().is_none(),
            "completed effect must be released before its result"
        );
        assert!(!fixture.control.is_cancelled());
    }
    assert_eq!(server.requests(), 5);
    assert_eq!(fixture.guardian.reviews, 5);
    assert_eq!(fixture.guardian.observers.lock().unwrap().len(), 5);
    assert_eq!(fixture.prompt.0, 0);
    assert_eq!(
        *server.routes.lock().unwrap(),
        [
            "/v1/convert/file",
            "/v1/convert/source",
            "/v1/convert/file",
            "/v1/convert/source",
            "/v1/convert/file"
        ]
    );
}

#[tokio::test]
async fn completed_http_and_response_failures_release_and_preserve_conversion_error() {
    for (status, body, reason) in [
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "conversion unavailable",
            "failed with HTTP 500",
        ),
        (
            StatusCode::OK,
            "invalid json",
            "failed to parse docling response body",
        ),
    ] {
        let server = Server::new(status, body, false, None).await;
        let mut fixture = Fixture::new(&server.base_url, ReviewMode::Allow).await;
        let error = fixture.execute(false).await.expect_err("conversion fails");
        assert!(error.to_string().contains(reason), "{error}");
        assert!(
            fixture.record().is_none(),
            "completed failed effect must also be released"
        );
        assert_eq!(server.requests(), 1);
        assert!(!fixture.control.is_cancelled());
        assert_eq!(fixture.prompt.0, 0);
    }
}

#[tokio::test]
async fn rejection_before_send_preserves_cause_and_never_calls_http() {
    for mode in [
        ReviewMode::Deny,
        ReviewMode::ChangeAuthority,
        ReviewMode::LoseOwnership,
        ReviewMode::FailRun,
    ] {
        let server = Server::new(StatusCode::OK, SUCCESS, false, None).await;
        let mut fixture = Fixture::new(&server.base_url, mode).await;
        let error = fixture
            .execute(false)
            .await
            .expect_err("pre-send rejection");
        match mode {
            ReviewMode::Deny => {
                assert!(matches!(&error, ToolError::PermissionDenied { .. }));
                let record = fixture.record().expect("denied fence is retained");
                assert_eq!(record.state, PermissionRetryFenceState::Denied);
                assert_eq!(
                    record.outcome,
                    Some(PermissionRetryFenceOutcome::GuardianDenied)
                );
            }
            ReviewMode::ChangeAuthority => {
                assert!(
                    matches!(&error, ToolError::PermissionDenied { .. }),
                    "{error}"
                );
                assert!(
                    error
                        .to_string()
                        .contains("canonical user authorization changed"),
                    "{error}"
                );
                assert!(fixture.record().is_none());
            }
            ReviewMode::LoseOwnership => {
                assert!(
                    error
                        .to_string()
                        .contains("lost its durable retry-fence ownership"),
                    "{error}"
                );
                assert!(fixture.control.is_cancelled());
            }
            ReviewMode::FailRun => assert!(matches!(error, ToolError::RunInterrupted)),
            ReviewMode::Allow => unreachable!(),
        }
        assert_eq!(server.requests(), 0);
        assert_eq!(fixture.prompt.0, 0);
    }
}

#[tokio::test]
async fn completion_ownership_loss_rejects_result_and_preserves_new_review() {
    let holder: Arc<Mutex<Option<(StoreBundle, PermissionRetryFenceKey, PermissionReviewLease)>>> =
        Default::default();
    let replacement: Arc<Mutex<Option<PermissionReviewLease>>> = Default::default();
    let (h, next) = (holder.clone(), replacement.clone());
    let hook = Arc::new(move || {
        let (store, key, lease) = h.lock().unwrap().take().unwrap();
        assert_eq!(
            lease.release().unwrap(),
            PermissionReviewTransition::Applied
        );
        let authority = seed_user(&store, key.root_session_id(), 1);
        let new_lease = match store
            .permission_retry_fence_store()
            .begin_review(key, authority, "c".repeat(64))
            .unwrap()
        {
            BeginPermissionReview::Claimed(lease) => lease,
            other => panic!("expected new review, got {other:?}"),
        };
        *next.lock().unwrap() = Some(new_lease);
    });
    let server = Server::new(StatusCode::OK, SUCCESS, true, Some(hook)).await;
    let mut fixture = Fixture::new(&server.base_url, ReviewMode::Allow).await;
    let observers = fixture.guardian.observers.clone();
    let store = fixture.services.store.clone();
    let key = fixture.guardian.key.clone();
    let mut execute = Box::pin(fixture.execute(false));
    tokio::select! {
        _ = server.received.notified() => {}
        result = &mut execute => panic!("returned before response: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("request was not received"),
    }
    *holder.lock().unwrap() = Some((store, key, observers.lock().unwrap()[0].clone()));
    server.release.notify_one();
    let error = execute
        .await
        .expect_err("lost cleanup ownership must reject success");
    assert!(error.to_string().contains("NotOwned"), "{error}");
    assert!(fixture.control.is_cancelled());
    let new_lease = replacement.lock().unwrap().take().unwrap();
    assert_eq!(
        fixture.record().unwrap().review_id,
        new_lease.claim().review_id
    );
    assert_eq!(
        new_lease.release().unwrap(),
        PermissionReviewTransition::Applied
    );
}

#[tokio::test]
async fn completion_storage_failure_rejects_success_and_retains_admitted_fence() {
    let server = Server::new(StatusCode::OK, SUCCESS, true, None).await;
    let mut fixture = Fixture::new(&server.base_url, ReviewMode::Allow).await;
    let path = fixture.services.storage_paths.database_path.clone();
    let mut execute = Box::pin(fixture.execute(false));
    tokio::select! {
        _ = server.received.notified() => {}
        result = &mut execute => panic!("returned before response: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("request was not received"),
    }
    let writer = rusqlite::Connection::open(path).unwrap();
    writer.execute_batch("CREATE TRIGGER fixture_block_release BEFORE DELETE ON permission_retry_fences BEGIN SELECT RAISE(ABORT, 'fixture release failure'); END;").unwrap();
    server.release.notify_one();
    let error = execute
        .await
        .expect_err("failed settlement must reject success");
    assert!(
        error.to_string().contains("fixture release failure"),
        "{error}"
    );
    assert!(fixture.control.is_cancelled());
    assert_eq!(
        fixture.record().unwrap().state,
        PermissionRetryFenceState::Admitted
    );
    writer
        .execute_batch("DROP TRIGGER fixture_block_release;")
        .unwrap();
    assert_eq!(
        fixture.guardian.observers.lock().unwrap()[0]
            .release()
            .unwrap(),
        PermissionReviewTransition::Applied
    );
}

#[tokio::test]
async fn unfinished_http_future_does_not_publish_or_finish_the_admission() {
    let server = Server::new(StatusCode::OK, SUCCESS, true, None).await;
    let mut fixture = Fixture::new(&server.base_url, ReviewMode::Allow).await;
    let mut execute = Box::pin(fixture.execute(false));
    tokio::select! {
        _ = server.received.notified() => {}
        result = &mut execute => panic!("returned before response: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("request was not received"),
    }
    drop(execute);
    assert_eq!(server.requests(), 1);
    assert_eq!(
        fixture.record().unwrap().state,
        PermissionRetryFenceState::Admitted
    );
    assert!(!fixture.control.is_cancelled());
    assert_eq!(
        fixture.guardian.observers.lock().unwrap()[0]
            .release()
            .unwrap(),
        PermissionReviewTransition::Applied
    );
}
