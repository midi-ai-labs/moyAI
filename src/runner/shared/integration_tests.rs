//! Opt-in integration using real Hub and Runner executables with isolated device identities.
#![cfg(windows)]

use super::*;
use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

fn command(executable: &Utf8Path) -> Command {
    let mut command = Command::new(executable);
    command.creation_flags(0x08000000).stdin(Stdio::null());
    command
}

fn bounded_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Fixture command deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn body(response: reqwest::Response) -> Value {
    let status = response.status();
    let path = response.url().path().to_owned();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "HTTP {status} at {path}: {text}");
    serde_json::from_str(&text).unwrap()
}

struct Hub {
    executable: Utf8PathBuf,
    directory: Utf8PathBuf,
    process: Child,
    url: String,
    browser: reqwest::Client,
    csrf: String,
    cookie: String,
    session: String,
}

impl Hub {
    async fn start(executable: Utf8PathBuf, directory: Utf8PathBuf) -> Self {
        std::fs::create_dir_all(&directory).unwrap();
        let log = std::fs::File::create(directory.join("process.log")).unwrap();
        let process = command(&executable)
            .args(["--data-dir", directory.as_str(), "--web-port", "0"])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let browser = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(40))
            .build()
            .unwrap();
        let mut hub = Self {
            executable,
            directory,
            process,
            url: String::new(),
            browser,
            csrf: String::new(),
            cookie: String::new(),
            session: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            let status = bounded_output(command(&hub.executable).args([
                "--status",
                "--data-dir",
                hub.directory.as_str(),
            ]))
            .unwrap();
            if status.status.success() {
                let value: Value = serde_json::from_slice(&status.stdout).unwrap();
                if let Some(url) = value["web"]["local_url"].as_str() {
                    hub.url = reqwest::Url::parse(url)
                        .unwrap()
                        .origin()
                        .ascii_serialization();
                    break;
                }
            }
            assert!(
                hub.process.try_wait().unwrap().is_none(),
                "Hub exited: {}",
                std::fs::read_to_string(hub.directory.join("process.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "Hub startup deadline");
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        let session = body(
            hub.browser
                .get(format!("{}/admin/session", hub.url))
                .send()
                .await
                .unwrap(),
        )
        .await;
        hub.csrf = session["csrf"].as_str().unwrap().into();
        // Obtain the launch capability through this fixture's verified local host,
        // exactly as the Hub launcher does. It never enters process logs.
        let descriptor: Value = serde_json::from_slice(
            &std::fs::read(hub.directory.join("host-control.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(descriptor["pid"].as_u64(), Some(hub.process.id() as u64));
        assert_eq!(
            descriptor["catalog_path"]
                .as_str()
                .map(std::path::PathBuf::from),
            Some(
                std::fs::canonicalize(&hub.directory)
                    .unwrap()
                    .join("catalog.json")
            ),
        );
        let access = body(
            hub.browser
                .post(format!(
                    "http://127.0.0.1:{}/host/command",
                    descriptor["port"].as_u64().unwrap()
                ))
                .bearer_auth(descriptor["token"].as_str().unwrap())
                .json(&json!({"command":"hub_web_open","args":{}}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(access["ok"], true);
        let access_url = reqwest::Url::parse(access["value"]["url"].as_str().unwrap()).unwrap();
        assert_eq!(access_url.origin().ascii_serialization(), hub.url);
        let ticket = access_url
            .fragment()
            .unwrap()
            .strip_prefix("access=")
            .unwrap();
        let response = hub
            .browser
            .post(format!("{}/admin/access", hub.url))
            .header("Origin", &hub.url)
            .header("X-Moyai-Csrf", &hub.csrf)
            .json(&json!({"ticket":ticket}))
            .send()
            .await
            .unwrap();
        hub.cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .into();
        let session = body(response).await;
        hub.session = session["authentication"]["session_id"]
            .as_str()
            .unwrap()
            .into();
        hub.csrf = session["csrf"].as_str().unwrap().into();
        hub
    }

    async fn admin(&self, name: &str, args: Value) -> Value {
        body(
            self.browser
                .post(format!("{}/admin/command", self.url))
                .header("Origin", &self.url)
                .header("X-Moyai-Csrf", &self.csrf)
                .header("Cookie", &self.cookie)
                .header("X-Moyai-Session", &self.session)
                .json(&json!({"command":name,"args":args}))
                .send()
                .await
                .unwrap(),
        )
        .await["result"]
            .clone()
    }

    async fn management(&self, request: Value) {
        let snapshot = self.admin("hub_shared_snapshot", json!({})).await;
        self.admin("hub_shared_command", json!({
            "request_id":ulid::Ulid::new().to_string(), "expected_revision":snapshot["revision"], "request":request,
        })).await;
    }

    async fn invitation(&self, label: &str) -> String {
        let snapshot = self.admin("hub_network_snapshot", json!({})).await;
        self.admin(
            "hub_network_invite",
            json!({"expectedRevision":snapshot["revision"],"label":label,"groups":[]}),
        )
        .await["code"]
            .as_str()
            .unwrap()
            .into()
    }

    async fn restart(&mut self) {
        let output = bounded_output(command(&self.executable).args([
            "--stop",
            "--data-dir",
            self.directory.as_str(),
        ]))
        .unwrap();
        assert!(
            output.status.success(),
            "Hub stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deadline = Instant::now() + Duration::from_secs(25);
        while self.process.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Hub shutdown deadline");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let log = std::fs::OpenOptions::new()
            .append(true)
            .open(self.directory.join("process.log"))
            .unwrap();
        self.process = command(&self.executable)
            .args([
                "--launch",
                "--no-browser",
                "--data-dir",
                self.directory.as_str(),
                "--web-port",
                "0",
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = bounded_output(command(&self.executable).args([
            "--stop",
            "--data-dir",
            self.directory.as_str(),
        ]));
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
    }
}

struct Worker {
    executable: Utf8PathBuf,
    config: Utf8PathBuf,
    data: Utf8PathBuf,
    settings: Utf8PathBuf,
    policy: Utf8PathBuf,
    process: Option<Child>,
    incarnation: String,
}

impl Worker {
    fn command(&self, args: &[&str]) -> std::process::Output {
        bounded_output(
            command(&self.executable)
                .env("MOYAI_CONFIG_PATH", &self.config)
                .env("MOYAI_DATA_DIR", &self.data)
                .args(args),
        )
        .unwrap()
    }

    async fn start(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.config.with_file_name("runner.log"))
            .unwrap();
        let mut process =
            command(&Utf8PathBuf::from_path_buf(std::env::current_exe().unwrap()).unwrap());
        process
            .env("MOYAI_CONFIG_PATH", &self.config)
            .env("MOYAI_DATA_DIR", &self.data)
            .env("MOYAI_TEST_RESOURCE_REGISTRY", &self.policy)
            .env_remove("MOYAI_TEST_SHARED_SETTINGS")
            .args([
                "--exact",
                "runner::shared::process_fixture::isolated_runner_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        if self.settings.is_file() {
            process.env("MOYAI_TEST_SHARED_SETTINGS", &self.settings);
        }
        self.process = Some(process.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let output = self.command(&["identity"]);
            if output.status.success() {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                self.incarnation = value["identity"]["runner_id"].as_str().unwrap().into();
                return;
            }
            assert!(
                self.process.as_mut().unwrap().try_wait().unwrap().is_none(),
                "Runner exited: {}",
                std::fs::read_to_string(self.config.with_file_name("runner.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "Runner startup deadline");
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    async fn stop(&mut self) {
        let output = self.command(&["shutdown", "--runner", &self.incarnation]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deadline = Instant::now() + Duration::from_secs(25);
        while self.process.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Runner shutdown deadline");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.process = None;
    }
    async fn settled_run(&self, id: &str) -> Value {
        self.run_state(id, "settled").await
    }
    async fn run_state(&self, id: &str, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let output = self.command(&["status", "--runner", &self.incarnation, "--run", id]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let run: Value = serde_json::from_slice(&output.stdout).unwrap();
            if run["run"]["state"] == expected {
                return run;
            }
            assert!(
                Instant::now() < deadline,
                "Local participant did not settle: {run}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            if !self.incarnation.is_empty() {
                let _ = self.command(&["shutdown", "--runner", &self.incarnation]);
            }
            if process.try_wait().ok().flatten().is_none() {
                let _ = process.kill();
            }
            let _ = process.wait();
        }
    }
}

struct Provider {
    endpoint: String,
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    release_child: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Provider {
    async fn start() -> Self {
        let parent_calls = Arc::new(AtomicUsize::new(0));
        let child_calls = Arc::new(AtomicUsize::new(0));
        let release_child = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (parents, children, released, captured) = (
            parent_calls.clone(),
            child_calls.clone(),
            release_child.clone(),
            requests.clone(),
        );
        let app = axum::Router::new()
            .route("/v1/models",axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"fixture-parent"},{"id":"fixture-child"}]})) }))
            .route("/v1/chat/completions",axum::routing::post(move |axum::Json(request):axum::Json<Value>| {
                let (parents,children,released,captured)=(parents.clone(),children.clone(),released.clone(),captured.clone());
                async move {
                    captured.lock().unwrap().push(request.clone());
                    let task_text = request["messages"].to_string();
                    let approval_case = task_text.contains("approval-fixture") || task_text.contains("cancel-fixture");
                    let (delta,finish) = if task_text.contains("guardian-handoff-fixture") {
                        let is_guardian = request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "system" && message["content"].as_str().is_some_and(|content| content.contains("independent permission guardian")));
                        if is_guardian {
                            assert!(request["tools"].is_null() || request["tools"].as_array().is_some_and(Vec::is_empty));
                            assert!(request["tool_choice"].is_null());
                            (json!({"role":"assistant","content":json!({"decision":"ask_user","rationale":"WinB側の作業フォルダに確認用ファイルを作成してよいか確認してください。"}).to_string()}),"stop")
                        } else if request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool" && message["tool_call_id"] == "guardian-handoff-effect") {
                            (json!({"role":"assistant","content":"Human-approved shared effect completed once"}),"stop")
                        } else {
                            let arguments = json!({"command":"[System.IO.File]::AppendAllText((Join-Path (Get-Location) 'guardian-handoff.txt'), 'approved')","sandbox_permissions":"require_escalated","justification":"Create the controlled shared test file after human confirmation."});
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"guardian-handoff-effect","type":"function","function":{"name":"shell","arguments":arguments.to_string()}}]}),"tool_calls")
                        }
                    } else if task_text.contains("gateway-interruption-fixture") {
                        released.notified().await;
                        (json!({"role":"assistant","content":"Interrupted response must never become a successful job"}),"stop")
                    } else if task_text.contains("gateway-reconnection-fixture") {
                        (json!({"role":"assistant","content":"Gateway reconnection verified"}),"stop")
                    } else if task_text.contains("shared-active-stop-fixture") {
                        if request["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool"&&message["tool_call_id"]=="active-stop-shell") {
                            released.notified().await;
                            (json!({"role":"assistant","content":"Stopped work must not complete"}),"stop")
                        } else {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"active-stop-shell","type":"function","function":{"name":"shell_start","arguments":json!({"command":"Start-Sleep -Seconds 120","timeout_ms":120000,"retain_after_turn":true}).to_string()}}]}),"tool_calls")
                        }
                    } else if task_text.contains("shared-retained-stop-fixture") {
                        if request["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool"&&message["tool_call_id"]=="retained-stop-shell") {
                            (json!({"role":"assistant","content":"Preview is available for this conversation"}),"stop")
                        } else {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"retained-stop-shell","type":"function","function":{"name":"shell_start","arguments":json!({"command":"Start-Sleep -Seconds 120","timeout_ms":120000,"retain_after_turn":true}).to_string()}}]}),"tool_calls")
                        }
                    } else if task_text.contains("local-managed-fixture") {
                        if request["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool"&&message["tool_call_id"]=="local-managed") {
                            (json!({"role":"assistant","content":"Local managed root completed"}),"stop")
                        } else {
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"local-managed","type":"function","function":{"name":"shell_start","arguments":json!({"command":"Start-Sleep -Seconds 120","timeout_ms":120000}).to_string()}}]}),"tool_calls")
                        }
                    } else if task_text.contains("local-resource-fixture") {
                        if request["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool"&&message["tool_call_id"]=="local-clock") {
                            (json!({"role":"assistant","content":"Local participant completed"}),"stop")
                        } else {
                            released.notified().await;
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"local-clock","type":"function","function":{"name":"current_time","arguments":"{}"}}]}),"tool_calls")
                        }
                    } else if task_text.contains("paused-parent-fixture") {
                        (json!({"role":"assistant","tool_calls":[{"index":0,"id":"paused-parent-call","type":"function","function":{"name":"shared_delegate","arguments":json!({"environment_id":"solver","title":"Wait for approval","prompt":"cancel-fixture: request the controlled write"}).to_string()}}]}),"tool_calls")
                    } else if approval_case {
                        let completed = request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool" && message["tool_call_id"] == "approval-effect");
                        if completed {
                            (json!({"role":"assistant","content":"Approval effect completed"}),"stop")
                        } else {
                            let target = if task_text.contains("cancel-fixture") { "cancelled.txt" } else { "reviewed.txt" };
                            let arguments=json!({"command":format!("[System.IO.File]::WriteAllText((Join-Path (Get-Location) '{target}'), 'approved')"),"sandbox_permissions":"require_escalated","justification":"Write only the controlled shared fixture after explicit approval."});
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"approval-effect","type":"function","function":{"name":"shell","arguments":arguments.to_string()}}]}),"tool_calls")
                        }
                    } else if request["model"] == "fixture-parent" {
                        let step = parents.fetch_add(1,Ordering::SeqCst);
                        if step == 0 {
                            assert!(request["tools"].as_array().unwrap().iter().any(|tool|tool["function"]["name"]=="shared_delegate"));
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"parent-child-call","type":"function","function":{"name":"shared_delegate","arguments":json!({"environment_id":"solver","title":"Solve","prompt":"Use current_time and return solver result"}).to_string()}}]}),"tool_calls")
                        } else {
                            assert_eq!(step,1,"Parent model must resume only once");
                            let outputs=request["messages"].as_array().unwrap().iter().filter(|message|message["role"]=="tool"&&message["tool_call_id"]=="parent-child-call").collect::<Vec<_>>();
                            assert_eq!(outputs.len(),1,"Child result must enter the original conversation once");
                            assert!(outputs[0]["content"].as_str().unwrap().contains("solver result"));
                            (json!({"role":"assistant","content":"Parent consumed solver result exactly once"}),"stop")
                        }
                    } else {
                        assert_eq!(request["model"],"fixture-child");
                        let step=children.fetch_add(1,Ordering::SeqCst);
                        if step==0 {
                            released.notified().await;
                            (json!({"role":"assistant","tool_calls":[{"index":0,"id":"child-clock","type":"function","function":{"name":"current_time","arguments":"{}"}}]}),"tool_calls")
                        } else {
                            assert_eq!(step,1,"Child effects must not be repeated");
                            assert!(request["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool"&&message["content"].as_str().is_some_and(|text|text.contains("unix_ms"))));
                            (json!({"role":"assistant","content":"solver result"}),"stop")
                        }
                    };
                    let chunk=json!({"id":"shared-fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                    let done=json!({"id":"shared-fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                    ([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n"))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            parent_calls,
            child_calls,
            requests,
            release_child,
            task,
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn shared_post(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    path: &str,
    payload: Value,
) -> Value {
    body(
        http.post(format!("{url}/v1/shared/{path}"))
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
            .unwrap(),
    )
    .await
}

async fn job(http: &reqwest::Client, url: &str, token: &str, id: &str) -> Value {
    body(
        http.get(format!("{url}/v1/shared/jobs/{id}"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap(),
    )
    .await
}

async fn wait_job(http: &reqwest::Client, url: &str, token: &str, id: &str, state: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let value = job(http, url, token, id).await;
        if value["state"] == state {
            return value;
        }
        assert!(
            !matches!(value["state"].as_str(), Some("failed" | "cancelled")),
            "Unexpected terminal: {value}"
        );
        assert!(
            Instant::now() < deadline,
            "Job did not become {state}: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_approval(http: &reqwest::Client, url: &str, token: &str, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        let value = body(
            http.get(format!("{url}/v1/shared/jobs/{id}/approval"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap(),
        )
        .await;
        if value["can_decide"] == true {
            return value;
        }
        let current = job(http, url, token, id).await;
        assert!(
            !matches!(
                current["state"].as_str(),
                Some("failed" | "succeeded" | "cancelled")
            ),
            "No approval: {current}"
        );
        assert!(
            Instant::now() < deadline,
            "Approval did not appear: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn real_hub_scenario() {
    let hub_exe =
        Utf8PathBuf::from(std::env::var("MOYAI_TEST_HUB_EXE").expect("MOYAI_TEST_HUB_EXE"));
    let runner_exe =
        Utf8PathBuf::from(std::env::var("MOYAI_TEST_RUNNER_EXE").expect("MOYAI_TEST_RUNNER_EXE"));
    assert!(hub_exe.is_file() && runner_exe.is_file());
    let root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/shared-work-completion-20260913")
        .join(format!("real-hub-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).unwrap();
    eprintln!("real shared fixture: {root}");
    let mut hub = Hub::start(hub_exe, root.join("hub")).await;
    let metadata_port = free_port();
    let mut gateway_port = free_port();
    while gateway_port == metadata_port {
        gateway_port = free_port();
    }
    let snapshot = hub.admin("hub_network_snapshot", json!({})).await;
    hub.admin(
        "hub_network_settings",
        json!({"expectedRevision":snapshot["revision"],"gatewayPort":gateway_port}),
    )
    .await;
    hub.admin(
        "hub_network_start",
        json!({"bind":format!("127.0.0.1:{metadata_port}"),"advertiseIp":"127.0.0.1"}),
    )
    .await;
    let network = hub.admin("hub_network_snapshot", json!({})).await;
    let hub_id = network["hub_id"].as_str().unwrap().to_owned();
    let url = format!("https://127.0.0.1:{metadata_port}");
    let trust = crate::device_network::SharedHubConfig {
        hub_url: url.clone(),
        ca_certificate_pem: network["ca_certificate_pem"].as_str().unwrap().into(),
    };
    let provider = Provider::start().await;
    // Register through the same admin boundary as the browser. The actual network start
    // above owns the HTTPS Gateway; workers never receive this provider endpoint.
    for model in ["fixture-parent", "fixture-child"] {
        let evidence = hub
            .admin(
                "hub_discover",
                json!({
                    "endpoint": provider.endpoint, "providerProfile": "openai_compatible_chat",
                }),
            )
            .await;
        let snapshot = hub.admin("hub_snapshot", json!({})).await;
        hub.admin(
            "hub_register",
            json!({"request": {
                "revision": snapshot["store"]["revision"], "evidence_id": evidence["id"],
                "endpoint": provider.endpoint, "provider_profile": "openai_compatible_chat",
                "model_id": model, "label": model, "logical_model_id": null,
                "capacity_pool_id": null, "capacity": 2, "allow_tools": true,
            }}),
        )
        .await;
    }
    let gateway = hub.admin("hub_snapshot", json!({})).await;
    assert_eq!(
        gateway["gateway"]["ready"], true,
        "real HTTPS model Gateway must be ready"
    );
    let mut workers = Vec::new();
    let mut identities = Vec::new();
    for (name, environment, model, children) in [
        ("parent", "analysis", "fixture-parent", vec!["solver"]),
        ("child", "solver", "fixture-child", vec![]),
    ] {
        let directory = root.join(name);
        let workspace = directory.join("workspace");
        let data = directory.join("data");
        let config = directory.join("config/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        // A retained manual profile deliberately cannot serve these jobs. Successful model
        // calls must use the independently saved Main choice through Hub and its Gateway.
        let text = "[model]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"must-not-use-direct\"\nprovider_profile = \"openai_compatible\"\nmax_retries = 0\n[multi_agent]\nenabled = false\n";
        std::fs::write(&config, text).unwrap();
        let invitation = hub.invitation(name).await;
        let identity = integration_fixture::enroll(
            &config,
            &data,
            &workspace,
            trust.clone(),
            &hub_id,
            &invitation,
        )
        .await
        .unwrap();
        let models = crate::hub::HubConnection::new(crate::hub::HubSettingsStore::new(
            config.with_file_name("hub-settings.json"),
        ));
        models
            .connect_device(
                &url,
                crate::device_network::ManagedHubHttp::new(identity.http.clone()),
                true,
                None,
            )
            .await
            .unwrap();
        let current = models.projection_now();
        let catalog = current.catalog.as_ref().unwrap();
        let selected = catalog
            .models
            .iter()
            .find(|candidate| candidate.label == model)
            .unwrap();
        let selection = crate::hub::HubSelection {
            allowed_model_ids: [selected.id.clone()].into_iter().collect(),
            preferred_model_id: selected.id.clone(),
            required_capabilities: ["tools".into()].into_iter().collect(),
            wait_policy: crate::hub::HubWaitPolicy::WaitForPreferred,
            affinity_turns: 1,
        };
        let saved = models
            .save_review(
                crate::hub::HubReviewContext::Main,
                selection,
                catalog.hub_id.clone(),
                catalog.revision,
                current.settings_revision,
                current.connection_generation,
            )
            .await
            .unwrap();
        assert!(!saved.main_uses_default);
        assert_eq!(
            saved.main_review.unwrap().selection.preferred_model_id,
            selected.id
        );
        models.shutdown().await;
        let settings = config.with_file_name("runner-shared.json");
        std::fs::write(&settings,serde_json::to_vec_pretty(&json!({"version":1,"hub_id":hub_id,"device_id":identity.device_id,"environments":[{"environment_id":environment,"directory":workspace,"access_mode":"default","allowed_child_environments":children}]})).unwrap()).unwrap();
        workers.push(Worker {
            executable: runner_exe.clone(),
            config,
            policy: data.with_file_name("resource-policy"),
            data,
            settings,
            process: None,
            incarnation: String::new(),
        });
        identities.push(identity);
    }
    // Legacy invitation enrollment has no inferred human owner. Associate each
    // device explicitly through the administrator before obtaining device sessions.
    for (index, label) in [(0, "Alice"), (1, "Bob")] {
        hub.management(json!({"kind":"bind_device_principal","device_id":identities[index].device_id,"user_id":null,"display_name":label})).await;
    }
    let http = &identities[0].http;
    let login = body(
        http.post(format!("{url}/v1/shared/device-session"))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let alice = login["principal"].clone();
    let token = login["token"].as_str().unwrap();
    let bob_login = body(
        identities[1]
            .http
            .post(format!("{url}/v1/shared/device-session"))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let bob = bob_login["principal"].clone();
    hub.management(json!({"kind":"create_project","id":"project","label":"Shared fixture"}))
        .await;
    for principal in [&alice, &bob] {
        hub.management(json!({"kind":"project_member","project_id":"project","user_id":principal["user_id"],"role":"contributor"})).await;
    }
    for (index, id) in [(0, "analysis"), (1, "solver")] {
        hub.management(json!({"kind":"create_environment","id":id,"label":id,"resource_id":format!("resource-{id}"),"runner_id":identities[index].device_id,"capacity":1,"project_ids":["project"]})).await;
    }
    workers[0].start().await;
    workers[1].start().await;
    let input = json!({"request_id":"root-request","project_id":"project","environment_id":"analysis","title":"Compute with solver","input":{"version":1,"prompt":"Delegate to solver, then use its result."},"descendant_budget":2});
    let accepted = shared_post(http, &url, token, "jobs", input.clone()).await;
    let root_id = accepted["id"].as_str().unwrap();
    assert_eq!(
        shared_post(http, &url, token, "jobs", input).await["id"],
        root_id
    );
    let waiting = wait_job(http, &url, token, root_id, "waiting_child").await;
    let child_id = waiting["awaiting_child_id"].as_str().unwrap().to_owned();
    wait_job(http, &url, token, &child_id, "running").await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while provider.child_calls.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let occupancy = body(
        http.get(format!("{url}/v1/shared/environments?project_id=project"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        occupancy
            .as_array()
            .unwrap()
            .iter()
            .find(|environment| environment["id"] == "analysis")
            .unwrap()["occupied"],
        0
    );
    assert_eq!(
        occupancy
            .as_array()
            .unwrap()
            .iter()
            .find(|environment| environment["id"] == "solver")
            .unwrap()["occupied"],
        1
    );
    assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 1);
    // A separate config and workspace on the same provided device must reserve the same Hub
    // slot before model execution. The fixture injects only its machine-policy directory.
    let caller_root = root.join("local-caller");
    std::fs::create_dir_all(&caller_root).unwrap();
    let caller_workspace = caller_root.join("unrelated-workspace");
    std::fs::create_dir(&caller_workspace).unwrap();
    let caller_config = caller_root.join("config.toml");
    std::fs::write(&caller_config,format!("[model]\nbase_url = {:?}\nmodel = \"fixture-local\"\nprovider_profile = \"openai_compatible\"\nmax_retries = 0\n[multi_agent]\nenabled = false\n",provider.endpoint)).unwrap();
    let mut caller = Worker {
        executable: runner_exe.clone(),
        config: caller_config,
        data: caller_root.join("data"),
        settings: caller_root.join("no-shared-settings.json"),
        policy: workers[1].policy.clone(),
        process: None,
        incarnation: String::new(),
    };
    caller.start().await;
    let signin = json!({"operation":"local_project","project_id":"project"}).to_string();
    let output = workers[1].command(&["operations", "--runner", &workers[1].incarnation, &signin]);
    assert!(
        output.status.success(),
        "Local device project selection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let denied_id = ulid::Ulid::new().to_string();
    let requests_before = provider.requests.lock().unwrap().len();
    let output = caller.command(&[
        "run",
        "--runner",
        &caller.incarnation,
        "--run",
        &denied_id,
        "--directory",
        caller_workspace.as_str(),
        "--single-agent",
        "local-resource-fixture",
    ]);
    assert!(output.status.success());
    let denied = caller.settled_run(&denied_id).await;
    assert!(
        denied["run"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("Hub rejected")),
        "{denied}"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        requests_before,
        "Busy local request must not reach the model"
    );
    let checkpoint = waiting["checkpoint"].clone();
    // Hub now owns the model Gateway as well as job metadata. A live model HTTP request
    // cannot survive its shutdown. Preserve the successful resume contract at the durable
    // boundary: the parent is stopped with its checkpoint and the child's result is saved.
    let old_incarnation = workers[0].incarnation.clone();
    workers[0].stop().await;
    provider.release_child.notify_one();
    let child_completed = wait_job(http, &url, token, &child_id, "succeeded").await;
    assert_eq!(child_completed["result"]["text"], "solver result");
    assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.child_calls.load(Ordering::SeqCst), 2);
    assert_eq!(job(http, &url, token, root_id).await["state"], "queued");
    hub.restart().await;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let response = http
            .get(format!("{url}/v1/shared/jobs/{root_id}"))
            .bearer_auth(token)
            .send()
            .await;
        if let Ok(response) = response {
            if response.status().is_success() {
                let restored = body(response).await;
                assert_eq!(restored["state"], "queued");
                assert_eq!(restored["checkpoint"], checkpoint);
                assert_eq!(restored["awaiting_child_id"], child_id);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "Saved Hub TLS endpoint did not resume"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.child_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        job(http, &url, token, root_id).await["checkpoint"],
        checkpoint
    );
    assert_eq!(
        job(http, &url, token, &child_id).await["result"],
        child_completed["result"]
    );
    workers[0].start().await;
    assert_ne!(workers[0].incarnation, old_incarnation);
    assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 1);
    let completed = wait_job(http, &url, token, root_id, "succeeded").await;
    assert_eq!(
        completed["result"]["text"],
        "Parent consumed solver result exactly once"
    );
    assert_eq!(
        completed["result"]["summary"]["session_id"],
        checkpoint["session_id"]
    );
    assert_eq!(
        completed["result"]["summary"]["turn_id"],
        checkpoint["turn_id"]
    );
    assert_eq!(provider.parent_calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.child_calls.load(Ordering::SeqCst), 2);
    let local_id = ulid::Ulid::new().to_string();
    assert!(
        caller
            .command(&[
                "run",
                "--runner",
                &caller.incarnation,
                "--run",
                &local_id,
                "--directory",
                caller_workspace.as_str(),
                "--single-agent",
                "local-resource-fixture"
            ])
            .status
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["model"] == "fixture-local")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Local model did not enter after resource release"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let queued=shared_post(http,&url,token,"jobs",json!({"request_id":"wait-for-local","project_id":"project","environment_id":"solver","title":"Wait for local resource","input":{"version":1,"prompt":"Should remain queued"},"descendant_budget":0})).await;
    let queued_id = queued["id"].as_str().unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(job(http, &url, token, queued_id).await["state"], "queued");
    shared_post(
        http,
        &url,
        token,
        &format!("jobs/{queued_id}/cancel"),
        json!({}),
    )
    .await;
    provider.release_child.notify_one();
    let local = caller.settled_run(&local_id).await;
    assert!(local["run"]["error"].is_null(), "{local}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status: Value = serde_json::from_slice(
            &workers[1]
                .command(&["shared-status", "--runner", &workers[1].incarnation])
                .stdout,
        )
        .unwrap();
        if status["status"]["attempts"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Previous local resource outcome was not acknowledged: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A terminal model turn can still hold managed processes. Hub Cancel must reach that
    // exact resource lifetime and wait for its process drain before releasing capacity.
    let mut config = std::fs::read_to_string(&caller.config).unwrap();
    config.push_str("\n[permissions]\naccess_mode = \"full_access\"\n");
    std::fs::write(&caller.config, config).unwrap();
    let managed_id = ulid::Ulid::new().to_string();
    assert!(
        caller
            .command(&[
                "run",
                "--runner",
                &caller.incarnation,
                "--run",
                &managed_id,
                "--directory",
                caller_workspace.as_str(),
                "--single-agent",
                "local-managed-fixture"
            ])
            .status
            .success()
    );
    let managed = caller.run_state(&managed_id, "processes_running").await;
    assert!(managed["run"]["summary"].is_object());
    let status: Value = serde_json::from_slice(
        &workers[1]
            .command(&["shared-status", "--runner", &workers[1].incarnation])
            .stdout,
    )
    .unwrap();
    let active = status["status"]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|attempt| attempt["state"] == "executing")
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1, "{status}");
    let local_job = active[0]["job_id"].as_str().unwrap().to_owned();
    assert_eq!(
        job(http, &url, token, &local_job).await["assignee_id"],
        bob["user_id"]
    );
    // Runner's local admissions renew bounded device sessions. Obtain this
    // controller's current session instead of reusing the initial setup token.
    let bob_login = body(
        identities[1]
            .http
            .post(format!("{url}/v1/shared/device-session"))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(bob_login["principal"]["user_id"], bob["user_id"]);
    let bob_token = bob_login["token"].as_str().unwrap();
    // Local work belongs to this provided device's actor, so its owner cancels it.
    shared_post(
        &identities[1].http,
        &url,
        bob_token,
        &format!("jobs/{local_job}/cancel"),
        json!({}),
    )
    .await;
    caller.settled_run(&managed_id).await;
    wait_job(http, &url, token, &local_job, "cancelled").await;
    caller.stop().await;
    let child_workspace = root.join("child/workspace");
    for (request_id, prompt, decision) in [
        (
            "approval-request",
            "approval-fixture: request the controlled write",
            "approve",
        ),
        (
            "cancel-request",
            "cancel-fixture: request the controlled write",
            "cancel",
        ),
    ] {
        let accepted = shared_post(http,&url,token,"jobs",json!({"request_id":request_id,"project_id":"project","environment_id":"solver","title":"Controlled permission","input":{"version":1,"prompt":prompt},"descendant_budget":0})).await;
        let id = accepted["id"].as_str().unwrap();
        let approval = wait_approval(http, &url, token, id).await;
        let approval_id = approval["id"].as_str().unwrap();
        let filename = if decision == "approve" {
            "reviewed.txt"
        } else {
            "cancelled.txt"
        };
        assert!(!child_workspace.join(filename).exists());
        let runs: Value = serde_json::from_slice(
            &workers[1]
                .command(&["list", "--runner", &workers[1].incarnation])
                .stdout,
        )
        .unwrap();
        let run_id = runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["approval"]["approval_id"] == approval_id)
            .unwrap()["run_id"]
            .as_str()
            .unwrap();
        let local_reply = workers[1].command(&[
            "approve",
            "--runner",
            &workers[1].incarnation,
            "--run",
            run_id,
            "--approval",
            approval_id,
            "approve",
        ]);
        assert!(
            !local_reply.status.success(),
            "Shared approval must not be authorized through local IPC"
        );
        assert!(!child_workspace.join(filename).exists());
        let old_approval_id = approval_id.to_owned();
        let before_handover = job(http, &url, token, id).await;
        let model_requests = provider.requests.lock().unwrap().len();
        shared_post(
            http,
            &url,
            token,
            &format!("jobs/{id}/handover"),
            json!({"expected_revision":before_handover["revision"],"new_assignee_id":bob["user_id"]}),
        ).await;
        let renewed = wait_approval(http, &url, token, id).await;
        let approval_id = renewed["id"].as_str().unwrap();
        assert_ne!(approval_id, old_approval_id);
        assert_eq!(renewed["attempt_id"], approval["attempt_id"]);
        assert_eq!(renewed["request"], approval["request"]);
        assert_eq!(
            provider.requests.lock().unwrap().len(),
            model_requests,
            "Reconfirmation must not replay the model or tool"
        );
        assert!(!child_workspace.join(filename).exists());
        let after_handover = job(http, &url, token, id).await;
        assert_eq!(after_handover["state"], "running");
        assert_eq!(after_handover["assignee_id"], alice["user_id"]);
        assert_eq!(
            after_handover["authority_generation"],
            before_handover["authority_generation"]
        );
        let capacity = body(
            http.get(format!("{url}/v1/shared/environments?project_id=project"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            capacity
                .as_array()
                .unwrap()
                .iter()
                .find(|environment| environment["id"] == "solver")
                .unwrap()["occupied"],
            1
        );
        let renewed_runs: Value = serde_json::from_slice(
            &workers[1]
                .command(&["list", "--runner", &workers[1].incarnation])
                .stdout,
        )
        .unwrap();
        assert_eq!(
            renewed_runs["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["approval"]["approval_id"] == approval_id)
                .unwrap()["run_id"],
            run_id
        );
        for (caller, credential, request) in [
            (http, token, old_approval_id.as_str()),
            (&identities[1].http, bob_token, approval_id),
        ] {
            let rejected = caller
                .post(format!(
                    "{url}/v1/shared/jobs/{id}/approvals/{request}/decision"
                ))
                .bearer_auth(credential)
                .json(&json!({"decision":"approve"}))
                .send()
                .await
                .unwrap();
            assert!(
                !rejected.status().is_success(),
                "Invalidated approval and pending new assignee cannot authorize the operation"
            );
        }
        assert!(!child_workspace.join(filename).exists());
        if decision == "approve" {
            shared_post(
                http,
                &url,
                token,
                &format!("jobs/{id}/approvals/{approval_id}/decision"),
                json!({"decision":"approve"}),
            )
            .await;
            wait_job(http, &url, token, id, "succeeded").await;
            assert_eq!(
                std::fs::read_to_string(child_workspace.join(filename)).unwrap(),
                "approved"
            );
        } else {
            shared_post(http, &url, token, &format!("jobs/{id}/cancel"), json!({})).await;
            wait_job(http, &url, token, id, "cancelled").await;
            assert!(!child_workspace.join(filename).exists());
            let stale = http
                .post(format!(
                    "{url}/v1/shared/jobs/{id}/approvals/{approval_id}/decision"
                ))
                .bearer_auth(token)
                .json(&json!({"decision":"approve"}))
                .send()
                .await
                .unwrap();
            assert!(!stale.status().is_success());
        }
    }
    // Exercise Guardian -> existing shared approval -> authenticated Hub -> exact effect.
    // The fixture restores the on-disk settings even if start/assertion panics; Worker Drop
    // stops the isolated host. A fresh start after this case restores its effective mode too.
    workers[1].stop().await;
    {
        struct RestoreSettings {
            path: Utf8PathBuf,
            original: Vec<u8>,
        }
        impl Drop for RestoreSettings {
            fn drop(&mut self) {
                if let Err(error) = std::fs::write(&self.path, &self.original) {
                    eprintln!(
                        "Could not restore isolated Runner settings {}: {error}",
                        self.path
                    );
                }
            }
        }
        let restore = RestoreSettings {
            path: workers[1].settings.clone(),
            original: std::fs::read(&workers[1].settings).unwrap(),
        };
        let mut settings: Value = serde_json::from_slice(&restore.original).unwrap();
        assert_eq!(settings["environments"][0]["access_mode"], "default");
        settings["environments"][0]["access_mode"] = json!("auto_review");
        std::fs::write(&restore.path, serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
        workers[1].start().await;
        // The running host captured its mapping at startup. Keep no temporary mode on disk.
        std::fs::write(&restore.path, &restore.original).unwrap();
    }
    let handoff = shared_post(http,&url,token,"jobs",json!({"request_id":"guardian-handoff-request","project_id":"project","environment_id":"solver","title":"Guardian human confirmation","input":{"version":1,"prompt":"guardian-handoff-fixture: create the controlled file after confirmation"},"descendant_budget":0})).await;
    let handoff_id = handoff["id"].as_str().unwrap();
    let handoff_approval = wait_approval(http, &url, token, handoff_id).await;
    let handoff_approval_id = handoff_approval["id"].as_str().unwrap();
    let handoff_details = handoff_approval["request"]["details"].to_string();
    assert!(
        handoff_details.contains("代理承認からの確認"),
        "{handoff_approval}"
    );
    assert!(
        handoff_details.contains("WinB側の作業フォルダ"),
        "{handoff_approval}"
    );
    assert!(
        handoff_details.contains("guardian-handoff.txt"),
        "{handoff_approval}"
    );
    assert!(!child_workspace.join("guardian-handoff.txt").exists());
    let pending_runs: Value = serde_json::from_slice(
        &workers[1]
            .command(&["list", "--runner", &workers[1].incarnation])
            .stdout,
    )
    .unwrap();
    let pending_run = pending_runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["approval"]["approval_id"] == handoff_approval_id)
        .unwrap();
    assert_eq!(pending_run["state"], "waiting_approval");
    shared_post(
        http,
        &url,
        token,
        &format!("jobs/{handoff_id}/approvals/{handoff_approval_id}/decision"),
        json!({"decision":"approve"}),
    )
    .await;
    wait_job(http, &url, token, handoff_id, "succeeded").await;
    assert_eq!(
        std::fs::read_to_string(child_workspace.join("guardian-handoff.txt")).unwrap(),
        "approved"
    );
    {
        let requests = provider.requests.lock().unwrap();
        let handoff_requests = requests
            .iter()
            .filter(|request| {
                request["messages"]
                    .to_string()
                    .contains("guardian-handoff-fixture")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            handoff_requests.len(),
            3,
            "One task request, one Guardian review, one continuation"
        );
        assert_eq!(
            handoff_requests
                .iter()
                .filter(|request| request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "system"
                        && message["content"].as_str().is_some_and(
                            |content| content.contains("independent permission guardian")
                        )))
                .count(),
            1
        );
    }
    workers[1].stop().await;
    workers[1].start().await;

    let paused = shared_post(http,&url,token,"jobs",json!({"request_id":"paused-parent-request","project_id":"project","environment_id":"analysis","title":"Cancel waiting parent","input":{"version":1,"prompt":"paused-parent-fixture: delegate and wait"},"descendant_budget":1})).await;
    let paused_id = paused["id"].as_str().unwrap();
    let paused = wait_job(http, &url, token, paused_id, "waiting_child").await;
    let paused_child = paused["awaiting_child_id"].as_str().unwrap();
    wait_approval(http, &url, token, paused_child).await;
    let paused_session = paused["checkpoint"]["session_id"].as_str().unwrap();
    let paused_turn = paused["checkpoint"]["turn_id"].as_str().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        workers[0].data.join("moyai.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let session_state = || {
        db.query_row(
            "SELECT status FROM sessions WHERE id=?1",
            [paused_session],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    };
    assert_eq!(session_state(), "running");
    // Exercise delivery after a second Runner restart as well as live cancellation propagation.
    workers[0].stop().await;
    shared_post(
        http,
        &url,
        token,
        &format!("jobs/{paused_id}/cancel"),
        json!({}),
    )
    .await;
    wait_job(http, &url, token, paused_id, "cancelled").await;
    assert_eq!(
        session_state(),
        "running",
        "No local owner ran during the outage"
    );
    workers[0].start().await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while session_state() == "running" {
        assert!(
            Instant::now() < deadline,
            "Terminal Hub parent left its local checkpoint running"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let terminals: i64 = db.query_row("SELECT COUNT(*) FROM protocol_runtime_events WHERE session_id=?1 AND turn_id=?2 AND json_extract(msg_json,'$.kind')='turn_terminal'", rusqlite::params![paused_session,paused_turn], |row| row.get(0)).unwrap();
    assert_eq!(terminals, 1, "The exact paused turn must settle once");
    assert!(!child_workspace.join("cancelled.txt").exists());
    let paused_parent_calls = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            request["messages"]
                .to_string()
                .contains("paused-parent-fixture")
        })
        .count();
    assert_eq!(
        paused_parent_calls, 1,
        "Cancellation settlement must never reenter the model"
    );
    wait_job(http, &url, token, paused_child, "cancelled").await;
    // Unlike the durable checkpoint case above, shutting down Hub during generation
    // interrupts its Gateway request. Report that failure without replay or Direct fallback.
    let interrupted = shared_post(http,&url,token,"jobs",json!({"request_id":"gateway-interruption","project_id":"project","environment_id":"solver","title":"Interrupt active Gateway request","input":{"version":1,"prompt":"gateway-interruption-fixture"},"descendant_budget":0})).await;
    let interrupted_id = interrupted["id"].as_str().unwrap();
    let interrupted_requests = || {
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request["messages"]
                    .to_string()
                    .contains("gateway-interruption-fixture")
            })
            .count()
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    while interrupted_requests() == 0 {
        assert!(
            Instant::now() < deadline,
            "Interrupted fixture model request did not start"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(interrupted_requests(), 1);
    hub.restart().await;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Ok(response) = http
            .get(format!("{url}/v1/shared/jobs/{interrupted_id}"))
            .bearer_auth(token)
            .send()
            .await
        {
            if response.status().is_success() {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "Hub did not reconnect after Gateway interruption"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let interrupted_terminal = wait_job(http, &url, token, interrupted_id, "failed").await;
    assert_eq!(
        interrupted_terminal["result"]["summary"]["terminal"]["outcome"]["kind"],
        "failed"
    );
    assert!(
        interrupted_terminal["result"]["summary"]["terminal"]["outcome"]["error"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert_eq!(
        interrupted_terminal["result"]["summary"]["terminal"]["metrics"]["model_request_count"],
        1
    );
    assert_eq!(
        interrupted_terminal["result"]["summary"]["terminal"]["tool_call_count"],
        0
    );
    provider.release_child.notify_one();
    // A separate successful job proves the worker completed reconnection and resumed
    // admission; the failed request must remain failed and must not be sent a second time.
    let reconnected = shared_post(http,&url,token,"jobs",json!({"request_id":"gateway-reconnection","project_id":"project","environment_id":"solver","title":"Verify Gateway reconnection","input":{"version":1,"prompt":"gateway-reconnection-fixture"},"descendant_budget":0})).await;
    let reconnected = wait_job(
        http,
        &url,
        token,
        reconnected["id"].as_str().unwrap(),
        "succeeded",
    )
    .await;
    assert_eq!(
        reconnected["result"]["text"],
        "Gateway reconnection verified"
    );
    assert_eq!(
        interrupted_requests(),
        1,
        "Reconnect must not replay the interrupted model request"
    );
    let after_reconnection = job(http, &url, token, interrupted_id).await;
    assert_eq!(after_reconnection["state"], "failed");
    assert_eq!(after_reconnection["result"], interrupted_terminal["result"]);
    // A stop fence must reach an already running shared command. The model's
    // second response is held so cancellation crosses a live managed process.
    let active_stop = shared_post(http,&url,token,"jobs",json!({"request_id":"shared-active-stop","project_id":"project","environment_id":"solver","title":"Stop an active managed server","input":{"version":1,"prompt":"shared-active-stop-fixture: start a finite preview, then wait"},"descendant_budget":0})).await;
    let active_stop_id = active_stop["id"].as_str().unwrap();
    let approval = wait_approval(http, &url, token, active_stop_id).await;
    shared_post(
        http,
        &url,
        token,
        &format!(
            "jobs/{active_stop_id}/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ),
        json!({"decision":"approve"}),
    )
    .await;
    let active_requests = || {
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request["messages"]
                    .to_string()
                    .contains("shared-active-stop-fixture")
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let deadline = Instant::now() + Duration::from_secs(35);
    while active_requests().len() < 2 {
        assert!(
            Instant::now() < deadline,
            "Managed shared command never reached the model's second request: {}",
            job(http, &url, token, active_stop_id).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        active_requests()[1]["messages"]
            .to_string()
            .contains("process_id"),
        "shell_start must return a real managed handle before stop"
    );
    shared_post(
        http,
        &url,
        token,
        &format!("jobs/{active_stop_id}/cancel"),
        json!({}),
    )
    .await;
    wait_job(http, &url, token, active_stop_id, "cancelled").await;
    provider.release_child.notify_one();
    let no_late_service = body(
        identities[1]
            .http
            .get(format!("{url}/v1/shared/runner/services"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(
        no_late_service.as_array().unwrap().is_empty(),
        "Stopped turn created a late retained service: {no_late_service}"
    );
    assert_eq!(
        active_requests().len(),
        2,
        "Stopped work must not replay its model request"
    );

    // A successful turn may retain one finite preview. Its lease keeps another
    // conversation queued until the exact process drains and ServiceStopped lands.
    let retained = shared_post(http,&url,token,"jobs",json!({"request_id":"shared-retained-stop","project_id":"project","environment_id":"solver","title":"Keep a finite preview","input":{"version":1,"prompt":"shared-retained-stop-fixture: keep this finite preview available"},"descendant_budget":0})).await;
    let retained_id = retained["id"].as_str().unwrap();
    let approval = wait_approval(http, &url, token, retained_id).await;
    shared_post(
        http,
        &url,
        token,
        &format!(
            "jobs/{retained_id}/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ),
        json!({"decision":"approve"}),
    )
    .await;
    let retained_job = wait_job(http, &url, token, retained_id, "succeeded").await;
    let retained_service_id = retained_job["result"]["retained_service"]["service_id"]
        .as_str()
        .expect("successful preview must publish its exact service ID");
    let leases = body(
        identities[1]
            .http
            .get(format!("{url}/v1/shared/runner/services"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        leases.as_array().unwrap().len(),
        1,
        "Runner must see the retained lease: {leases}"
    );
    assert_eq!(leases[0]["service_id"], retained_service_id);
    assert_eq!(leases[0]["stop_requested"], false);
    let waiting_for_drain = shared_post(http,&url,token,"jobs",json!({"request_id":"after-retained-stop","project_id":"project","environment_id":"solver","title":"Wait for preview drain","input":{"version":1,"prompt":"gateway-reconnection-fixture: only run after the preview stops"},"descendant_budget":0})).await;
    let waiting_id = waiting_for_drain["id"].as_str().unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        job(http, &url, token, waiting_id).await["state"],
        "queued",
        "Retained process must keep the physical slot occupied"
    );
    body(
        identities[1]
            .http
            .post(format!(
                "{url}/v1/shared/runner/services/{retained_service_id}/stop"
            ))
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    wait_job(http, &url, token, waiting_id, "succeeded").await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let leases = body(
            identities[1]
                .http
                .get(format!("{url}/v1/shared/runner/services"))
                .send()
                .await
                .unwrap(),
        )
        .await;
        if leases.as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Stopped service lease remained occupied: {leases}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["messages"]
                .to_string()
                .contains("shared-retained-stop-fixture"))
            .count(),
        2,
        "Retained preview turn must not replay after stop"
    );
    let status = body(
        http.get(format!("{url}/v1/shared/status?project_id=project"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap(),
    )
    .await;
    std::fs::write(
        root.join("status-final.json"),
        serde_json::to_vec_pretty(&status).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("provider-requests.json"),
        serde_json::to_vec_pretty(&*provider.requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    workers[0].stop().await;
    workers[1].stop().await;
    eprintln!("parent+child actual process integration PASS; evidence={root}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires current MOYAI_TEST_HUB_EXE and MOYAI_TEST_RUNNER_EXE; starts isolated real Windows processes"]
async fn real_hub_two_runners_resume_parent_after_restart_without_repeating_child() {
    tokio::time::timeout(Duration::from_secs(300), real_hub_scenario())
        .await
        .expect("Real shared-work integration deadline");
}
