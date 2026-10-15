const MODEL_COMPACTION_OLD_READ: &str = "OLD_READ_EVIDENCE ";
const MODEL_COMPACTION_LATEST_READ: &str = "LATEST_READ_EVIDENCE ";
const MODEL_COMPACTION_SIBLING_READ: &str = "SIBLING_READ_EVIDENCE";
const MODEL_COMPACTION_SUMMARY: &str = "Earlier inspection found the initial file content. Continue writing hello.txt using the latest read evidence.";

struct ModelCompactionReadFixture;

#[async_trait(?Send)]
impl crate::tool::registry::Tool for ModelCompactionReadFixture {
    fn spec(&self) -> crate::tool::ToolSpec {
        crate::tool::ToolSpec {
            name: ToolName::Read,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "return distinct inspection evidence for the requested fixture",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"fixture": {"type": "string"}},
                "required": ["fixture"],
            }),
        }
    }

    async fn execute(
        &self,
        arguments: Value,
        _ctx: crate::tool::context::ToolContext<'_>,
    ) -> Result<ToolResult, crate::error::ToolError> {
        let output_text = match arguments["fixture"].as_str() {
            Some("old") => MODEL_COMPACTION_OLD_READ.repeat(1_500),
            Some("latest") => MODEL_COMPACTION_LATEST_READ.repeat(200),
            Some("sibling") => MODEL_COMPACTION_SIBLING_READ.to_string(),
            other => panic!("unexpected read fixture {other:?}"),
        };
        Ok(ToolResult {
            title: "inspection evidence".to_string(),
            output_text,
            metadata: serde_json::json!({"success": true}),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

fn model_compaction_tool_events(call_id: &str, tool_name: &str, arguments: Value) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
        },
        LlmEvent::ToolCallArgsDelta {
            call_id: call_id.to_string(),
            delta: arguments.to_string(),
        },
    ]
}

async fn run_model_compaction_fixture(summary: &str, sibling: bool) -> ScriptedRun {
    run_model_compaction_fixture_with_steers(summary, sibling, Vec::new()).await
}

async fn run_model_compaction_fixture_with_steers(
    summary: &str,
    sibling: bool,
    pending_steers: Vec<SteerTurn>,
) -> ScriptedRun {
    let mut config = ResolvedConfig::default();
    config.model.context_window = 128_000;
    config.model.max_output_tokens = 512;
    config.session.overflow_margin_tokens = 128;
    let mut latest_events = vec![LlmEvent::TextDelta("Latest inspection commentary.".into())];
    latest_events.extend(model_compaction_tool_events(
        "latest-read",
        "read",
        serde_json::json!({"fixture": "latest"}),
    ));
    let mut compact_events = vec![LlmEvent::TextDelta("Compaction batch commentary.".into())];
    compact_events.extend(model_compaction_tool_events(
        "model-compact",
        "compact_context",
        serde_json::json!({"summary": summary}),
    ));
    if sibling {
        compact_events.extend(model_compaction_tool_events(
            "sibling-read",
            "read",
            serde_json::json!({"fixture": "sibling"}),
        ));
    }
    run_scripted_internal_with_prior_user_and_api_key_resolver(
        config,
        vec![
            ScriptedOutcome::Response(ScriptedResponse {
                events: model_compaction_tool_events(
                    "old-read",
                    "read",
                    serde_json::json!({"fixture": "old"}),
                ),
                finish_reason: FinishReason::ToolCall,
            }),
            ScriptedOutcome::Response(ScriptedResponse {
                events: latest_events,
                finish_reason: FinishReason::ToolCall,
            }),
            ScriptedOutcome::Response(ScriptedResponse {
                events: compact_events,
                finish_reason: FinishReason::ToolCall,
            }),
            ScriptedOutcome::Response(ScriptedResponse {
                events: vec![LlmEvent::TextDelta(
                    "Model compaction fixture complete.".into(),
                )],
                finish_reason: FinishReason::Stop,
            }),
        ],
        None,
        pending_steers,
        crate::cli::ReviewDecision::Approved,
        RunControl::new(),
        Some(Arc::new(ModelCompactionReadFixture)),
        false,
        None,
        None,
        None,
        true,
        None,
        None,
    )
    .await
    .expect("model compaction fixture")
}

