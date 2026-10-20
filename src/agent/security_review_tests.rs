struct ReviewEffectFixture;

#[async_trait(?Send)]
impl crate::tool::registry::Tool for ReviewEffectFixture {
    fn spec(&self) -> crate::tool::ToolSpec {
        crate::tool::ToolSpec {
            name: ToolName::Write,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "simulate an external effect without a file-change sidecar",
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn execute(
        &self,
        _arguments: Value,
        _ctx: crate::tool::context::ToolContext<'_>,
    ) -> Result<ToolResult, crate::error::ToolError> {
        Ok(ToolResult {
            title: "External effect fixture".into(),
            output_text: "A potential external change was simulated; security is unreviewed.".into(),
            metadata: serde_json::json!({"success": true}),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

fn review_fixture_final() -> ScriptedResponse {
    ScriptedResponse {
        events: vec![LlmEvent::TextDelta("Requested work is finished.".into())],
        finish_reason: FinishReason::Stop,
    }
}

#[tokio::test]
async fn security_review_unreviewed_effect_emits_durable_warning_before_completion() {
    let run = run_scripted_with_control_tool_and_protocol_recording(
        ResolvedConfig::default(),
        vec![scripted_write_call("effect"), review_fixture_final()],
        RunControl::new(),
        Arc::new(ReviewEffectFixture),
    )
    .await
    .expect("fixture execution");
    assert_eq!(run.summary.expect("completion").status(), SessionStatus::Completed);
    let warning = run.events.iter().position(|event| matches!(
        event,
        RunEvent::RecoverableRuntimeFeedback { .. }
    )).expect("unreviewed effect is reported");
    let terminal = run.events.iter().position(|event| matches!(event, RunEvent::TurnTerminal { .. }))
        .expect("terminal");
    assert!(warning < terminal);
    let items = run.store.protocol_event_store().list_history_items_for_session(run.session_id)
        .expect("canonical history");
    assert!(items.iter().any(|item| matches!(item.payload, HistoryItemPayload::DurableFeedback { .. })));
}

fn review_fixture_call(id: &str, name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse {
        events: vec![
            LlmEvent::ToolCallStart { call_id: id.into(), tool_name: name.into() },
            LlmEvent::ToolCallArgsDelta { call_id: id.into(), delta: args.to_string() },
        ],
        finish_reason: FinishReason::ToolCall,
    }
}

#[tokio::test]
async fn security_review_records_current_evidence_and_detects_a_later_effect() {
    for later_effect in [false, true] {
        let mut responses = vec![scripted_write_call("effect")];
        responses.push(review_fixture_call("review", "security_review", serde_json::json!({
            "summary": "Only the simulated external effect was checked; adequacy is not certified.",
            "findings": [], "limitations": [], "check_tool_call_ids": ["effect"]
        })));
        if later_effect { responses.push(scripted_write_call("later_effect")); }
        responses.push(review_fixture_final());
        let run = run_scripted_with_control_tool_and_protocol_recording(
            ResolvedConfig::default(), responses, RunControl::new(), Arc::new(ReviewEffectFixture)
        ).await.expect("fixture execution");
        assert_eq!(run.summary.expect("completion").status(), SessionStatus::Completed);
        let warning_count = run.events.iter().filter(|event| matches!(
            event, RunEvent::RecoverableRuntimeFeedback { .. }
        )).count();
        assert_eq!(warning_count, usize::from(later_effect));
        assert!(run.events.iter().any(|event| matches!(
            event, RunEvent::ToolCallCompleted { tool: ToolName::SecurityReview, .. }
        )));
    }
}

#[tokio::test]
async fn security_review_does_not_add_a_round_or_warning_to_read_only_work() {
    let run = run_scripted_with_control_and_tool(
        ResolvedConfig::default(),
        vec![scripted_read_call("read"), review_fixture_final()],
        RunControl::new(), Arc::new(LargeReadOutputTool { output_chars: 4 })
    ).await.expect("fixture execution");
    assert_eq!(run.summary.expect("completion").status(), SessionStatus::Completed);
    assert_eq!(run.requests.len(), 2);
    assert!(!run.events.iter().any(|event| matches!(event, RunEvent::RecoverableRuntimeFeedback { .. })));
}
