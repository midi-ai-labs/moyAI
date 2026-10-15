fn input_budget_owner_unit(
    sequence_no: i64,
    body: String,
    source_count: usize,
) -> OwnerContextUnit {
    let source_ids = (0..source_count)
        .map(|_| HistoryItemId::new())
        .collect::<Vec<_>>();
    OwnerContextUnit {
        key: OwnerContextUnitKey::Tool(crate::session::ToolCallId::new()),
        source_order: usize::try_from(sequence_no).expect("fixture order"),
        source_ids,
        kind: "owner_tool",
        body,
    }
}

fn input_budget_owner_text(prepared: &PreparedSideChatInput) -> &str {
    let SideChatHistoryMessage::OwnerContext(text) = &prepared.messages[0] else {
        panic!("the prepared input must start with the owner evidence");
    };
    text
}

fn assert_input_budget_text_headroom(prepared: &PreparedSideChatInput, text_limit: usize) {
    let message_bytes = prepared
        .messages
        .iter()
        .map(|message| match message {
            SideChatHistoryMessage::OwnerContext(text)
            | SideChatHistoryMessage::User(text)
            | SideChatHistoryMessage::Assistant(text) => text.len(),
        })
        .sum::<usize>();
    assert!(prepared.system_prompt.len() + message_bytes <= text_limit);
}

#[test]
fn input_budget_selects_complete_dense_owner_units_before_the_context_fills() {
    for (raw, encoded, repeats) in [
        (
            "if (ready) { return 1; }\n",
            "if (ready) { return 1; }\n",
            70,
        ),
        ("設計と実装の結果。\n", "設計と実装の結果。\n", 60),
        (
            "&<>\"'[]\0",
            "&amp;&lt;&gt;&quot;&apos;&#x5B;&#x5D;&#x0;",
            40,
        ),
    ] {
        let units = (0..3)
            .map(|index| {
                input_budget_owner_unit(
                    index,
                    format!(
                        "CALL_START_{index}\n{}\nOUTPUT_END_{index}",
                        raw.repeat(repeats)
                    ),
                    2,
                )
            })
            .collect();
        let prepared = build_prepared_side_chat_input(
            SessionId::new(),
            Some(77),
            units,
            3,
            false,
            Vec::new(),
            None,
            "Explain the completed work.",
            side_chat_system_prompt(""),
            8_192,
        )
        .expect("dense history should be reduced rather than reject the question");

        assert!(prepared.context.truncated);
        assert!(prepared.context.included_owner_unit_count > 0);
        assert!(prepared.context.included_owner_unit_count < 3);
        let owner = input_budget_owner_text(&prepared);
        for index in 0..3 {
            let has_call = owner.contains(&format!("CALL_START_{index}"));
            let has_output = owner.contains(&format!("OUTPUT_END_{index}"));
            assert_eq!(has_call, has_output, "a tool unit must never be split");
            if has_call {
                assert!(owner.contains(&format!(
                    "CALL_START_{index}\n{}\nOUTPUT_END_{index}",
                    encoded.repeat(repeats)
                )));
            }
        }
        assert_input_budget_text_headroom(&prepared, 7_168);
        assert!(
            matches!(prepared.messages.last(), Some(SideChatHistoryMessage::User(text)) if text == "Explain the completed work.")
        );
    }
}

#[test]
fn input_budget_accounts_for_many_provenance_ids_and_short_evidence_tags() {
    for (source_count, unit_count) in [(1, 80), (96, 2)] {
        let units = (0..unit_count)
            .map(|index| {
                input_budget_owner_unit(index, format!("PROVENANCE_UNIT_{index}"), source_count)
            })
            .collect::<Vec<_>>();
        let expected = units
            .iter()
            .map(|unit| {
                (
                    unit.body.clone(),
                    unit.source_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                )
            })
            .collect::<Vec<_>>();
        let prepared = build_prepared_side_chat_input(
            SessionId::new(),
            Some(99),
            units,
            usize::try_from(unit_count).expect("small fixture"),
            false,
            Vec::new(),
            None,
            "Explain the provenance.",
            side_chat_system_prompt(""),
            8_192,
        )
        .expect("metadata-heavy history should still yield bounded evidence");
        assert!(prepared.context.truncated);
        assert!(prepared.context.included_owner_unit_count > 0);
        assert!(prepared.context.included_owner_unit_count < expected.len());
        let owner = input_budget_owner_text(&prepared);
        let mut observed_units = 0;
        for (body, source_ids) in expected {
            if owner.contains(&format!("\n{body}\n</evidence_unit>")) {
                observed_units += 1;
                assert!(owner.contains(&format!("source_history_item_ids=\"{source_ids}\"")));
            }
        }
        assert_eq!(observed_units, prepared.context.included_owner_unit_count);
        assert_input_budget_text_headroom(&prepared, 7_168);
    }
}

