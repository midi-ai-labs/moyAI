use super::*;

impl AgentLoop {
    /// The model supplies the continuation note in its ordinary tool response.
    /// The host selects complete source units before sampling and commits only
    /// after the current batch has all of its matching tool outputs.
    pub(super) async fn apply_model_compaction(
        &self,
        request: &mut AgentRunRequest,
        current_status: &ContextWindowTokenStatus,
        summary: String,
        source_item_ids: Vec<HistoryItemId>,
        sink: &mut dyn RunEventSink,
    ) -> Result<bool, AgentError> {
        let active_ids = request
            .context
            .active_item_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        if source_item_ids.is_empty() || source_item_ids.iter().any(|id| !active_ids.contains(id)) {
            return self.reject_model_compaction(request, sink,
                "Context compaction was not applied: there was no eligible completed history in the sampled context.");
        }
        let supports_images = request
            .turn
            .policy
            .model
            .input_modalities
            .contains(&crate::llm::model_policy::InputModality::Image);
        // Preserve any input anchors carried by an earlier semantic checkpoint.
        // Explicit user/steer input remains outside the replacement selection.
        let preserved_user_messages = request
            .context
            .compaction_user_messages_for_items(&source_item_ids);
        let before =
            estimate_model_messages_tokens(&request.context.model_messages(supports_images));
        let after =
            estimate_model_messages_tokens(&request.context.model_messages_after_compaction(
                request.session.session.id,
                request.turn_id(),
                preserved_user_messages.clone(),
                summary.clone(),
                source_item_ids.clone(),
                &HashSet::new(),
                supports_images,
            ));
        let before_request = current_status.active_context_tokens;
        let after_request = before_request.saturating_sub(before).saturating_add(after);
        if after >= before || after_request >= request.turn.policy.model.working_context_token_limit
        {
            return self.reject_model_compaction(request, sink,
                "Context compaction was not applied: the proposed notes did not reduce the input below its compaction threshold. Original context is retained.");
        }
        let commit = request.run_control.begin_tool_settlement().ok_or_else(|| {
            AgentError::Message("context compaction cancelled before commit".into())
        })?;
        ensure_admission_active(&self.store, request).await?;
        let event = RunEvent::CompactionCompleted {
            layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
            clm_checkpoint: None,
            summarized_messages: source_item_ids.len(),
            preserved_user_messages,
            summary,
            replacement_item_ids: source_item_ids,
        };
        self.store
            .session_repo()
            .commit_admitted_compaction_with_protocol_bundle(
                request.session.session.id,
                request.admission_id(),
                &event,
                request.turn_id(),
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        drop(commit);
        let _ = sink.emit_committed(event);
        refresh_committed_context_page(
            &self.store,
            request.session.session.id,
            &mut request.context,
        )?;
        Ok(true)
    }

    fn reject_model_compaction(
        &self,
        request: &AgentRunRequest,
        sink: &mut dyn RunEventSink,
        message: &str,
    ) -> Result<bool, AgentError> {
        sink.emit(RunEvent::RecoverableRuntimeFeedback {
            session_id: request.session.session.id,
            feedback: crate::session::DurableRuntimeFeedback::new(
                crate::session::DurableFeedbackSeverity::Warning,
                crate::session::DurableFeedbackCategory::Context,
                message,
            ),
        })?;
        Ok(false)
    }
}
