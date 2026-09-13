#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ulid::Ulid;

struct Runner {
    child: Child,
    _temp: tempfile::TempDir,
    config: std::path::PathBuf,
    data: std::path::PathBuf,
    workspace: std::path::PathBuf,
    id: String,
}

impl Runner {
    fn new(endpoint: &str) -> Self {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../project_sandbox/shared-work-completion-20260913/runner-process-gate");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(base).unwrap();
        let config = temp.path().join("config.toml");
        let data = temp.path().join("data");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&config, format!("[model]\nbase_url = {endpoint:?}\nmodel = \"runner-fixture\"\nprovider_profile = \"openai_compatible\"\nmax_retries = 0\n[multi_agent]\nenabled = true\n")).unwrap();
        let log = std::fs::File::create(temp.path().join("runner.log")).unwrap();
        let child = Self::host_command(&config, &data)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut runner = Self {
            child,
            _temp: temp,
            config,
            data,
            workspace,
            id: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = runner.command(&["identity"]);
            if output.status.success() {
                runner.id = serde_json::from_slice::<Value>(&output.stdout).unwrap()["identity"]["runner_id"].as_str().unwrap().into();
                break;
            }
            assert!(
                runner.child.try_wait().unwrap().is_none(),
                "Runner exited: {}",
                std::fs::read_to_string(runner._temp.path().join("runner.log")).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "Runner did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        runner
    }

    fn base_command(config: &std::path::Path, data: &std::path::Path) -> Command {
        let executable = std::env::var_os("MOYAI_TEST_RUNNER_EXE")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_moyai-runner").into());
        let mut command = Command::new(executable);
        command
            .env("MOYAI_CONFIG_PATH", config)
            .env("MOYAI_DATA_DIR", data)
            .stdin(Stdio::null());
        command
    }
    fn host_command(config: &std::path::Path, data: &std::path::Path) -> Command {
        use std::os::windows::process::CommandExt;
        let executable=std::env::var_os("MOYAI_TEST_RUNNER_LIB_EXE").expect("Runner process gate requires current MOYAI_TEST_RUNNER_LIB_EXE; see docs/runner-local.md");
        let mut command = Command::new(executable);
        command
            .env("MOYAI_CONFIG_PATH", config)
            .env("MOYAI_DATA_DIR", data)
            .env(
                "MOYAI_TEST_RESOURCE_REGISTRY",
                data.with_file_name("resource-policy"),
            )
            .env_remove("MOYAI_TEST_SHARED_SETTINGS")
            .args([
                "--exact",
                "runner::shared::process_fixture::isolated_runner_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdin(Stdio::null())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        command
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        Self::base_command(&self.config, &self.data)
            .args(args)
            .output()
            .unwrap()
    }

    fn value(&self, args: &[&str]) -> Value {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn submit(&self, id: &str, prompt: &str) -> Value {
        self.value(&[
            "run",
            "--runner",
            &self.id,
            "--run",
            id,
            "--directory",
            self.workspace.to_str().unwrap(),
            "--single-agent",
            prompt,
        ])
    }

    async fn settled(&self, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let value = self.value(&["status", "--runner", &self.id, "--run", id]);
            if value["run"]["state"] == "settled" {
                return value;
            }
            assert!(Instant::now() < deadline, "not settled: {value}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn shutdown(&mut self) {
        self.value(&["shutdown", "--runner", &self.id]);
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Runner did not drain");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn reopen(&mut self) {
        assert!(self.child.try_wait().unwrap().is_some());
        let log = std::fs::OpenOptions::new()
            .append(true)
            .open(self._temp.path().join("runner.log"))
            .unwrap();
        self.child = Self::host_command(&self.config, &self.data)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = self.command(&["identity"]);
            if output.status.success() {
                self.id = serde_json::from_slice::<Value>(&output.stdout).unwrap()["identity"]["runner_id"].as_str().unwrap().into();
                return;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    async fn state(&self, id: &str, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let value = self.value(&["status", "--runner", &self.id, "--run", id]);
            if value["run"]["state"] == expected {
                return value;
            }
            assert!(value["run"]["error"].is_null(), "{value}");
            assert!(
                Instant::now() < deadline,
                "state did not become {expected}: {value}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

async fn provider(
    block: bool,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let count = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(tokio::sync::Notify::new());
    let calls = count.clone();
    let release = released.clone();
    let router = axum::Router::new()
        .route("/v1/models", axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"runner-fixture"}]})) }))
        .route("/v1/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let calls = calls.clone(); let release = release.clone();
            async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if block && attempt == 0 { release.notified().await; }
                let (delta, finish) = if attempt == 0 {
                    (json!({"role":"assistant","tool_calls":[{"index":0,"id":"clock","type":"function","function":{"name":"current_time","arguments":"{}"}}]}), "tool_calls")
                } else {
                    assert!(body["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool" && message["content"].as_str().is_some_and(|content| content.contains("unix_ms"))));
                    (json!({"role":"assistant","content":"Runner persisted result"}), "stop")
                };
                let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let done = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                ([ ("content-type", "text/event-stream") ], format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n"))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, count, released, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Required Runner process gate: set MOYAI_TEST_RUNNER_LIB_EXE to current libtest and run --ignored"]
async fn runner_outlives_submit_client_and_retransmission_never_reexecutes() {
    let (endpoint, count, release, server) = provider(true).await;
    let mut runner = Runner::new(&endpoint);
    let id = Ulid::new().to_string();
    let accepted = runner.submit(&id, "Use the current_time tool, then answer.");
    assert_eq!(accepted["run"]["run_id"], id);
    let deadline = Instant::now() + Duration::from_secs(15);
    while count.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Each invocation above/below is a different, already-exited client process.
    assert_eq!(
        runner.submit(&id, "Use the current_time tool, then answer.")["run"]["run_id"],
        id
    );
    assert!(
        !runner
            .command(&[
                "run",
                "--runner",
                &runner.id,
                "--run",
                &id,
                "--directory",
                runner.workspace.to_str().unwrap(),
                "different input"
            ])
            .status
            .success()
    );
    assert!(
        !runner
            .command(&[
                "run",
                "--runner",
                &runner.id,
                "--run",
                &Ulid::new().to_string(),
                "--directory",
                runner.workspace.to_str().unwrap(),
                "another run"
            ])
            .status
            .success()
    );
    assert!(
        !runner
            .command(&["status", "--runner", &Ulid::new().to_string(), "--run", &id])
            .status
            .success()
    );
    release.notify_waiters();
    let settled = runner.settled(&id).await;
    assert!(settled["run"]["error"].is_null(), "{settled}");
    assert!(settled["run"]["summary"].is_object(), "{settled}");
    assert_eq!(settled["run"]["result_text"], "Runner persisted result");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let replay = runner.submit(&id, "Use the current_time tool, then answer.");
    assert_eq!(replay["run"]["session_id"], settled["run"]["session_id"]);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let old_runner = runner.id.clone();
    runner.shutdown();
    runner.reopen();
    assert_ne!(runner.id, old_runner);
    assert!(
        !runner
            .command(&["status", "--runner", &old_runner, "--run", &id])
            .status
            .success()
    );
    assert_eq!(
        runner.value(&["list", "--runner", &runner.id])["runs"],
        json!([])
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    runner.shutdown();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Required Runner process gate: set MOYAI_TEST_RUNNER_LIB_EXE to current libtest and run --ignored"]
async fn runner_stop_and_shutdown_drain_a_real_blocked_run() {
    let (endpoint, count, _, server) = provider(true).await;
    let mut runner = Runner::new(&endpoint);
    let id = Ulid::new().to_string();
    runner.submit(&id, "Wait for the provider.");
    let deadline = Instant::now() + Duration::from_secs(15);
    while count.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    runner.value(&["stop", "--runner", &runner.id, "--run", &id]);
    let settled = runner.settled(&id).await;
    assert!(settled["run"]["summary"].is_object(), "{settled}");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    runner.shutdown();
    server.abort();
}

async fn tool_provider(tool: &str, arguments: Value) -> (String, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = tool.to_owned();
    let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move || {
        let calls = calls.clone(); let tool = tool.clone(); let arguments = arguments.clone();
        async move {
            let (delta, finish) = if calls.fetch_add(1, Ordering::SeqCst) % 2 == 0 {
                (json!({"role":"assistant","tool_calls":[{"index":0,"id":"effect","type":"function","function":{"name":tool,"arguments":arguments.to_string()}}]}), "tool_calls")
            } else { (json!({"role":"assistant","content":"Effect processed"}), "stop") };
            let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
            let done = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
            ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Required Runner process gate: set MOYAI_TEST_RUNNER_LIB_EXE to current libtest and run --ignored"]
async fn separate_local_client_answers_exact_approval_before_the_effect() {
    for decision in ["approve", "deny", "stop"] {
        let (endpoint, server) = tool_provider("shell", json!({
            "command":"[System.IO.File]::WriteAllText((Join-Path (Get-Location) 'reviewed.txt'), 'approved')",
            "sandbox_permissions":"require_escalated", "justification":"Write only the controlled integration fixture after explicit approval."
        })).await;
        let mut runner = Runner::new(&endpoint);
        let id = Ulid::new().to_string();
        runner.submit(&id, "Create the controlled fixture after approval.");
        let pending = runner.state(&id, "waiting_approval").await;
        assert!(!runner.workspace.join("reviewed.txt").exists());
        let approval = pending["run"]["approval"]["approval_id"].as_str().unwrap();
        assert!(
            !runner
                .command(&[
                    "approve",
                    "--runner",
                    &runner.id,
                    "--run",
                    &id,
                    "--approval",
                    &Ulid::new().to_string(),
                    "approve"
                ])
                .status
                .success()
        );
        runner.value(&[
            "approve",
            "--runner",
            &runner.id,
            "--run",
            &id,
            "--approval",
            approval,
            decision,
        ]);
        runner.settled(&id).await;
        assert_eq!(
            runner.workspace.join("reviewed.txt").exists(),
            decision == "approve"
        );
        assert!(
            !runner
                .command(&[
                    "approve",
                    "--runner",
                    &runner.id,
                    "--run",
                    &id,
                    "--approval",
                    approval,
                    "approve"
                ])
                .status
                .success()
        );
        runner.shutdown();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Required Runner process gate: set MOYAI_TEST_RUNNER_LIB_EXE to current libtest and run --ignored"]
async fn completed_root_keeps_capacity_until_its_managed_process_really_stops() {
    let (endpoint, server) = tool_provider(
        "shell_start",
        json!({"command":"Start-Sleep -Seconds 120", "timeout_ms":120000}),
    )
    .await;
    let mut runner = Runner::new(&endpoint);
    let mut config = std::fs::read_to_string(&runner.config).unwrap();
    config.push_str("\n[permissions]\naccess_mode = \"full_access\"\n");
    std::fs::write(&runner.config, config).unwrap();
    let id = Ulid::new().to_string();
    runner.submit(&id, "Start the managed fixture process.");
    let running = runner.state(&id, "processes_running").await;
    assert!(running["run"]["summary"].is_object());
    assert!(
        !runner
            .command(&[
                "run",
                "--runner",
                &runner.id,
                "--run",
                &Ulid::new().to_string(),
                "--directory",
                runner.workspace.to_str().unwrap(),
                "another run"
            ])
            .status
            .success()
    );
    runner.value(&["stop", "--runner", &runner.id, "--run", &id]);
    let old_receipt = runner.settled(&id).await;
    let session = old_receipt["run"]["session_id"].as_str().unwrap();
    let resumed_id = Ulid::new().to_string();
    runner.value(&[
        "run",
        "--runner",
        &runner.id,
        "--run",
        &resumed_id,
        "--directory",
        runner.workspace.to_str().unwrap(),
        "--session",
        session,
        "--single-agent",
        "Start another managed fixture in the same session.",
    ]);
    let resumed = runner.state(&resumed_id, "processes_running").await;
    assert_eq!(
        runner.value(&["status", "--runner", &runner.id, "--run", &id]),
        old_receipt,
        "a new process in the same session must not reopen the old receipt"
    );
    runner.value(&["stop", "--runner", &runner.id, "--run", &id]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        runner.value(&["status", "--runner", &runner.id, "--run", &resumed_id]),
        resumed,
        "stopping the old receipt must not change the replacement result or process"
    );
    runner.value(&["stop", "--runner", &runner.id, "--run", &resumed_id]);
    runner.settled(&resumed_id).await;
    assert_eq!(
        runner.value(&["status", "--runner", &runner.id, "--run", &id]),
        old_receipt
    );
    runner.shutdown();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Required Runner process gate: set MOYAI_TEST_RUNNER_LIB_EXE to current libtest and run --ignored"]
async fn legacy_configuration_preserves_common_local_admission_stop_and_status() {
    let (endpoint, count, _, server) = provider(false).await;
    let mut runner = Runner::new(&endpoint);
    let id = Ulid::new().to_string();
    runner.submit(&id, "Keep the existing work.");
    let deadline = Instant::now() + Duration::from_secs(15);
    while count.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let original = runner.settled(&id).await;
    let mut settings = moyai::device_network::DeviceSettings::default();
    settings.receiver.confirmed = true;
    moyai::device_network::DeviceSettingsStore::new(
        camino::Utf8PathBuf::from_path_buf(
            runner
                .config
                .parent()
                .unwrap()
                .join("device-network/device.json"),
        )
        .unwrap(),
    )
    .save(&settings)
    .unwrap();
    runner.value(&["status", "--runner", &runner.id, "--run", &id]);
    runner.value(&["stop", "--runner", &runner.id, "--run", &id]);
    runner.settled(&id).await;
    let next_id = Ulid::new().to_string();
    let accepted = runner.command(&[
        "run",
        "--runner",
        &runner.id,
        "--run",
        &next_id,
        "--directory",
        runner.workspace.to_str().unwrap(),
        "--session",
        original["run"]["session_id"].as_str().unwrap(),
        "--single-agent",
        "new private work",
    ]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let completed = runner.settled(&next_id).await;
    assert!(completed["run"]["error"].is_null(), "{completed}");
    assert_eq!(count.load(Ordering::SeqCst), 3);
    runner.shutdown();
    server.abort();
}