#[test]
fn input_budget_keeps_the_quote_source_and_rejects_oversized_fixed_input() {
    let owner_session_id = SessionId::new();
    let selected = "<&>".repeat(60);
    let expected_encoded = "&lt;&amp;&gt;".repeat(60);
    let protected = input_budget_owner_unit(
        0,
        format!("PROTECTED_CALL &\n{selected}\nPROTECTED_OUTPUT"),
        2,
    );
    let quote = SideChatQuoteRequest {
        source_kind: SideChatQuoteSourceKind::Artifact,
        source_history_item_id: protected.source_ids[1],
        source_append_position: Some(i64::MAX),
        selected_text: selected,
    };
    let system = side_chat_system_prompt(&"Use grounded evidence. ".repeat(20));
    let question = "説明してください。".repeat(20);
    let prepared = build_prepared_side_chat_input(
        owner_session_id,
        Some(i64::MAX),
        vec![
            protected.clone(),
            input_budget_owner_unit(1, "UNSELECTED_NEW_TOOL".repeat(350), 2),
        ],
        2,
        false,
        Vec::new(),
        Some(quote.clone()),
        &question,
        system.clone(),
        8_192,
    )
    .expect("the quoted older source should take priority over new evidence");
    let owner = input_budget_owner_text(&prepared);
    assert!(owner.contains(&format!(
        "PROTECTED_CALL &amp;\n{expected_encoded}\nPROTECTED_OUTPUT"
    )));
    assert!(owner.contains(&format!("\n{expected_encoded}\n</selected_quote>")));
    assert!(owner.contains(&format!(
        "source_history_item_id=\"{}\" source_append_position=\"{}\"",
        quote.source_history_item_id,
        i64::MAX
    )));
    assert!(!owner.contains("UNSELECTED_NEW_TOOL"));
    assert_eq!(prepared.context.included_owner_unit_count, 1);
    assert_eq!(
        prepared.context.quote_source_history_item_id,
        Some(quote.source_history_item_id)
    );
    assert!(prepared.context.truncated);
    assert_input_budget_text_headroom(&prepared, 7_168);

    let error = build_prepared_side_chat_input(
        owner_session_id,
        Some(i64::MAX),
        vec![protected],
        1,
        false,
        Vec::new(),
        Some(quote),
        &"質問".repeat(600),
        system,
        4_096,
    )
    .expect_err("system, question and quote framing must fit before selecting owner history");
    assert!(error.contains("question does not fit"));
}

#[test]
fn input_budget_never_retains_half_of_a_prior_side_exchange() {
    let prior = vec![
        SideChatHistoryMessage::User(format!("OLD_USER_{}", "u".repeat(700))),
        SideChatHistoryMessage::Assistant(format!("OLD_ASSISTANT_{}", "a".repeat(700))),
        SideChatHistoryMessage::User(format!("RECENT_USER_{}", "u".repeat(150))),
        SideChatHistoryMessage::Assistant(format!("RECENT_ASSISTANT_{}", "a".repeat(150))),
    ];
    let recent_user = prior[2].clone();
    let recent_assistant = prior[3].clone();
    let prepared = build_prepared_side_chat_input(
        SessionId::new(),
        Some(7),
        vec![input_budget_owner_unit(0, "OWNER_EVIDENCE".repeat(15), 2)],
        1,
        false,
        prior,
        None,
        "CURRENT_QUESTION",
        side_chat_system_prompt(""),
        4_096,
    )
    .expect("an older pair can be dropped while the recent exchange remains complete");
    assert!(prepared.context.truncated);
    assert_eq!(prepared.messages.len(), 4);
    assert_eq!(prepared.messages[1], recent_user);
    assert_eq!(prepared.messages[2], recent_assistant);
    assert_eq!(
        prepared.messages[3],
        SideChatHistoryMessage::User("CURRENT_QUESTION".to_string())
    );
    assert!(input_budget_owner_text(&prepared).contains("OWNER_EVIDENCE"));
    assert_input_budget_text_headroom(&prepared, 3_584);
}