fn assert_model_compaction_tool_pair(messages: &[ModelMessage], expected_call_id: &str) {
    let matching_calls = messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::AssistantToolCalls { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| call.call_id == expected_call_id)
        .count();
    let matching_outputs = messages
        .iter()
        .filter(|message| {
            matches!(message, ModelMessage::Tool { call_id, .. } if call_id == expected_call_id)
        })
        .count();
    assert_eq!(matching_calls, 1, "native call {expected_call_id}");
    assert_eq!(matching_outputs, 1, "native output {expected_call_id}");
}

#[tokio::test]
async fn model_compaction_retains_recent_evidence_and_append_only_history() {
    let run = run_model_compaction_fixture(MODEL_COMPACTION_SUMMARY, false).await;
    assert!(run.summary.is_ok(), "fixture run succeeds");
    assert_eq!(run.requests.len(), 4, "the model supplies its own summary");
    assert!(run.requests.iter().all(|request| !request.tools.is_empty()));
    let final_request = run.requests.last().expect("post-compaction request");
    let projected = serde_json::to_string(&final_request.messages).expect("request messages");
    assert_eq!(final_request.messages.iter().filter(|message| matches!(
        message, ModelMessage::User { content } if content.contains(MODEL_COMPACTION_SUMMARY)
    )).count(), 1, "one semantic checkpoint carries the supplied summary");
    assert!(!projected.contains(MODEL_COMPACTION_OLD_READ));
    assert!(final_request.messages.iter().any(|message| matches!(
        message,
        ModelMessage::Tool { call_id, result, .. }
            if call_id == "latest-read" && result == &MODEL_COMPACTION_LATEST_READ.repeat(200)
    )));
    assert_eq!(
        projected.matches("Latest inspection commentary.").count(),
        1
    );
    assert_eq!(projected.matches("Compaction batch commentary.").count(), 1);
    assert_eq!(
        final_request
            .messages
            .iter()
            .filter(|message| matches!(
                message, ModelMessage::User { content } if content == "write hello.txt"
            ))
            .count(),
        1
    );
    assert_model_compaction_tool_pair(&final_request.messages, "latest-read");
    assert_model_compaction_tool_pair(&final_request.messages, "model-compact");

    let history = run
        .store
        .protocol_event_store()
        .list_history_items_for_session(run.session_id)
        .expect("canonical history");
    let checkpoints = history
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::Compaction {
                layout,
                summary,
                replacement_item_ids,
                ..
            } => Some((layout, summary, replacement_item_ids)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(layout, summary, replaced)] = checkpoints.as_slice() else {
        panic!("exactly one model checkpoint is durable")
    };
    assert_eq!(
        **layout,
        crate::protocol::CompactionLayout::UserAnchoredCheckpoint
    );
    assert_eq!(*summary, MODEL_COMPACTION_SUMMARY);
    for id in history_tool_unit_ids(&history, "old-read") {
        assert!(
            replaced.contains(&id),
            "older completed evidence is summarized"
        );
    }
    for call_id in ["latest-read", "model-compact"] {
        for id in history_tool_unit_ids(&history, call_id) {
            assert!(
                !replaced.contains(&id),
                "retained {call_id} unit is complete"
            );
        }
    }
    assert!(
        history.iter().any(|item| matches!(
            &item.payload, HistoryItemPayload::ToolOutput { output_text, .. }
                if output_text == &MODEL_COMPACTION_OLD_READ.repeat(1_500)
        )),
        "compaction appends a checkpoint without deleting canonical raw output"
    );

    let mut builder = context_manager::ContextManager::active_history_builder();
    let boundary = run
        .store
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            run.session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                builder.ingest_page(page.items);
                Ok(())
            },
        )
        .expect("resumed active history");
    let resumed = builder.finish(boundary.append_fence, boundary.canonical_count);
    let replayed = context_manager::ContextManager::rehydrate(history);
    assert_eq!(
        serde_json::to_value(resumed.model_messages(false)).unwrap(),
        serde_json::to_value(replayed.model_messages(false)).unwrap(),
        "active-history loading and append-only replay resume the same context"
    );
}

