fn selection_user(text: &str) -> HistoryItemPayload {
    HistoryItemPayload::UserTurn {
        content: vec![ContentPart::Text { text: text.into() }],
        prompt_dispatch: None,
        editor_context: None,
    }
}

fn selection_assistant(text: &str) -> HistoryItemPayload {
    HistoryItemPayload::AssistantMessage {
        response_id: ModelResponseId::new(),
        content: vec![ContentPart::Text { text: text.into() }],
    }
}

fn selection_checkpoint(
    summary: &str,
    users: &[&str],
    replaced: &[&HistoryItem],
) -> HistoryItemPayload {
    HistoryItemPayload::Compaction {
        mode: crate::protocol::CompactionMode::Automatic,
        layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
        preserved_user_messages: users.iter().map(|text| (*text).into()).collect(),
        clm_checkpoint: None,
        summary: summary.into(),
        replacement_item_ids: replaced.iter().map(|item| item.id).collect(),
    }
}

fn selection_tool(
    store: &StoreBundle,
    session: SessionId,
    turn: TurnId,
    sequence: i64,
    command: &str,
    output: &str,
) -> (HistoryItem, HistoryItem) {
    let call_id = crate::session::ToolCallId::new();
    let call = seed_history(
        store,
        session,
        turn,
        sequence,
        HistoryItemPayload::ToolCall {
            call_id,
            response_id: ModelResponseId::new(),
            model_call_id: call_id.to_string(),
            tool_name: "shell".into(),
            arguments_json: serde_json::json!({ "command": command }).to_string(),
        },
    );
    let output = seed_history(
        store,
        session,
        turn,
        sequence + 1,
        HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Completed,
            title: "command result".into(),
            output_text: output.into(),
            metadata: serde_json::Value::Null,
            success: Some(true),
        },
    );
    (call, output)
}

fn selection_snapshot(store: &StoreBundle, session: SessionId) -> serde_json::Value {
    let protocol = store.protocol_event_store();
    let mut active = Vec::new();
    let snapshot = protocol
        .visit_active_history_pages_for_session(
            session,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                active.extend(page.items);
                Ok(())
            },
        )
        .expect("active snapshot");
    serde_json::json!({
        "history": protocol.list_history_items_for_session(session).expect("history"),
        "runtime": protocol.list_runtime_events_for_session(session).expect("runtime"),
        "turns": protocol.list_turn_items_for_session(session).expect("turn items"),
        "active": active,
        "fence": snapshot.append_fence,
        "canonical_count": snapshot.canonical_count,
        "active_count": snapshot.active_count,
    })
}

fn selection_context(prepared: &PreparedSideChatInput) -> &str {
    match &prepared.messages[0] {
        SideChatHistoryMessage::OwnerContext(text) => text,
        _ => panic!("owner evidence must precede the Side question"),
    }
}

