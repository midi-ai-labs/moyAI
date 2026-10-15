mod provider_diagnostics_tests {
    use super::*;
    use crate::harness::{HarnessEventStore as _, HarnessRunStore as _};
    use std::io::{Read as _, Write as _};

    fn recorded_runs(
        store: &StoreBundle,
        session_id: SessionId,
    ) -> Vec<crate::harness::HarnessRunRecord> {
        let connection = rusqlite::Connection::open_with_flags(
            store.paths().database_path.as_std_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("read fixture diagnostic records");
        let mut statement = connection
            .prepare("SELECT id FROM harness_runs WHERE session_id = ?1 ORDER BY id")
            .expect("diagnostic run identities");
        let ids = statement
            .query_map([session_id.to_string()], |row| row.get::<_, String>(0))
            .expect("read diagnostic identities")
            .collect::<Result<Vec<_>, _>>()
            .expect("diagnostic identities");
        ids.into_iter()
            .map(|id| {
                store
                    .harness_run_store()
                    .get_run(id.parse().expect("typed diagnostic run identity"))
                    .expect("read diagnostic run")
                    .expect("diagnostic run remains available")
            })
            .collect()
    }

    fn rejection_server(
        reason: &'static str,
    ) -> (String, std::thread::JoinHandle<(String, serde_json::Value)>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback provider");
        let address = listener.local_addr().expect("loopback address");
        listener
            .set_nonblocking(true)
            .expect("bounded fixture acceptance");
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider request did not arrive");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback provider accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bounded fixture reads");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("bounded fixture writes");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let count = stream.read(&mut chunk).expect("provider request headers");
                assert_ne!(count, 0, "provider closed before the request headers");
                request.extend_from_slice(&chunk[..count]);
                assert!(request.len() <= 64 * 1024, "fixture request bound");
                if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..header_end])
                .expect("HTTP fixture headers")
                .to_string();
            let request_line = headers.lines().next().expect("request line").to_string();
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .expect("prepared body has a content length")
                .1
                .trim()
                .parse::<usize>()
                .expect("request content length");
            assert!(length <= 64 * 1024, "fixture body bound");
            while request.len() - header_end < length {
                let count = stream.read(&mut chunk).expect("provider request body");
                assert_ne!(count, 0, "provider closed before the complete request body");
                request.extend_from_slice(&chunk[..count]);
                assert!(request.len() <= 128 * 1024, "fixture envelope bound");
            }
            let body = serde_json::from_slice(&request[header_end..header_end + length])
                .expect("serialized provider request");
            let response = serde_json::to_vec(&serde_json::json!({
                "error": {
                    "message": reason,
                    "type": "invalid_request_error",
                    "code": null,
                    "param": null
                }
            }))
            .expect("provider rejection JSON");
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .expect("provider rejection headers");
            stream
                .write_all(&response)
                .expect("provider rejection body");
            (request_line, body)
        });
        (format!("http://{address}"), worker)
    }

    #[tokio::test]
    async fn rejected_side_request_keeps_private_diagnostics_out_of_public_history() {
        const PRIVATE_REASON: &str = "Prompt too long: PRIVATE_SIDE_REJECTION_DETAIL at https://diagnostic.fixture.invalid/private-provider-route";
        let (base_url, server) = rejection_server(PRIVATE_REASON);
        let (_temp, store, owner_session_id) = canonical_side_chat_fixture().await;
        seed_history(
            &store,
            owner_session_id,
            TurnId::new(),
            0,
            HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "Keep the Main conversation unchanged.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        );
        let mut request_profile = profile();
        request_profile.base_url = base_url;
        request_profile.provider_profile = ProviderProfile::OpenAiCompatible;
        request_profile.request_timeout_ms = 5_000;
        request_profile.connect_timeout_ms = 1_000;
        let binding = store
            .side_chat_repo()
            .ensure(
                owner_session_id,
                crate::storage::SideChatProviderTarget {
                    route_kind: crate::storage::SideChatRouteKind::Direct,
                    base_url: request_profile.base_url.clone(),
                    model: request_profile.model.clone(),
                    provider_profile: request_profile.provider_profile,
                    api_key_env: None,
                    system_prompt: String::new(),
                    context_window: request_profile.context_window,
                    request_timeout_ms: request_profile.request_timeout_ms,
                    connect_timeout_ms: request_profile.connect_timeout_ms,
                    max_retries: 0,
                    supports_images: false,
                    supports_tools: false,
                },
            )
            .expect("owned Side conversation");
        let service = crate::session::SessionService::new(store.clone());
        let before_main = service
            .canonical_session_snapshot(owner_session_id, 0, 100, 0, 100)
            .await
            .expect("Main before the Side request");
        let mut observed_phases = Vec::new();
        let public_error = execute_canonical_side_chat(
            store.clone(),
            binding.conversation_session_id,
            request_profile,
            "Explain the recorded work.".to_string(),
            CancellationToken::new(),
            |event| {
                if let SideChatStreamEvent::ProviderPhase(phase) = event {
                    observed_phases.push(phase);
                }
            },
        )
        .await
        .expect_err("the provider rejects this request before generation");
        let (request_line, body) = server.join().expect("loopback provider completed");
        assert!(request_line.starts_with("POST /v1/chat/completions "));
        assert_eq!(body["stream"], true);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("tools").is_none());
        assert!(public_error.contains("HTTP 400"));

        let runs = recorded_runs(&store, binding.conversation_session_id);
        assert_eq!(runs.len(), 1, "one private run owns the Side attempt");
        let run = &runs[0];
        assert_eq!(run.status, crate::harness::HarnessRunStatus::Fail);
        assert!(run.completed_at_ms.is_some());
        let events = store
            .harness_event_store()
            .list_events(run.id)
            .expect("typed private diagnostic events");
        let recorded_phases = events
            .iter()
            .filter_map(|event| match &event.payload {
                crate::harness::HarnessEventPayload::Generic(value) => {
                    match serde_json::from_value::<RunEvent>(value.clone()) {
                        Ok(RunEvent::ProviderPhase { response_id, event }) => {
                            Some((response_id, event))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recorded_phases.len(), observed_phases.len());
        assert!(!recorded_phases.is_empty());
        for ((response_id, recorded), observed) in recorded_phases.iter().zip(&observed_phases) {
            assert_eq!(recorded, observed);
            assert_eq!(*response_id, recorded_phases[0].0);
            assert_eq!(recorded.request_id, recorded_phases[0].1.request_id);
            assert_eq!(recorded.attempt, 1);
        }
        let failed_phase = recorded_phases.last().expect("provider terminal phase");
        assert_eq!(
            failed_phase.1.phase,
            crate::llm::ProviderPhase::ProviderTerminal
        );
        assert_eq!(
            failed_phase.1.terminal_status,
            Some(crate::llm::ProviderTerminalStatus::Failed)
        );
        let failure = failed_phase.1.failure.as_ref().expect("private failure");
        assert_eq!(failure.kind, crate::llm::ProviderFailureKind::HttpStatus);
        assert_eq!(failure.status, Some(400));
        assert_eq!(failure.code, None);
        assert!(failure.message.contains(PRIVATE_REASON));
        assert_eq!(failure.request_id, failed_phase.1.request_id);

        let canonical_events = store
            .protocol_event_store()
            .list_runtime_events_for_session(binding.conversation_session_id)
            .expect("canonical Side events");
        let terminals = canonical_events
            .iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 1);
        assert_eq!(run.protocol_turn_id, Some(terminals[0].turn_id));
        assert_eq!(
            run.canonical_terminal_runtime_event_id,
            Some(terminals[0].id)
        );
        let RuntimeEventMsg::TurnTerminal { terminal } = &terminals[0].msg else {
            unreachable!("filtered canonical terminal")
        };
        assert!(matches!(
            &terminal.outcome,
            TurnTerminalOutcome::Failed { error } if error == &public_error
        ));
        assert_eq!(terminal.metrics.model_request_count, 1);
        assert_eq!(terminal.final_response_id, None);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == crate::harness::HarnessEventKind::RunTerminalized)
                .count(),
            1
        );
        let projection = store
            .side_chat_repo()
            .conversation_projection(owner_session_id)
            .expect("public Side projection")
            .expect("retained Side binding");
        assert_eq!(projection.status, SessionStatus::Failed);
        assert_eq!(
            projection.last_error.as_deref(),
            Some(public_error.as_str())
        );
        let read = crate::session::markdown::canonical_markdown_export_read(
            &service,
            binding.conversation_session_id,
        )
        .await
        .expect("canonical Side export input");
        let markdown = crate::session::canonical_session_read_to_markdown(&read);
        let public_views = [
            public_error,
            serde_json::to_string(&canonical_events).expect("canonical event JSON"),
            serde_json::to_string(&read).expect("canonical history JSON"),
            serde_json::to_string(&projection).expect("Side projection JSON"),
            markdown,
        ];
        let response_identity = failed_phase.0.to_string();
        for view in public_views {
            for private in [
                "PRIVATE_SIDE_REJECTION_DETAIL",
                "diagnostic.fixture.invalid",
                failure.request_id.as_str(),
                response_identity.as_str(),
            ] {
                assert!(
                    !view.contains(private),
                    "private diagnostic entered a public view"
                );
            }
        }
        assert_eq!(
            serde_json::to_value(
                service
                    .canonical_session_snapshot(owner_session_id, 0, 100, 0, 100)
                    .await
                    .expect("Main after the Side request")
            )
            .expect("Main after JSON"),
            serde_json::to_value(before_main).expect("Main before JSON")
        );
        assert!(recorded_runs(&store, owner_session_id).is_empty());
    }

    #[tokio::test]
    async fn side_setup_failure_and_pre_cancel_settle_private_runs_from_canonical_terminal() {
        for cancelled in [false, true] {
            let (_temp, store, session_id) = canonical_side_chat_fixture().await;
            let mut request_profile = profile();
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            } else {
                let missing_key = format!("MOYAI_DIAGNOSTIC_MISSING_KEY_{}", ulid::Ulid::new());
                assert!(std::env::var_os(&missing_key).is_none());
                request_profile.api_key_env = Some(missing_key);
            }
            let result = execute_canonical_side_chat(
                store.clone(),
                session_id,
                request_profile,
                "A request that must not reach a provider.".to_string(),
                cancel,
                |_| {},
            )
            .await;
            assert_eq!(result.is_ok(), cancelled);
            let runs = recorded_runs(&store, session_id);
            assert_eq!(runs.len(), 1);
            let run = &runs[0];
            assert_eq!(
                run.status,
                if cancelled {
                    crate::harness::HarnessRunStatus::Blocked
                } else {
                    crate::harness::HarnessRunStatus::Fail
                }
            );
            assert!(run.completed_at_ms.is_some());
            let canonical = store
                .protocol_event_store()
                .list_runtime_events_for_session(session_id)
                .expect("pre-request canonical terminal");
            let terminals = canonical
                .iter()
                .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
                .collect::<Vec<_>>();
            assert_eq!(terminals.len(), 1);
            assert_eq!(run.protocol_turn_id, Some(terminals[0].turn_id));
            assert_eq!(
                run.canonical_terminal_runtime_event_id,
                Some(terminals[0].id)
            );
            let RuntimeEventMsg::TurnTerminal { terminal } = &terminals[0].msg else {
                unreachable!("filtered canonical terminal")
            };
            assert_eq!(terminal.metrics.model_request_count, 0);
            let events = store
                .harness_event_store()
                .list_events(run.id)
                .expect("pre-request private diagnostic events");
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.kind == crate::harness::HarnessEventKind::RunTerminalized)
                    .count(),
                1
            );
        }
    }
}