#[tokio::test]
async fn model_compaction_waits_for_the_complete_sibling_tool_batch() {
    let run = run_model_compaction_fixture(MODEL_COMPACTION_SUMMARY, true).await;
    assert!(run.summary.is_ok());
    assert_eq!(run.requests.len(), 4);
    let request = run.requests.last().expect("post-batch request");
    for call_id in ["latest-read", "model-compact", "sibling-read"] {
        assert_model_compaction_tool_pair(&request.messages, call_id);
    }
    assert!(
        request.messages.iter().any(|message| matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.len() == 2
                    && tool_calls.iter().any(|call| call.call_id == "model-compact")
                    && tool_calls.iter().any(|call| call.call_id == "sibling-read")
        )),
        "the current provider batch remains one native assistant response"
    );
    assert!(request.messages.iter().any(|message| matches!(
        message,
        ModelMessage::Tool { call_id, result, .. }
            if call_id == "sibling-read" && result == MODEL_COMPACTION_SIBLING_READ
    )));
    let history = run
        .store
        .protocol_event_store()
        .list_history_items_for_session(run.session_id)
        .expect("canonical history");
    let replaced = history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::Compaction {
                replacement_item_ids,
                ..
            } => Some(replacement_item_ids),
            _ => None,
        })
        .expect("model checkpoint");
    for call_id in ["model-compact", "sibling-read"] {
        for id in history_tool_unit_ids(&history, call_id) {
            assert!(
                !replaced.contains(&id),
                "checkpoint source excludes its own batch"
            );
        }
    }
}

#[tokio::test]
async fn model_compaction_preserves_the_latest_explicit_steer_as_raw_input() {
    const LATEST_STEER: &str =
        "Preserve the newest file evidence and write the greeting in Japanese.";
    let run = run_model_compaction_fixture_with_steers(
        MODEL_COMPACTION_SUMMARY,
        false,
        vec![SteerTurn {
            expected_turn_id: TurnId::new(),
            items: vec![UserInputItem::Text {
                text: LATEST_STEER.into(),
            }],
            additional_context: Default::default(),
            client_user_message_id: Some("model-compaction-steer".into()),
        }],
    )
    .await;
    assert!(run.summary.is_ok());
    assert_eq!(run.requests.len(), 4);
    let request = run.requests.last().expect("post-compaction request");
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|message| matches!(
                message, ModelMessage::User { content } if content == LATEST_STEER
            ))
            .count(),
        1,
        "the latest user correction remains a raw user input"
    );
    let history = run
        .store
        .protocol_event_store()
        .list_history_items_for_session(run.session_id)
        .expect("canonical history");
    let steer_id = history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::SteerTurn { content, .. }
                if content_text(content) == LATEST_STEER =>
            {
                Some(item.id)
            }
            _ => None,
        })
        .expect("canonical explicit steer");
    let replaced = history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::Compaction {
                replacement_item_ids,
                ..
            } => Some(replacement_item_ids),
            _ => None,
        })
        .expect("model checkpoint");
    assert!(!replaced.contains(&steer_id));
}

