use super::*;
use crate::tool::shell::{ShellStartTool, ShellStatusTool, ShellStopTool};
use std::time::Duration;

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
async fn managed_timeout_stops_the_os_process_and_preserves_terminal_facts() {
    let fixture = shell_tool_fixture().await;
    let (control, fence) = admit_test_run(&fixture).await;
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30; exit 0"
    } else {
        "sleep 30; exit 0"
    };
    let start = managed_call(
        &fixture,
        &ShellStartTool,
        serde_json::json!({"command":command,"timeout_ms":1500}),
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
    let fixture = shell_tool_fixture().await;
    let (control, fence) = admit_test_run(&fixture).await;
    for lifetime in [0, fixture.config.shell.max_timeout_ms + 1] {
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
