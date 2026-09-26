use super::*;
use crate::tool::shell::{ShellStartTool, ShellStatusTool, ShellStopTool};
use std::time::Duration;

use crate::tool::permission_guardian::{
    PermissionGuardian, PermissionGuardianDecision, PermissionGuardianError,
    PermissionGuardianEvidence, permission_retry_effect_keys,
};

struct DurableManagedGuardian {
    store: StoreBundle,
    session_id: crate::session::SessionId,
    authority_id: crate::protocol::HistoryItemId,
    workspace: Utf8PathBuf,
    call: crate::llm::ModelToolCall,
    lease: Option<crate::storage::PermissionReviewLease>,
    key: Option<crate::storage::PermissionRetryFenceKey>,
    reviewed: usize,
    deny: bool,
}

#[async_trait::async_trait(?Send)]
impl PermissionGuardian for DurableManagedGuardian {
    async fn review(
        &mut self,
        request: &crate::tool::PermissionRequest,
        evidence: &PermissionGuardianEvidence,
    ) -> Result<PermissionGuardianDecision, PermissionGuardianError> {
        let keys = permission_retry_effect_keys(request, evidence, &self.call, &self.workspace)?;
        let key = crate::storage::PermissionRetryFenceKey::new(
            self.session_id,
            keys.family_version,
            keys.family_sha256,
        )
        .unwrap();
        self.key = Some(key.clone());
        let lease = match self
            .store
            .permission_retry_fence_store()
            .begin_review(key, self.authority_id, keys.identity_sha256)
            .unwrap()
        {
            crate::storage::BeginPermissionReview::Claimed(lease) => lease,
            crate::storage::BeginPermissionReview::Blocked(_) => {
                return Err(PermissionGuardianError::RetryFenced(
                    "an earlier elevated effect still owns the same authority".into(),
                ));
            }
            other => panic!("unexpected authority change: {other:?}"),
        };
        self.reviewed += 1;
        if self.deny {
            assert_eq!(
                lease
                    .mark_denied(crate::storage::PermissionRetryFenceOutcome::GuardianDenied)
                    .unwrap(),
                crate::storage::PermissionReviewTransition::Applied
            );
            Ok(PermissionGuardianDecision::Deny {
                rationale: "this independent operation is not authorized".into(),
            })
        } else {
            assert_eq!(
                lease.mark_allowed_pending().unwrap(),
                crate::storage::PermissionReviewTransition::Applied
            );
            self.lease = Some(lease);
            Ok(PermissionGuardianDecision::Allow {
                rationale: "this exact operation is authorized".into(),
            })
        }
    }

    fn take_retry_lease(&mut self) -> Option<crate::storage::PermissionReviewLease> {
        self.lease.take()
    }
}

async fn managed_guardian(fixture: &mut ShellToolFixture) -> DurableManagedGuardian {
    fixture.config.permissions.access_mode = AccessMode::AutoReview;
    fixture
        .services
        .store
        .session_repo()
        .compare_and_set_root_session_access_mode(
            fixture.session.session.id,
            AccessMode::FullAccess,
            AccessMode::AutoReview,
        )
        .await
        .unwrap()
        .unwrap();
    let authority_id = crate::protocol::HistoryItemId::new();
    fixture
        .services
        .store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: authority_id,
            session_id: fixture.session.session.id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: 0,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "Start the application and inspect it while it remains running.".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .unwrap();
    DurableManagedGuardian {
        store: fixture.services.store.clone(),
        session_id: fixture.session.session.id,
        authority_id,
        workspace: fixture.session.workspace.root.clone(),
        call: crate::llm::ModelToolCall {
            call_id: String::new(),
            tool_name: String::new(),
            arguments_json: String::new(),
        },
        lease: None,
        key: None,
        reviewed: 0,
        deny: false,
    }
}