#[tokio::test]
async fn model_compaction_rejects_invalid_and_expanding_summaries_without_replacing_history() {
    for summary in ["   ".to_string(), "EXPANDING_SUMMARY ".repeat(5_000)] {
        let run = run_model_compaction_fixture(&summary, false).await;
        assert!(
            run.summary.is_ok(),
            "the agent continues after rejecting the summary"
        );
        assert_eq!(
            run.requests.len(),
            4,
            "rejection does not request a repair summary"
        );
        let request = run
            .requests
            .last()
            .expect("request after rejected compaction");
        for call_id in ["old-read", "latest-read", "model-compact"] {
            assert_model_compaction_tool_pair(&request.messages, call_id);
        }
        assert!(request.messages.iter().any(|message| matches!(
            message, ModelMessage::Tool { call_id, result, .. }
                if call_id == "old-read" && result == &MODEL_COMPACTION_OLD_READ.repeat(1_500)
        )));
        let history = run
            .store
            .protocol_event_store()
            .list_history_items_for_session(run.session_id)
            .expect("canonical history");
        assert!(
            !history
                .iter()
                .any(|item| matches!(item.payload, HistoryItemPayload::Compaction { .. })),
            "unusable summaries never become durable checkpoints"
        );
    }
}

const MODEL_COMPACTION_SAVE_SUMMARY: &str = "The earlier inspection completed. Continue the task.";

struct ModelCompactionSaveFixture {
    store: StoreBundle,
    agent: AgentLoop,
    request: AgentRunRequest,
    template: ChatRequest,
    source_ids: Vec<HistoryItemId>,
}

