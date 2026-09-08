use super::*;
use sha2::{Digest, Sha256};

#[tokio::test]
async fn received_inputs_are_readable_and_outputs_are_frozen_for_the_exact_job() {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captured.clone();
    let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
        let captured = requests.clone();
        async move {
            let index = { let mut requests = captured.lock().unwrap(); let index = requests.len(); requests.push(body.clone()); index };
            let (delta, finish) = match index {
                0 => {
                    let directory = body["messages"].as_array().unwrap().iter()
                        .filter(|message| message["role"] == "system")
                        .filter_map(|message| message["content"].as_str())
                        .flat_map(str::lines).filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .find(|value| value.get("directory").is_some()).unwrap()["directory"].as_str().unwrap().to_owned();
                    let arguments = json!({"path":Utf8PathBuf::from(directory).join("reference.txt")});
                    (json!({"role":"assistant","tool_calls":[{"index":0,"id":"read-input","type":"function","function":{"name":"read","arguments":arguments.to_string()}}]}), "tool_calls")
                }
                1 => {
                    let arguments = json!({"patch_text":"*** Begin Patch\n*** Add File: generated.txt\n+Generated from immutable input.\n*** End Patch"});
                    (json!({"role":"assistant","tool_calls":[{"index":0,"id":"write-output","type":"function","function":{"name":"apply_patch","arguments":arguments.to_string()}}]}), "tool_calls")
                }
                _ => (json!({"role":"assistant","content":"Created generated.txt."}), "stop"),
            };
            let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
            let end = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
            ([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (_temp, app, profile) = fixture(&endpoint).await;
    let (network, jobs, dispatcher, authority, _peer) = managed_fixture(&app, &profile).await;
    let input = "immutable-input-sentinel-9742";
    let mut request = task("request-one");
    request["inputs"] = json!([{"path":"reference.txt","sha256":format!("{:x}",Sha256::digest(input.as_bytes())),"text":input}]);
    let accepted = dispatcher
        .call_authorized(
            "delegate_task",
            request.clone(),
            CancellationToken::new(),
            authority.clone(),
        )
        .await
        .unwrap();
    let id = accepted["structuredContent"]["job_id"].as_str().unwrap();
    let bundle = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(bundle) = dispatcher
                .call_authorized(
                    "task_artifacts",
                    json!({"job_id":id}),
                    CancellationToken::new(),
                    authority.clone(),
                )
                .await
            {
                return bundle["structuredContent"].clone();
            }
            assert!(
                jobs.pending_approval().is_none(),
                "the exact staged input root grants read access without expanding writes"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !app.workspace.root.join("reference.txt").exists(),
        "inputs never overwrite or install receiver workspace files"
    );
    assert_eq!(bundle["manifest"]["job_id"], id);
    assert_eq!(bundle["manifest"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(bundle["files"][0]["path"], "generated.txt");
    assert_eq!(
        bundle["files"][0]["text"]
            .as_str()
            .unwrap()
            .replace("\r\n", "\n"),
        "Generated from immutable input.\n"
    );
    let records = captured.lock().unwrap().clone();
    assert_eq!(records.len(), 3);
    assert!(
        records[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool" && message.to_string().contains(input)),
        "the real read tool must return the received file bytes"
    );
    let staged = records[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .flat_map(str::lines)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v.get("directory").is_some())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while jobs.has_active_jobs() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !Utf8PathBuf::from(staged["directory"].as_str().unwrap()).exists(),
        "the worker owns and releases its disposable input copy"
    );
    std::fs::write(
        app.workspace.root.join("generated.txt"),
        "later receiver change",
    )
    .unwrap();
    let repeated = dispatcher
        .call_authorized(
            "task_artifacts",
            json!({"job_id":id,"version":bundle["manifest"]["version"]}),
            CancellationToken::new(),
            authority.clone(),
        )
        .await
        .unwrap();
    assert_eq!(repeated["structuredContent"], bundle);
    for (args, other) in [
        (json!({"job_id":Ulid::new()}), authority.clone()),
        (
            json!({"job_id":id,"version":"0".repeat(64)}),
            authority.clone(),
        ),
        (json!({"job_id":id}), {
            let mut other = authority.clone();
            other.claims.root_task_id = "other-root".into();
            other
        }),
        (json!({"job_id":id}), {
            let mut other = authority.clone();
            other.claims.actor_device_id = "other-caller".into();
            other
        }),
    ] {
        assert!(
            dispatcher
                .call_authorized("task_artifacts", args, CancellationToken::new(), other)
                .await
                .is_err()
        );
    }
    request["inputs"][0]["text"] = json!("changed");
    request["inputs"][0]["sha256"] = json!(format!("{:x}", Sha256::digest(b"changed")));
    assert!(
        dispatcher
            .call_authorized(
                "delegate_task",
                request,
                CancellationToken::new(),
                authority
            )
            .await
            .is_err(),
        "the same key cannot replace accepted input versions"
    );
    assert_eq!(
        captured.lock().unwrap().len(),
        3,
        "artifact reads and conflicting retries never replay work"
    );
    network.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn receiver_diagnostics_echo_target_without_admitting_work_or_claiming_remote_reachability() {
    let (_temp, app, profile) = fixture("http://127.0.0.1:1/v1").await;
    let (network, jobs, _dispatcher, _authority, _peer) = managed_fixture(&app, &profile).await;
    let before = network.projection_now();
    let result = network
        .diagnose(
            crate::device_network::DiagnosticScope::Receiver,
            None,
            None,
            &before.revision,
            &before.generation,
        )
        .await
        .unwrap();
    assert_eq!(result.revision, before.revision);
    assert_eq!(result.generation, before.generation);
    assert!(result.device_id.is_none() && result.profile_id.is_none());
    assert!(
        result
            .stages
            .iter()
            .any(|stage| stage.key == "firewall" && stage.status == "skipped")
    );
    assert!(!result.stages.iter().any(|stage| stage.key == "mcp"));
    assert!(
        app.store
            .remote_job_store()
            .recent_all(64)
            .unwrap()
            .is_empty()
    );
    assert!(jobs.pending_approval().is_none());
    assert_eq!(network.projection_now().revision, before.revision);
    assert!(
        network
            .diagnose(
                crate::device_network::DiagnosticScope::Peer,
                Some("device-c".into()),
                None,
                &before.revision,
                &before.generation
            )
            .await
            .is_err()
    );
    assert!(
        network
            .diagnose(
                crate::device_network::DiagnosticScope::Receiver,
                None,
                None,
                "stale-revision",
                &before.generation
            )
            .await
            .is_err()
    );
    network.shutdown().await;
}