async fn guardian_managed_call<T: Tool>(
    fixture: &ShellToolFixture,
    tool: &T,
    arguments: serde_json::Value,
    control: &RunControl,
    fence: &RunMutationFence,
    guardian: &mut DurableManagedGuardian,
) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
    let tool_call_id = ToolCallId::new();
    guardian.call = crate::llm::ModelToolCall {
        call_id: tool_call_id.to_string(),
        tool_name: tool.spec().name.to_string(),
        arguments_json: arguments.to_string(),
    };
    let mut prompt = AllowPrompt;
    tool.execute(
        arguments,
        ToolContext {
            session: &fixture.session,
            workspace: &fixture.session.workspace,
            config: &fixture.config,
            tool_call_id,
            cancel: control.token(),
            run_control: control.clone(),
            run_mutation_fence: fence.clone(),
            prompt: &mut prompt,
            services: &fixture.services,
            agent: None,
            permission_guardian: Some(guardian),
        },
    )
    .await
}

#[tokio::test]
async fn managed_start_releases_approval_for_independent_shell_while_process_is_running() {
    let mut fixture = shell_tool_fixture().await;
    let mut guardian = managed_guardian(&mut fixture).await;
    let (control, fence) = admit_test_run(&fixture).await;
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30"
    } else {
        "sleep 30"
    };
    let start = guardian_managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({
            "command":command,"timeout_ms":45000,
            "sandbox_permissions":"require_escalated","justification":"start the application"
        }),
        &control,
        &fence,
        &mut guardian,
    )
    .await
    .expect("reviewed process start");
    assert_eq!(start.metadata["state"], "running");
    let inspection = guardian_managed_call(
        &fixture,
        &crate::tool::shell::ShellTool,
        serde_json::json!({
            "command":successful_read_only_command(),
            "sandbox_permissions":"require_escalated","justification":"inspect the application"
        }),
        &control,
        &fence,
        &mut guardian,
    )
    .await;
    let status = execute_tool_in_test_run(
        &fixture,
        &ShellStatusTool,
        serde_json::json!({"process_id":start.metadata["process_id"]}),
        &control,
        &fence,
    )
    .await;
    // Reap the child even when the regression reproduces and the next assertion fails.
    fixture.services.managed_shells.shutdown().await;
    let inspection = inspection.expect("the next independent action must receive its own review");
    assert_eq!(inspection.metadata["success"], true);
    assert!(inspection.output_text.contains("baseline-preserved"));
    assert_eq!(status.unwrap().metadata["state"], "running");
    assert_eq!(guardian.reviewed, 2);
    assert!(
        fixture
            .services
            .store
            .permission_retry_fence_store()
            .record(guardian.key.as_ref().unwrap())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn managed_process_cleanup_preserves_a_later_denial_and_blocks_an_alternate_command() {
    let mut fixture = shell_tool_fixture().await;
    let mut guardian = managed_guardian(&mut fixture).await;
    let (control, fence) = admit_test_run(&fixture).await;
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30"
    } else {
        "sleep 30"
    };
    let start = guardian_managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({
            "command":command,"timeout_ms":45000,
            "sandbox_permissions":"require_escalated","justification":"start the application"
        }),
        &control,
        &fence,
        &mut guardian,
    )
    .await
    .expect("reviewed process start");
    assert_eq!(start.metadata["state"], "running");
    guardian.deny = true;
    let denied = guardian_managed_call(
        &fixture,
        &crate::tool::shell::ShellTool,
        serde_json::json!({
            "command":replace_baseline_command(),
            "sandbox_permissions":"require_escalated","justification":"unapproved file replacement"
        }),
        &control,
        &fence,
        &mut guardian,
    )
    .await;
    let denied_without_effect = matches!(
        denied,
        Err(crate::error::ToolError::PermissionDenied { .. })
    );
    drop(denied);
    let store = fixture.services.store.permission_retry_fence_store();
    let key = guardian.key.clone().unwrap();
    let before_cleanup = store.record(&key).unwrap();
    fixture.services.managed_shells.shutdown().await;

    assert!(denied_without_effect);
    assert_eq!(guardian.reviewed, 2);
    assert!(!fixture.session.workspace.root.join("baseline.txt").exists());
    let denied_claim = before_cleanup.expect("the later denial must be durable");
    assert_eq!(
        denied_claim.state,
        crate::storage::PermissionRetryFenceState::Denied
    );
    assert_eq!(store.record(&key).unwrap(), Some(denied_claim.clone()));
    assert!(matches!(
        store.begin_review(
            key,
            guardian.authority_id,
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        ).unwrap(),
        crate::storage::BeginPermissionReview::Blocked(record) if record == denied_claim
    ));
    assert!(
        !fixture
            .services
            .managed_shells
            .has_local_work(fixture.session.session.id)
    );
}