async fn model_compaction_save_fixture() -> ModelCompactionSaveFixture {
    let mut config = ResolvedConfig::default();
    config.model.context_window = 128_000;
    config.model.max_output_tokens = 512;
    let previous = run_scripted_with_control_tool_and_protocol_recording(
        config.clone(),
        vec![
            ScriptedResponse {
                events: model_compaction_tool_events(
                    "settled-old-read",
                    "read",
                    serde_json::json!({"fixture": "old"}),
                ),
                finish_reason: FinishReason::ToolCall,
            },
            ScriptedResponse {
                events: vec![LlmEvent::TextDelta("Previous inspection finished.".into())],
                finish_reason: FinishReason::Stop,
            },
        ],
        RunControl::new(),
        Arc::new(ModelCompactionReadFixture),
    )
    .await
    .expect("completed source turn");
    assert_eq!(
        previous.summary.as_ref().unwrap().status(),
        SessionStatus::Completed
    );
    let store = previous.store;
    let source_ids = history_tool_unit_ids(
        &store
            .protocol_event_store()
            .list_history_items_for_session(previous.session_id)
            .unwrap(),
        "settled-old-read",
    )
    .to_vec();
    let turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_session_turn(previous.session_id, turn_id)
        .await
        .unwrap()
        .expect("admitted follow-up turn");
    store
        .session_repo()
        .append_user_turn_with_protocol_bundle(
            previous.session_id,
            admission.admission_id,
            &UserTurn {
                turn_id,
                items: vec![UserInputItem::Text {
                    text: "Continue the task.".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
            turn_id,
            0,
        )
        .await
        .unwrap();
    let session = SessionContext {
        session: store
            .session_repo()
            .get_session(previous.session_id)
            .await
            .unwrap(),
        workspace: WorkspaceDiscovery::discover_fixed_root(&previous.root, &config).unwrap(),
    };
    let mut builder = context_manager::ContextManager::active_history_builder();
    let active = store
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            previous.session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                builder.ingest_page(page.items);
                Ok(())
            },
        )
        .unwrap();
    let context = builder.finish(active.append_fence, active.canonical_count);
    let mut template = previous.requests.last().unwrap().clone();
    template.messages = context.model_messages(false);
    let tools = test_tool_services(&config, &store, store.paths().clone());
    let agent = AgentLoop::new(
        Arc::new(ScriptedClient {
            outcomes: Mutex::new(Vec::new()),
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        ToolRegistry::builtin(tools.clone()),
        store.clone(),
        PromptBuilder,
        tools,
    );
    let collaboration_mode = mode::CollaborationMode::resolve(mode::ModeKind::Default);
    let policy = crate::llm::model_policy::ResolvedTurnPolicy::resolve(
        &collaboration_mode,
        crate::llm::model_policy::ModelPolicy::from_config(&config),
        crate::llm::model_policy::ProviderCapabilities::from_config(&config),
        config.model.reasoning_summary,
    )
    .unwrap();
    let request = AgentRunRequest {
        session,
        turn: Arc::new(turn_context::TurnContext {
            turn_id,
            admission_id: admission.admission_id,
            mode: collaboration_mode,
            policy: Arc::new(policy),
            config: Arc::new(crate::config::ResolvedTurnConfig::capture(config).unwrap()),
            goal: None,
            current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
        }),
        context,
        run_control: RunControl::new(),
        agent_context: None,
        initial_user_history_item_id: None,
    };
    ModelCompactionSaveFixture {
        store,
        agent,
        request,
        template,
        source_ids,
    }
}

fn model_compaction_save_snapshot(store: &StoreBundle, request: &AgentRunRequest) -> Value {
    serde_json::json!({
        "history": store.protocol_event_store().list_history_items_for_session(request.session.session.id).unwrap(),
        "turns": store.protocol_event_store().list_turn_items_for_session(request.session.session.id).unwrap(),
        "events": store.protocol_event_store().list_runtime_events(request.session.session.id, request.turn_id()).unwrap(),
        "projection": request.context.model_messages(false),
    })
}

#[tokio::test]
async fn model_compaction_save_stop_first_keeps_an_owned_admission_unchanged() {
    let ModelCompactionSaveFixture {
        store,
        agent,
        mut request,
        template,
        source_ids,
    } = model_compaction_save_fixture().await;
    let before = model_compaction_save_snapshot(&store, &request);
    assert!(
        request
            .run_control
            .interrupt(TurnInterruptionCause::UserStop)
    );
    let mut sink = CapturingSink::default();
    agent
        .apply_model_compaction(
            &mut request,
            &ContextWindowTokenStatus::for_request(&template, 0),
            MODEL_COMPACTION_SAVE_SUMMARY.into(),
            source_ids,
            &mut sink,
        )
        .await
        .expect_err("Stop wins before a tool-settlement reservation");
    assert_eq!(model_compaction_save_snapshot(&store, &request), before);
    assert!(sink.events.is_empty());
    assert!(
        matches!(
            store
                .session_repo()
                .admitted_run_state(
                    request.session.session.id,
                    request.admission_id(),
                    request.turn_id(),
                )
                .await
                .unwrap(),
            AdmittedRunState::OwnedRunning
        ),
        "the rejection is caused by in-process Stop while database ownership is valid"
    );
}

#[tokio::test]
async fn model_compaction_save_rejects_stale_admission_without_a_checkpoint() {
    let ModelCompactionSaveFixture {
        store,
        agent,
        mut request,
        template,
        source_ids,
    } = model_compaction_save_fixture().await;
    let owned_admission_id = request.admission_id();
    Arc::make_mut(&mut request.turn).admission_id = crate::session::AdmissionId::new();
    let before = model_compaction_save_snapshot(&store, &request);
    let mut sink = CapturingSink::default();
    agent
        .apply_model_compaction(
            &mut request,
            &ContextWindowTokenStatus::for_request(&template, 0),
            MODEL_COMPACTION_SAVE_SUMMARY.into(),
            source_ids,
            &mut sink,
        )
        .await
        .expect_err("a caller with no matching admission cannot save compaction");
    assert_eq!(model_compaction_save_snapshot(&store, &request), before);
    assert!(sink.events.is_empty());
    assert!(
        matches!(
            store
                .session_repo()
                .admitted_run_state(
                    request.session.session.id,
                    owned_admission_id,
                    request.turn_id(),
                )
                .await
                .unwrap(),
            AdmittedRunState::OwnedRunning
        ),
        "the current owner retains its admission"
    );
}

struct ModelCompactionStopAfterCommit {
    store: StoreBundle,
    control: RunControl,
    session_id: SessionId,
    delivered_after_commit: bool,
}

impl RunEventSink for ModelCompactionStopAfterCommit {
    fn emit(&mut self, _event: RunEvent) -> Result<(), crate::error::RuntimeError> {
        panic!("the valid checkpoint is published through emit_committed");
    }

    fn emit_committed(&mut self, event: RunEvent) -> Result<(), crate::error::RuntimeError> {
        assert!(matches!(event, RunEvent::CompactionCompleted { .. }));
        self.delivered_after_commit = self
            .store
            .protocol_event_store()
            .list_history_items_for_session(self.session_id)
            .unwrap()
            .iter()
            .any(|item| {
                matches!(
                    &item.payload, HistoryItemPayload::Compaction { summary, .. }
                        if summary == MODEL_COMPACTION_SAVE_SUMMARY
                )
            });
        assert!(
            self.delivered_after_commit,
            "publication follows durable save"
        );
        assert!(
            self.control.interrupt(TurnInterruptionCause::UserStop),
            "Stop can win after the save reservation is released"
        );
        Err(crate::error::RuntimeError::Message(
            "fixture delivery failed".into(),
        ))
    }
}

#[tokio::test]
async fn model_compaction_save_survives_delivery_failure_and_later_stop() {
    let ModelCompactionSaveFixture {
        store,
        agent,
        mut request,
        template,
        source_ids,
    } = model_compaction_save_fixture().await;
    let expected_projection = request.context.model_messages_after_compaction(
        request.session.session.id,
        request.turn_id(),
        request
            .context
            .compaction_user_messages_for_items(&source_ids),
        MODEL_COMPACTION_SAVE_SUMMARY.into(),
        source_ids.clone(),
        &HashSet::new(),
        false,
    );
    let original = store
        .protocol_event_store()
        .list_history_items_for_session(request.session.session.id)
        .unwrap();
    let mut sink = ModelCompactionStopAfterCommit {
        store: store.clone(),
        control: request.run_control.clone(),
        session_id: request.session.session.id,
        delivered_after_commit: false,
    };
    assert!(
        agent
            .apply_model_compaction(
                &mut request,
                &ContextWindowTokenStatus::for_request(&template, 0),
                MODEL_COMPACTION_SAVE_SUMMARY.into(),
                source_ids,
                &mut sink,
            )
            .await
            .expect("publication failure and later Stop cannot revoke the checkpoint")
    );
    assert!(sink.delivered_after_commit);
    assert!(request.run_control.is_cancelled());
    let after = store
        .protocol_event_store()
        .list_history_items_for_session(request.session.session.id)
        .unwrap();
    assert_eq!(after.len(), original.len() + 1);
    assert_eq!(
        serde_json::to_value(&after[..original.len()]).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(
        serde_json::to_value(request.context.model_messages(false)).unwrap(),
        serde_json::to_value(expected_projection).unwrap()
    );
    let reopened = StoreBundle::new(SqliteStore::open(store.paths()).unwrap());
    assert_eq!(
        serde_json::to_value(
            reopened
                .protocol_event_store()
                .list_history_items_for_session(request.session.session.id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[tokio::test]
async fn model_compaction_save_uses_calibrated_full_request_budget() {
    let ModelCompactionSaveFixture {
        store,
        agent,
        mut request,
        template,
        source_ids,
    } = model_compaction_save_fixture().await;
    let before_tokens = estimate_model_messages_tokens(&request.context.model_messages(false));
    let after_tokens = estimate_model_messages_tokens(
        &request.context.model_messages_after_compaction(
            request.session.session.id,
            request.turn_id(),
            request
                .context
                .compaction_user_messages_for_items(&source_ids),
            MODEL_COMPACTION_SAVE_SUMMARY.into(),
            source_ids.clone(),
            &HashSet::new(),
            false,
        ),
    );
    assert!(
        after_tokens < before_tokens,
        "the summary compresses eligible history"
    );
    let working_limit = request.turn.policy.model.working_context_token_limit;
    let local_status = ContextWindowTokenStatus::for_request(&template, 0);
    assert!(
        local_status
            .active_context_tokens
            .saturating_sub(before_tokens)
            .saturating_add(after_tokens)
            < working_limit,
        "the coarse local request estimate would accept this summary"
    );
    let calibrated_status = ContextWindowTokenStatus::from_provider_usage(
        &template,
        0,
        working_limit.saturating_add(before_tokens),
        &[],
        0,
    );
    let before = model_compaction_save_snapshot(&store, &request);
    let mut sink = CapturingSink::default();
    assert!(
        !agent
            .apply_model_compaction(
                &mut request,
                &calibrated_status,
                MODEL_COMPACTION_SAVE_SUMMARY.into(),
                source_ids,
                &mut sink,
            )
            .await
            .expect("a budget rejection lets the current turn continue")
    );
    assert_eq!(
        model_compaction_save_snapshot(&store, &request),
        before,
        "provider-calibrated overhead prevents saving an oversized full request"
    );
    assert!(matches!(
        sink.events.as_slice(),
        [RunEvent::RecoverableRuntimeFeedback { .. }]
    ));
}

#[test]
fn model_compaction_preserves_legacy_snapshot_input_anchors_when_resummarizing() {
    const MULTIPART_INPUT: &str =
        "Original attachment question.\nPreserve the attached image's description.";
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let legacy_id = HistoryItemId::new();
    let legacy = crate::protocol::ClmCheckpoint::from_messages(
        &[
            ModelMessage::User {
                content: "Original user task.".into(),
            },
            ModelMessage::Assistant {
                content: "Protected assistant commentary is not an input anchor.".into(),
            },
            ModelMessage::Agent {
                content: "Original delegated task.".into(),
            },
            ModelMessage::UserParts {
                parts: vec![
                    ModelContentPart::Text {
                        text: "Original attachment question.".into(),
                    },
                    ModelContentPart::Image {
                        mime_type: "image/png".into(),
                        data_base64: "aA==".into(),
                    },
                    ModelContentPart::Text {
                        text: "Preserve the attached image's description.".into(),
                    },
                ],
            },
            ModelMessage::User {
                content: "Unprotected historical input is summarized.".into(),
            },
            ModelMessage::Assistant {
                content: "Historical work evidence is summarized.".into(),
            },
        ],
        4,
    )
    .expect("valid historical snapshot");
    let context = context_manager::ContextManager::from_active_history(
        vec![
            HistoryItem {
                id: legacy_id,
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 0,
                created_at_ms: 0,
                payload: HistoryItemPayload::Compaction {
                    mode: crate::protocol::CompactionMode::Automatic,
                    layout: crate::protocol::CompactionLayout::ClmCheckpoint,
                    preserved_user_messages: Vec::new(),
                    clm_checkpoint: Some(legacy),
                    summary: "Historical display summary.".into(),
                    replacement_item_ids: Vec::new(),
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: 1,
                payload: HistoryItemPayload::UserTurn {
                    content: vec![ContentPart::Text {
                        text: "Current continuation request.".into(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            },
        ],
        Some(2),
        2,
    );
    let anchors = context.compaction_user_messages_for_items(&[legacy_id]);
    assert_eq!(
        anchors,
        [
            "Original user task.",
            "Original delegated task.",
            MULTIPART_INPUT
        ]
    );
    let projected = context.model_messages_after_compaction(
        session_id,
        turn_id,
        anchors,
        "New independent continuation notes.".into(),
        vec![legacy_id],
        &HashSet::new(),
        false,
    );
    for expected in [
        "Original user task.",
        "Original delegated task.",
        MULTIPART_INPUT,
        "Current continuation request.",
    ] {
        assert_eq!(
            projected
                .iter()
                .filter(|message| matches!(
                    message, ModelMessage::User { content } if content == expected
                ))
                .count(),
            1,
            "input {expected} survives exactly once"
        );
    }
    let serialized = serde_json::to_string(&projected).unwrap();
    assert!(serialized.contains("New independent continuation notes."));
    for removed in [
        "Protected assistant commentary",
        "Unprotected historical input",
        "Historical work evidence",
        "Historical display summary",
    ] {
        assert!(
            !serialized.contains(removed),
            "historical non-anchor evidence is replaced"
        );
    }
}