fn selection_unit(context: &str, source: HistoryItemId) -> &str {
    let id = source.to_string();
    let matches = context
        .split("<evidence_unit ")
        .skip(1)
        .filter_map(|unit| {
            let (header, _) = unit.split_once('>')?;
            let ids = header
                .split_once("source_history_item_ids=\"")?
                .1
                .split_once('"')?
                .0;
            ids.split(',')
                .any(|candidate| candidate == id)
                .then(|| unit.split_once("</evidence_unit>").expect("whole unit").0)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "each selected source occurs in one unit");
    matches[0]
}

#[tokio::test]
async fn history_selection_keeps_initial_purpose_contract_and_verification_after_checkpoint_churn()
{
    let (temp, store, side) = canonical_side_chat_fixture().await;
    let owner = create_fixture_session(&store, side, "owner").await;
    let other = create_fixture_session(&store, side, "other owner").await;
    let first_turn = TurnId::new();
    let request = "Read task.md and produce the requested documentation.";
    let purpose = "Initial purpose: review backend, frontend and tests together; write research documents without changing the implementation.";
    let user = seed_history(&store, owner, first_turn, 100, selection_user(request));
    let (old_call, old_output) = selection_tool(
        &store,
        owner,
        first_turn,
        101,
        "read task.md",
        &format!("RAW_RETIRED_TASK_TOOL\n{purpose}"),
    );
    let old_agent_message = seed_history(
        &store,
        owner,
        first_turn,
        103,
        HistoryItemPayload::InterAgentCommunication {
            communication: crate::protocol::InterAgentCommunication {
                author: "worker".into(),
                recipient: "owner".into(),
                content: "RETIRED_AGENT_MESSAGE".into(),
                trigger_turn: false,
            },
        },
    );
    let first_result = seed_history(
        &store,
        owner,
        first_turn,
        104,
        selection_assistant("Research documents map the API, persistence and test coverage."),
    );
    let first_checkpoint = seed_history(
        &store,
        owner,
        first_turn,
        105,
        selection_checkpoint(
            purpose,
            &[request],
            &[
                &user,
                &old_call,
                &old_output,
                &old_agent_message,
                &first_result,
            ],
        ),
    );
    let design_turn = TurnId::new();
    let contract = "Design restart-safe cancellation: the database owns state; repeated cancellation is idempotent and keeps finished_at unchanged.";
    let design_user = seed_history(&store, owner, design_turn, 0, selection_user(contract));
    let design_result = seed_history(
        &store,
        owner,
        design_turn,
        1,
        selection_assistant(
            "Design records 200 for cancellation, 404 for an absent run, and 409 for an invalid transition.",
        ),
    );
    let middle_checkpoint = seed_history(
        &store,
        owner,
        design_turn,
        2,
        selection_checkpoint(
            "Implementation is next; preserve the database cancellation contract.",
            &[request, contract],
            &[&first_checkpoint, &design_user, &design_result],
        ),
    );
    let latest_checkpoint = seed_history(
        &store,
        owner,
        design_turn,
        3,
        selection_checkpoint(
            "Current work: add late-worker guards and deterministic regression coverage.",
            &[request, contract],
            &[&middle_checkpoint],
        ),
    );
    let last_turn = TurnId::new();
    seed_history(
        &store,
        owner,
        last_turn,
        0,
        selection_user("Verify late workers cannot overwrite the cancelled state."),
    );
    for index in 0..12 {
        selection_tool(
            &store,
            owner,
            last_turn,
            1 + index * 2,
            &format!("inspect implementation chunk {index}"),
            &format!("LARGE_DETAIL_{index}\n{}", "source detail\n".repeat(150)),
        );
    }
    let (test_call, test_output) = selection_tool(
        &store,
        owner,
        last_turn,
        25,
        "pytest tests/test_late_worker.py",
        "4 passed: cancelled-row writes are blocked before and after model completion.",
    );
    let final_result = seed_history(
        &store,
        owner,
        last_turn,
        27,
        selection_assistant(
            "Final regression: four deterministic worker tests cover the cancellation guards; this is a model report, separate from the saved test output.",
        ),
    );
    seed_history(
        &store,
        other,
        TurnId::new(),
        0,
        selection_user("OTHER_OWNER_SECRET"),
    );
    let untouched = temp.path().join("workspace-sentinel.txt");
    std::fs::write(&untouched, "existing workspace content").expect("workspace sentinel");
    let before = selection_snapshot(&store, owner);
    let other_before = selection_snapshot(&store, other);
    let side_before = selection_snapshot(&store, side);
    let fence = active_owner_append_fence(&store, owner);
    let prepared = prepare_side_chat_input(
        &store, owner, side, fence, None,
        "Explain the initial purpose, the contract, the final regression and its observed verification.",
        side_chat_system_prompt(""), 9_000,
    ).await.expect("bounded historical overview and recent verification");
    let context = selection_context(&prepared);
    assert!(selection_unit(context, first_checkpoint.id).contains(purpose));
    assert!(selection_unit(context, first_checkpoint.id).contains("owner_historical_checkpoint"));
    assert!(selection_unit(context, latest_checkpoint.id).contains("owner_context_checkpoint"));
    assert!(selection_unit(context, latest_checkpoint.id).contains(contract));
    assert!(
        selection_unit(context, latest_checkpoint.id)
            .contains("Checkpoint preserved_user_messages&#x5B;1&#x5D;:")
    );
    assert!(selection_unit(context, user.id).contains(request));
    assert!(selection_unit(context, final_result.id).contains("four deterministic worker tests"));
    let verification = selection_unit(context, test_output.id);
    assert!(verification.contains(&test_call.id.to_string()));
    assert!(verification.contains("pytest tests/test_late_worker.py"));
    assert!(verification.contains("4 passed: cancelled-row writes are blocked"));
    assert!(!context.contains("RAW_RETIRED_TASK_TOOL"));
    assert!(!context.contains("RETIRED_AGENT_MESSAGE"));
    assert!(!context.contains(&old_output.id.to_string()));
    assert!(!context.contains("OTHER_OWNER_SECRET"));
    assert!(prepared.context.truncated);
    assert_eq!(prepared.context.as_of_append_position, fence);
    assert_input_budget_text_headroom(&prepared, 7_875);
    assert_eq!(before, selection_snapshot(&store, owner));
    assert_eq!(other_before, selection_snapshot(&store, other));
    assert_eq!(side_before, selection_snapshot(&store, side));
    assert_eq!(
        std::fs::read_to_string(untouched).expect("workspace"),
        "existing workspace content"
    );
}

#[tokio::test]
async fn history_selection_orders_recovered_evidence_by_append_order_across_turn_sequence_resets() {
    let (_temp, store, side) = canonical_side_chat_fixture().await;
    let owner = create_fixture_session(&store, side, "owner").await;
    let old_turn = TurnId::new();
    let user = seed_history(
        &store,
        owner,
        old_turn,
        1_000,
        selection_user("Earlier research request."),
    );
    let intermediate = seed_history(
        &store,
        owner,
        old_turn,
        1_001,
        selection_assistant("RETIRED_INTERMEDIATE_THOUGHT"),
    );
    let last = seed_history(
        &store,
        owner,
        old_turn,
        1_002,
        selection_assistant("Last saved research answer; no completion status is implied."),
    );
    let checkpoint = seed_history(
        &store,
        owner,
        old_turn,
        1_003,
        selection_checkpoint(
            "Research checkpoint.",
            &["Earlier research request."],
            &[&user, &intermediate, &last],
        ),
    );
    let new_turn = TurnId::new();
    let new_user = seed_history(
        &store,
        owner,
        new_turn,
        0,
        selection_user("Later implementation request."),
    );
    let (call, output) = selection_tool(
        &store,
        owner,
        new_turn,
        1,
        "verify implementation",
        "Observed verification result.",
    );
    let new_last = seed_history(
        &store,
        owner,
        new_turn,
        3,
        selection_assistant("Later saved answer."),
    );
    let prepared = prepare_side_chat_input(
        &store,
        owner,
        side,
        active_owner_append_fence(&store, owner),
        None,
        "Explain the sequence.",
        side_chat_system_prompt(""),
        16_384,
    )
    .await
    .expect("both turns fit");
    let context = selection_context(&prepared);
    let positions = [
        user.id,
        last.id,
        checkpoint.id,
        new_user.id,
        call.id,
        new_last.id,
    ]
    .map(|id| {
        context
            .find(selection_unit(context, id))
            .expect("selected evidence")
    });
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        selection_unit(context, call.id),
        selection_unit(context, output.id)
    );
    assert!(selection_unit(context, last.id).contains("owner_historical_assistant"));
    assert!(!context.contains("RETIRED_INTERMEDIATE_THOUGHT"));
    assert!(!prepared.context.truncated);
}