#[tokio::test]
async fn managed_start_settlement_failure_stops_the_run_and_cleans_up_its_process() {
    let mut fixture = shell_tool_fixture().await;
    let mut guardian = managed_guardian(&mut fixture).await;
    let (control, fence) = admit_test_run(&fixture).await;
    let database = rusqlite::Connection::open(&fixture.services.storage_paths.database_path)
        .expect("open this test's isolated database");
    database
        .execute_batch(
            "CREATE TRIGGER fixture_block_managed_fence_release
         BEFORE DELETE ON permission_retry_fences
         BEGIN SELECT RAISE(ABORT, 'fixture managed release failure'); END;",
        )
        .expect("inject settlement failure after process startup");
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30"
    } else {
        "sleep 30"
    };
    let result = guardian_managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({
            "command":command,"timeout_ms":45000,
            "sandbox_permissions":"require_escalated","justification":"start the application"
        }),
        &control,
        &fence,
        &mut guardian,
    )
    .await;
    // Even a successfully spawned child must be reclaimed when durable handoff fails.
    // Observe cancellation before shutdown, which would otherwise mask a lost handoff.
    let cancelled_before_shutdown = tokio::time::timeout(Duration::from_secs(10), async {
        while fixture
            .services
            .managed_shells
            .has_local_work(fixture.session.session.id)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    fixture.services.managed_shells.shutdown().await;
    assert!(cancelled_before_shutdown.is_ok());
    assert!(matches!(result, Err(crate::error::ToolError::Message(_))));
    assert!(control.is_cancelled());
    assert_eq!(guardian.reviewed, 1);
    assert!(
        !fixture
            .services
            .managed_shells
            .has_local_work(fixture.session.session.id)
    );
    assert_eq!(
        fixture
            .services
            .store
            .permission_retry_fence_store()
            .record(guardian.key.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .state,
        crate::storage::PermissionRetryFenceState::Admitted
    );
    database
        .execute_batch("DROP TRIGGER fixture_block_managed_fence_release;")
        .expect("remove the isolated failure injection");
}

async fn managed_call<T: Tool>(
    fixture: &ShellToolFixture,
    tool: &T,
    args: serde_json::Value,
    control: &RunControl,
    fence: &RunMutationFence,
) -> crate::tool::ToolResult {
    execute_tool_in_test_run(fixture, tool, args, control, fence)
        .await
        .expect("managed tool")
}

#[cfg(windows)]
fn delayed_exit_command() -> &'static str {
    "Start-Sleep -Milliseconds 1200; Write-Output measured; exit 7"
}
#[cfg(not(windows))]
fn delayed_exit_command() -> &'static str {
    "sleep 1.2; printf measured; exit 7"
}