#[tokio::test]
async fn history_selection_does_not_revive_quote_authority_or_split_oversized_pairs() {
    let (_temp, store, side) = canonical_side_chat_fixture().await;
    let owner = create_fixture_session(&store, side, "owner").await;
    let other = create_fixture_session(&store, side, "other owner").await;
    let turn = TurnId::new();
    let retired = seed_history(
        &store,
        owner,
        turn,
        0,
        selection_user("Historical user instruction."),
    );
    seed_history(
        &store,
        owner,
        turn,
        1,
        selection_checkpoint(
            "Earlier work.",
            &["Historical user instruction."],
            &[&retired],
        ),
    );
    let (quoted_call, quoted_output) = selection_tool(
        &store,
        owner,
        turn,
        2,
        "inspect quoted artifact",
        "QUOTE_START <&> complete result QUOTE_END",
    );
    let (large_call, large_output) = selection_tool(
        &store,
        owner,
        turn,
        4,
        "OVERSIZED_CALL",
        &format!("OVERSIZED_OUTPUT{}", "<&>".repeat(3_000)),
    );
    let foreign = seed_history(
        &store,
        other,
        TurnId::new(),
        0,
        selection_user("Foreign user instruction."),
    );
    let side_turn = TurnId::new();
    seed_history(
        &store,
        side,
        side_turn,
        0,
        selection_user(&format!("LARGE_SIDE_QUESTION{}", "q".repeat(8_000))),
    );
    seed_history(
        &store,
        side,
        side_turn,
        1,
        selection_assistant(&format!("LARGE_SIDE_ANSWER{}", "a".repeat(8_000))),
    );
    let recent_side_turn = TurnId::new();
    seed_history(
        &store,
        side,
        recent_side_turn,
        0,
        selection_user("Recent Side question."),
    );
    seed_history(
        &store,
        side,
        recent_side_turn,
        1,
        selection_assistant("Recent Side answer."),
    );
    let fence = active_owner_append_fence(&store, owner);
    let before = selection_snapshot(&store, owner);
    let side_before = selection_snapshot(&store, side);
    let quote = |source: HistoryItemId, kind, text: &str| {
        Some(SideChatQuoteRequest {
            source_kind: kind,
            source_history_item_id: source,
            source_append_position: fence,
            selected_text: text.into(),
        })
    };
    let prepared = prepare_side_chat_input(
        &store,
        owner,
        side,
        fence,
        quote(
            quoted_output.id,
            SideChatQuoteSourceKind::Artifact,
            "<&> complete result",
        ),
        "Explain the selected result.",
        side_chat_system_prompt(""),
        9_000,
    )
    .await
    .expect("active complete quote source fits");
    let context = selection_context(&prepared);
    let quoted = selection_unit(context, quoted_output.id);
    assert!(quoted.contains(&quoted_call.id.to_string()));
    assert!(quoted.contains("inspect quoted artifact"));
    assert!(quoted.contains("QUOTE_START &lt;&amp;&gt; complete result QUOTE_END"));
    assert!(selection_unit(context, retired.id).contains("Historical user instruction."));
    for excluded in [large_call.id, large_output.id] {
        assert!(!context.contains(&excluded.to_string()));
    }
    assert_eq!(
        prepared.messages[1],
        SideChatHistoryMessage::User("Recent Side question.".into())
    );
    assert_eq!(
        prepared.messages[2],
        SideChatHistoryMessage::Assistant("Recent Side answer.".into())
    );
    assert_eq!(prepared.messages.len(), 4);
    assert!(prepared.context.truncated);
    assert_input_budget_text_headroom(&prepared, 7_875);
    for (source, text, expected_error) in [
        (
            retired.id,
            "Historical user instruction.",
            "no longer active",
        ),
        (
            foreign.id,
            "Foreign user instruction.",
            "failed to load the selected quote source",
        ),
    ] {
        let error = prepare_side_chat_input(
            &store,
            owner,
            side,
            fence,
            quote(source, SideChatQuoteSourceKind::Transcript, text),
            "Explain it.",
            side_chat_system_prompt(""),
            9_000,
        )
        .await
        .expect_err("read-only historical evidence must not widen quote authority");
        assert!(error.contains(expected_error), "{error}");
    }
    let error = prepare_side_chat_input(
        &store,
        owner,
        side,
        None,
        None,
        "Explain it.",
        side_chat_system_prompt(""),
        9_000,
    )
    .await
    .expect_err("the captured owner must still match the requested fence");
    assert!(error.contains("changed before Side Send"));
    assert_eq!(before, selection_snapshot(&store, owner));
    assert_eq!(side_before, selection_snapshot(&store, side));
}