#[tokio::test]
async fn managed_start_returns_live_identity_and_status_preserves_actual_failure() {
    let fixture = shell_tool_fixture().await;
    let (control, fence) = admit_test_run(&fixture).await;
    let start = managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":delayed_exit_command(),"timeout_ms":10000}),
        &control,
        &fence,
    )
    .await;
    let id = start.metadata["process_id"].clone();
    assert_eq!(start.metadata["state"], "running");
    assert!(start.metadata["pid"].as_u64().unwrap() > 0);
    assert!(start.metadata["success"].is_null());
    assert_eq!(start.metadata["readiness"], "not_checked");
    assert_eq!(start.metadata["output_available"], false);
    let completed = managed_call(
        &fixture,
        &ShellStatusTool,
        serde_json::json!({"process_id":id,"wait_ms":10000}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(completed.metadata["state"], "failed");
    assert_eq!(completed.metadata["success"], false);
    assert_eq!(completed.metadata["result"]["exit_code"], 7);
    assert_eq!(completed.metadata["result"]["cleanup_failed"], false);
    assert!(completed.metadata["result"].get("stdout").is_none());
    let body: serde_json::Value = serde_json::from_str(&completed.output_text).unwrap();
    assert_eq!(body["result"]["exit_code"], 7);
    assert!(
        body["result"]["stdout"]
            .as_str()
            .unwrap()
            .contains("measured")
    );
    assert!(completed.metadata["finished_at"].as_str().is_some());
    fixture.services.managed_shells.shutdown().await;
}

#[tokio::test]
async fn managed_lifetime_and_retained_receipt_inherit_model_timeout() {
    for (configured, requested) in [
        (3_600_000, None),
        (1_234_567, None),
        (1_234_567, Some(700_000)),
        (1_234_567, Some(1_234_567)),
    ] {
        let mut fixture = shell_tool_fixture().await;
        fixture.config.model.request_timeout_ms = configured;
        let scope = ulid::Ulid::new();
        fixture.services.managed_shells = fixture
            .services
            .managed_shells
            .with_lifetime(CancellationToken::new(), scope);
        let (control, fence) = admit_test_run(&fixture).await;
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let mut args = serde_json::json!({"command":command,"retain_after_turn":true});
        if let Some(requested) = requested {
            args["timeout_ms"] = serde_json::json!(requested);
        }
        let before = chrono::Utc::now().timestamp_millis() as u64;
        let start = managed_call(&fixture, &ShellStartTool, args, &control, &fence).await;
        let after = chrono::Utc::now().timestamp_millis() as u64;
        let service = fixture
            .services
            .managed_shells
            .completion_service_in_scope(fixture.session.session.id, scope)
            .expect("running server can be retained after completion");
        let expected = requested.unwrap_or(configured);
        assert_eq!(start.metadata["timeout_ms"], expected);
        assert_eq!(start.metadata["process_id"], service.service_id.to_string());
        assert!(service.retain_after_turn);
        assert!((before + expected..=after + expected).contains(&service.expires_at_ms));

        // Later configuration changes must not renew an already issued service deadline.
        fixture.config.model.request_timeout_ms = 10_000;
        let status = managed_call(
            &fixture,
            &ShellStatusTool,
            serde_json::json!({"process_id":start.metadata["process_id"]}),
            &control,
            &fence,
        )
        .await;
        assert_eq!(status.metadata["state"], "running");
        assert_eq!(status.metadata["timeout_ms"], expected);
        assert_eq!(
            fixture
                .services
                .managed_shells
                .completion_service_in_scope(fixture.session.session.id, scope)
                .unwrap()
                .expires_at_ms,
            service.expires_at_ms
        );
        let stopped = managed_call(
            &fixture,
            &ShellStopTool,
            serde_json::json!({"process_id":start.metadata["process_id"],"wait_ms":15000}),
            &control,
            &fence,
        )
        .await;
        assert_eq!(stopped.metadata["state"], "cancelled");
        assert_eq!(stopped.metadata["result"]["cleanup_failed"], false);
        fixture.services.managed_shells.shutdown().await;
    }
}

#[tokio::test]
async fn managed_timeout_stops_the_os_process_and_preserves_terminal_facts() {
    let mut fixture = shell_tool_fixture().await;
    fixture.config.model.request_timeout_ms = 1500;
    let (control, fence) = admit_test_run(&fixture).await;
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30; exit 0"
    } else {
        "sleep 30; exit 0"
    };
    let start = managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":command}),
        &control,
        &fence,
    )
    .await;
    assert!(start.metadata["pid"].as_u64().unwrap() > 0);
    let completed = managed_call(
        &fixture,
        &ShellStatusTool,
        serde_json::json!({"process_id":start.metadata["process_id"],"wait_ms":15000}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(completed.metadata["state"], "timed_out");
    assert_eq!(completed.metadata["success"], false);
    assert_eq!(completed.metadata["output_available"], true);
    assert_eq!(completed.metadata["readiness"], "not_checked");
    assert_eq!(completed.metadata["pid"], start.metadata["pid"]);
    assert!(completed.metadata["elapsed_ms"].as_u64().unwrap() >= 1500);
    for field in ["started_at", "finished_at"] {
        chrono::DateTime::parse_from_rfc3339(completed.metadata[field].as_str().unwrap())
            .expect("observed lifecycle timestamp");
    }
    let body: serde_json::Value = serde_json::from_str(&completed.output_text).unwrap();
    assert_eq!(body["result"]["timed_out"], true);
    assert_eq!(body["result"]["cancelled"], false);
    assert_eq!(body["result"]["effect_started"], true);
    assert_eq!(body["result"]["cleanup_failed"], false);
    fixture.services.managed_shells.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
async fn managed_server_serves_after_start_returns_then_stop_reaps_its_listener() {
    use std::io::{Read, Write};
    let fixture = shell_tool_fixture().await;
    let (control, fence) = admit_test_run(&fixture).await;
    // Bind to port zero inside the child and publish the actual address, avoiding port races.
    // Consume the request before closing: unread TCP data can turn close into a reset on Windows.
    let command = r#"$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
[IO.File]::WriteAllText((Join-Path (Get-Location) 'server-port.txt'), [string]$listener.LocalEndpoint.Port)
while ($true) {
    $client = $listener.AcceptTcpClient()
    $stream = $client.GetStream()
    $stream.ReadTimeout = 5000
    $request = ''
    while (-not $request.EndsWith("`r`n`r`n")) {
        $byte = $stream.ReadByte()
        if ($byte -lt 0) { throw 'request ended before its headers' }
        $request += [char]$byte
        if ($request.Length -gt 4096) { throw 'request headers exceeded fixture limit' }
    }
    $bytes = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 OK`r`nContent-Length: 2`r`nConnection: close`r`n`r`nok")
    $stream.Write($bytes, 0, $bytes.Length)
    $client.Dispose()
}"#;
    let start = managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":command,"timeout_ms":20000}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(start.metadata["state"], "running");
    let path = fixture.session.workspace.root.join("server-port.txt");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let port: u16 = loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(port) = text.parse()
        {
            break port;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server never became ready"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let response = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        // TCP may split the headers and body across reads.
        let mut response = String::new();
        stream.take(1024).read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.ends_with("ok"));
    let stopped = managed_call(
        &fixture,
        &ShellStopTool,
        serde_json::json!({"process_id":start.metadata["process_id"],"wait_ms":15000}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(stopped.metadata["state"], "cancelled");
    assert_eq!(stopped.metadata["success"], false);
    let body: serde_json::Value = serde_json::from_str(&stopped.output_text).unwrap();
    assert_eq!(body["result"]["cleanup_failed"], false);
    assert!(
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(100)
        )
        .is_err()
    );
    fixture.services.managed_shells.shutdown().await;
}

#[tokio::test]
async fn managed_start_rejects_cancelled_admission_and_invalid_lifetime_before_effect() {
    let mut fixture = shell_tool_fixture().await;
    fixture.config.model.request_timeout_ms = 9000;
    let (control, fence) = admit_test_run(&fixture).await;
    for lifetime in [0, fixture.config.model.request_timeout_ms + 1] {
        assert!(
            execute_tool_in_test_run(
                &fixture,
                &ShellStartTool,
                serde_json::json!({"command":successful_read_only_command(),"timeout_ms":lifetime}),
                &control,
                &fence
            )
            .await
            .is_err()
        );
    }
    assert!(
        !fixture
            .services
            .managed_shells
            .has_local_work(fixture.session.session.id)
    );
    control.token().cancel();
    let result = execute_tool_in_test_run(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":successful_read_only_command()}),
        &control,
        &fence,
    )
    .await;
    match result {
        Err(_) => {}
        Ok(value) => {
            assert_ne!(value.metadata["state"], "running");
            assert!(value.metadata["pid"].is_null());
            assert_ne!(value.metadata["success"], true);
        }
    }
    fixture.services.managed_shells.shutdown().await;
}

#[tokio::test]
async fn ordinary_shell_keeps_its_own_timeout_limit() {
    let mut fixture = shell_tool_fixture().await;
    fixture.config.shell.default_timeout_ms = 2000;
    fixture.config.shell.max_timeout_ms = 1500;
    let (control, fence) = admit_test_run(&fixture).await;
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30; exit 0"
    } else {
        "sleep 30; exit 0"
    };
    let result = managed_call(
        &fixture,
        &crate::tool::shell::ShellTool,
        serde_json::json!({"command":command}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(result.metadata["timeout"], true);
    assert_eq!(result.metadata["success"], false);
    assert_eq!(result.metadata["cancelled"], false);
    assert_eq!(result.metadata["cleanup_failed"], false);
}

#[tokio::test]
async fn managed_start_uses_same_permission_denial_as_shell_and_unknown_ids_fail_closed() {
    let mut fixture = shell_tool_fixture().await;
    fixture.config.permissions.access_mode = AccessMode::AutoReview;
    fixture
        .services
        .store
        .session_repo()
        .compare_and_set_root_session_access_mode(
            fixture.session.session.id,
            AccessMode::FullAccess,
            AccessMode::AutoReview,
        )
        .await
        .unwrap()
        .unwrap();
    let (control, fence) = admit_test_run(&fixture).await;
    // No Guardian is configured: an elevated call must fail closed, never spawn unrestricted.
    let result = execute_tool_in_test_run(&fixture, &ShellStartTool,
        serde_json::json!({"command":successful_read_only_command(),"sandbox_permissions":"require_escalated","justification":"test denied start"}), &control, &fence).await;
    assert!(result.is_err());
    for stop in [false, true] {
        let tool: &dyn Tool = if stop {
            &ShellStopTool
        } else {
            &ShellStatusTool
        };
        assert!(
            execute_tool_in_test_run(
                &fixture,
                tool,
                serde_json::json!({"process_id":ulid::Ulid::new()}),
                &control,
                &fence
            )
            .await
            .is_err()
        );
    }
    fixture.services.managed_shells.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
async fn managed_workspace_mode_keeps_the_restricted_backend_and_observed_startup() {
    let mut fixture = shell_tool_fixture().await;
    fixture.config.permissions.access_mode = AccessMode::Default;
    fixture
        .services
        .store
        .session_repo()
        .compare_and_set_root_session_access_mode(
            fixture.session.session.id,
            AccessMode::FullAccess,
            AccessMode::Default,
        )
        .await
        .unwrap()
        .unwrap();
    let (control, fence) = admit_test_run(&fixture).await;
    let start = managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":"Write-Output restricted-observed","timeout_ms":15000}),
        &control,
        &fence,
    )
    .await;
    assert!(matches!(
        start.metadata["state"].as_str(),
        Some("running" | "completed")
    ));
    assert!(start.metadata["pid"].as_u64().unwrap() > 0);
    assert!(
        start.metadata["sandbox"]
            .as_str()
            .unwrap()
            .starts_with("workspace_write(")
    );
    let end = managed_call(
        &fixture,
        &ShellStatusTool,
        serde_json::json!({"process_id":start.metadata["process_id"],"wait_ms":15000}),
        &control,
        &fence,
    )
    .await;
    assert_eq!(end.metadata["state"], "completed");
    let body: serde_json::Value = serde_json::from_str(&end.output_text).unwrap();
    assert_eq!(body["result"]["cleanup_failed"], false);
    assert!(
        body["result"]["stdout"]
            .as_str()
            .unwrap()
            .contains("restricted-observed")
    );
    fixture.services.managed_shells.shutdown().await;
}
