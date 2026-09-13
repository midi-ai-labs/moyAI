use super::*;
use crate::agent::shared::{
    SharedCheckpoint, SharedCheckpointSettlement, SharedRunContext, SharedYield,
    SharedYieldProposal,
};

fn invalid(message: &str) -> StorageError {
    StorageError::Message(message.into())
}

/// Hash actual canonical bytes and their append order, not an independently retained digest.
pub(super) fn history_digest(
    connection: &Connection,
    session: SessionId,
    version: u32,
) -> Result<String, StorageError> {
    let mut digest = Sha256::new();
    digest.update(if version == 1 {
        b"moyai.shared-canonical-history.v1\0"
    } else {
        b"moyai.shared-canonical-history.v2\0"
    });
    let mut statement = connection.prepare(
        "SELECT h.id, h.scope_kind, COALESCE(h.turn_id, ''), h.sequence_no,
                h.payload_json, a.append_position
         FROM protocol_history_items h
         JOIN protocol_item_append_order a ON a.source_kind = 'history_item' AND a.source_id = h.id
         WHERE h.session_id = ?1 ORDER BY a.append_position",
    )?;
    let mut rows = statement.query(params![session.to_string()])?;
    let mut total = 0usize;
    let mut position = 0i64;
    while let Some(row) = rows.next()? {
        position += 1;
        for index in [0, 1, 2, 4] {
            let value: String = row.get(index)?;
            total = total.saturating_add(value.len());
            if total > 64 * 1024 * 1024 {
                return Err(invalid(
                    "canonical history exceeds the shared checkpoint verification bound",
                ));
            }
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        digest.update(row.get::<_, i64>(3)?.to_le_bytes());
        digest.update(
            if version == 1 {
                row.get::<_, i64>(5)?
            } else {
                position
            }
            .to_le_bytes(),
        );
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn validate_paused(
    connection: &Connection,
    context: &SharedRunContext,
    checkpoint: &SharedCheckpoint,
) -> Result<AdmissionId, StorageError> {
    let row = connection.query_row(
        "SELECT project_id, environment_id, session_id, turn_id, admission_id, state, checkpoint_json
         FROM shared_run_checkpoints WHERE job_id = ?1",
        params![context.job_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, Option<String>>(6)?)),
    ).optional()?.ok_or_else(|| invalid("shared checkpoint local record is missing; automatic replay is forbidden"))?;
    if row.0 != context.project_id
        || row.1 != context.environment_id
        || row.2 != checkpoint.session_id.to_string()
        || row.3 != checkpoint.turn_id.to_string()
        || row.5 != "paused"
        || row
            .6
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?
            != context
                .resume
                .as_ref()
                .map(|resume| resume.checkpoint.clone())
    {
        return Err(invalid(
            "shared checkpoint does not match the saved paused execution",
        ));
    }
    let admission: AdmissionId = row
        .4
        .parse()
        .map_err(|_| invalid("invalid saved shared admission"))?;
    let state = session_runtime_state_from_connection(connection, checkpoint.session_id)?
        .ok_or_else(|| invalid("shared checkpoint session is missing"))?;
    if state.status != SessionStatus::Running
        || state
            .admission
            .is_none_or(|a| a.admission_id != admission || a.turn_id != checkpoint.turn_id)
        || terminal_for_turn_in_connection(connection, checkpoint.session_id, checkpoint.turn_id)?
            .is_some()
        || first_applicable_tree_stop_fence_for_turn_in_connection(
            connection,
            checkpoint.session_id,
            checkpoint.turn_id,
        )?
        .is_some()
        || connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM exact_execution_interrupt_requests WHERE session_id = ?1)",
            params![checkpoint.session_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(invalid(
            "shared checkpoint was stopped, settled, or replaced",
        ));
    }
    if history_digest(connection, checkpoint.session_id, checkpoint.version)?
        != checkpoint.history_digest
    {
        return Err(invalid(
            "canonical history changed after the shared checkpoint; automatic replay is forbidden",
        ));
    }
    Ok(admission)
}

/// Unlike resume, settlement accepts an already requested Stop. It never acquires execution
/// authority, and checks the saved checkpoint again inside the canonical terminal transaction.
pub(super) fn pending_checkpoint_admission(
    connection: &Connection,
    checkpoint: &SharedCheckpoint,
) -> Result<Option<DurableRunAdmission>, StorageError> {
    let row = connection.query_row(
        "SELECT project_id, environment_id, session_id, turn_id, admission_id, state, checkpoint_json
         FROM shared_run_checkpoints WHERE job_id = ?1",
        params![checkpoint.job_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, Option<String>>(6)?)),
    ).optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.0 != checkpoint.project_id
        || row.1 != checkpoint.environment_id
        || row.2 != checkpoint.session_id.to_string()
        || row.3 != checkpoint.turn_id.to_string()
    {
        return Err(invalid(
            "shared terminal receipt belongs to a different local execution",
        ));
    }
    if row.5 == "active" {
        return Ok(None);
    }
    let saved: SharedCheckpoint = serde_json::from_str(
        row.6
            .as_deref()
            .ok_or_else(|| invalid("paused checkpoint is missing"))?,
    )?;
    if saved.checkpoint_id != checkpoint.checkpoint_id {
        return Ok(None);
    }
    if serde_json::to_value(&saved)? != serde_json::to_value(checkpoint)? {
        return Err(invalid(
            "shared terminal receipt differs from the saved checkpoint",
        ));
    }
    let state = session_runtime_state_from_connection(connection, checkpoint.session_id)?
        .ok_or_else(|| invalid("shared checkpoint session is missing"))?;
    if state.status != SessionStatus::Running {
        return Ok(None);
    }
    let admission = state
        .admission
        .filter(|admission| {
            admission.admission_id.to_string() == row.4 && admission.turn_id == checkpoint.turn_id
        })
        .ok_or_else(|| invalid("shared terminal receipt lost its exact admission"))?;
    if terminal_for_turn_in_connection(connection, checkpoint.session_id, checkpoint.turn_id)?
        .is_some()
        || history_digest(connection, checkpoint.session_id, checkpoint.version)?
            != checkpoint.history_digest
    {
        return Err(invalid(
            "paused canonical history changed before shared terminal settlement",
        ));
    }
    Ok(Some(admission))
}

impl SqliteSessionRepository {
    /// The Runner calls this only with the job returned by its authenticated Hub attempt read.
    /// Hub transport owns device/attempt authentication; this owner fences the exact local turn.
    pub(crate) fn settle_shared_checkpoint_terminal(
        &self,
        checkpoint: &serde_json::Value,
        hub_job: &serde_json::Value,
    ) -> Result<SharedCheckpointSettlement, StorageError> {
        let checkpoint: SharedCheckpoint = serde_json::from_value(checkpoint.clone())?;
        if !matches!(checkpoint.version, 1 | 2)
            || checkpoint.checkpoint_id.is_empty()
            || [
                &checkpoint.job_id,
                &checkpoint.project_id,
                &checkpoint.environment_id,
            ]
            .into_iter()
            .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || hub_job["id"].as_str() != Some(checkpoint.job_id.as_str())
            || hub_job["project_id"].as_str() != Some(checkpoint.project_id.as_str())
            || hub_job["environment_id"].as_str() != Some(checkpoint.environment_id.as_str())
        {
            return Err(invalid(
                "Hub terminal does not identify the checkpoint's shared job",
            ));
        }
        {
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            if pending_checkpoint_admission(&connection, &checkpoint)?.is_none() {
                return Ok(SharedCheckpointSettlement::NoLongerPaused);
            }
        }
        let outcome = match hub_job["state"].as_str() {
            Some("cancelled") => TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            Some("failed") => TurnTerminalOutcome::Failed {
                error: "Hub shared job failed while this turn was waiting for a child".into(),
            },
            Some("succeeded") => {
                return Err(invalid(
                    "Hub reports success while the exact local checkpoint is still paused",
                ));
            }
            Some("queued" | "assigned" | "running" | "waiting_child" | "cancelling") => {
                return Ok(SharedCheckpointSettlement::Pending);
            }
            _ => return Err(invalid("invalid Hub shared job state")),
        };
        // A rejected later Yield leaves the previous accepted checkpoint on the Hub job.
        // Terminal delivery therefore uses the exact local receipt and same-job identity;
        // resumption still requires the saved checkpoint and child-result contract.
        let failed = matches!(outcome, TurnTerminalOutcome::Failed { .. });
        let mut failed_by_name = checkpoint.progress.failed_tool_calls_by_name.clone();
        if failed {
            *failed_by_name.entry("shared_delegate".into()).or_default() += 1;
        }
        let event = RunEvent::TurnTerminal {
            session_id: checkpoint.session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome,
                final_response_id: None,
                tool_call_count: checkpoint.progress.tool_call_count,
                failed_tool_count: checkpoint
                    .progress
                    .failed_tool_count
                    .saturating_add(usize::from(failed)),
                change_count: checkpoint.progress.change_count,
                metrics: crate::session::RunMetrics {
                    model_request_count: checkpoint.progress.model_request_count,
                    token_usage: checkpoint.progress.latest_usage.clone(),
                    tool_calls_by_name: checkpoint.progress.tool_calls_by_name.clone(),
                    failed_tool_calls_by_name: failed_by_name,
                    ..Default::default()
                },
            }),
        };
        match self.terminalize_turn_with_protocol_event_guarded(
            checkpoint.session_id,
            &event,
            TerminalOwnerGuard::SharedCheckpoint(&checkpoint),
            None,
            false,
            false,
            false,
            None,
        )? {
            GuardedTerminalization::Settled { .. } => Ok(SharedCheckpointSettlement::Applied),
            GuardedTerminalization::NotOwned => {
                let connection = self.connection.lock().expect("sqlite mutex poisoned");
                if pending_checkpoint_admission(&connection, &checkpoint)?.is_none() {
                    Ok(SharedCheckpointSettlement::NoLongerPaused)
                } else {
                    Err(invalid("the exact paused shared turn was not terminalized"))
                }
            }
            _ => Err(invalid("shared checkpoint terminal settlement is blocked")),
        }
    }

    pub(crate) fn validate_shared_resume(
        &self,
        context: &SharedRunContext,
    ) -> Result<SharedCheckpoint, StorageError> {
        let checkpoint = context
            .checkpoint()
            .map_err(StorageError::Message)?
            .ok_or_else(|| invalid("shared resume omitted its checkpoint"))?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        validate_paused(&connection, context, &checkpoint)?;
        Ok(checkpoint)
    }

    pub(crate) fn require_active_shared_tool(
        &self,
        job_id: &str,
        session: SessionId,
        turn: TurnId,
    ) -> Result<(), StorageError> {
        self.with_active_shared_tool(job_id, session, turn, |_| Ok(()))
    }

    pub(crate) fn require_shared_artifact_capacity(
        &self,
        job_id: &str,
        session: SessionId,
        turn: TurnId,
        paths: &crate::storage::StoragePaths,
        additional_bytes: usize,
    ) -> Result<(), StorageError> {
        self.with_active_shared_tool(job_id, session, turn, |db| {
            super::shared_archive::require_sidecar_capacity(
                db,
                &session.to_string(),
                paths,
                additional_bytes,
            )
        })
    }

    fn with_active_shared_tool(
        &self,
        job_id: &str,
        session: SessionId,
        turn: TurnId,
        check: impl FnOnce(&Transaction<'_>) -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        let admission: Option<String> = transaction.query_row(
            "SELECT admission_id FROM shared_run_checkpoints WHERE job_id = ?1 AND session_id = ?2 AND turn_id = ?3 AND state = 'active'",
            params![job_id, session.to_string(), turn.to_string()],
            |row| row.get(0),
        ).optional()?;
        let admission = admission.ok_or_else(|| {
            invalid("artifact publication requires the exact active shared execution")
        })?;
        let admission = admission
            .parse::<AdmissionId>()
            .map_err(|_| invalid("invalid shared admission identity"))?;
        require_active_admission_in_transaction(&transaction, session, admission, turn)?;
        check(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn bind_shared_run(
        &self,
        context: &SharedRunContext,
        session: SessionId,
        turn: TurnId,
        admission: AdmissionId,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session, admission, turn)?;
        transaction.execute(
            "INSERT INTO shared_run_checkpoints(job_id, project_id, environment_id, session_id, turn_id, admission_id, state)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'active')",
            params![context.job_id, context.project_id, context.environment_id, session.to_string(), turn.to_string(), admission.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Called after the model worker and heartbeat stop, while the process lease remains held.
    pub(crate) fn checkpoint_shared_run(
        &self,
        context: &SharedRunContext,
        session: SessionId,
        turn: TurnId,
        admission: AdmissionId,
        proposal: SharedYieldProposal,
    ) -> Result<SharedYield, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(&transaction, session, admission, turn)?;
        if first_applicable_tree_stop_fence_for_turn_in_connection(&transaction, session, turn)?.is_some()
            || transaction.query_row("SELECT EXISTS(SELECT 1 FROM exact_execution_interrupt_requests WHERE session_id = ?1)", params![session.to_string()], |row| row.get::<_, bool>(0))?
            || count_unfinished_tool_calls_for_turn_in_transaction(&transaction, session, turn)? != 1
        {
            return Err(invalid("shared yield requires one pending child tool and an unstopped owner"));
        }
        validate_canonical_tool_call_in_transaction(
            &transaction,
            session,
            turn,
            proposal.tool_call_id,
            crate::tool::ToolName::SharedDelegate,
        )?;
        let checkpoint = SharedCheckpoint {
            version: 2,
            checkpoint_id: ulid::Ulid::new().to_string(),
            job_id: context.job_id.clone(),
            project_id: context.project_id.clone(),
            environment_id: context.environment_id.clone(),
            session_id: session,
            turn_id: turn,
            tool_call_id: proposal.tool_call_id,
            history_digest: history_digest(&transaction, session, 2)?,
            child: proposal.child.clone(),
            progress: proposal.progress,
        };
        let value = serde_json::to_value(&checkpoint)?;
        let changed = transaction.execute(
            "UPDATE shared_run_checkpoints SET state = 'paused', checkpoint_json = ?1
             WHERE job_id = ?2 AND session_id = ?3 AND turn_id = ?4 AND admission_id = ?5 AND state = 'active'",
            params![value.to_string(), context.job_id, session.to_string(), turn.to_string(), admission.to_string()],
        )?;
        if changed != 1 {
            return Err(invalid("shared yield lost its exact execution binding"));
        }
        // Expire the old worker's authority without inventing a TurnTerminal. No future is retained.
        transaction.execute(
            "UPDATE sessions SET active_run_lease_expires_at_ms = 1 WHERE id = ?1",
            params![session.to_string()],
        )?;
        transaction.commit()?;
        Ok(SharedYield {
            checkpoint: value,
            child: proposal.child,
            session_id: session,
            turn_id: turn,
        })
    }

    /// A result and the fresh execution fence become visible in one transaction. Replays cannot
    /// re-enter an active/terminal turn, nor append the same child's ToolResult a second time.
    pub(crate) fn resume_shared_run(
        &self,
        context: &SharedRunContext,
    ) -> Result<AdmittedTurnSnapshot, StorageError> {
        let checkpoint = context
            .checkpoint()
            .map_err(StorageError::Message)?
            .ok_or_else(|| invalid("shared resume omitted checkpoint"))?;
        let resume = context
            .resume
            .as_ref()
            .ok_or_else(|| invalid("shared resume omitted child result"))?;
        let child = &resume.child_result;
        let child_id = child
            .get("id")
            .and_then(|value| value.as_str())
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or_else(|| invalid("child result has no valid identity"))?;
        let state = child.get("state").and_then(|value| value.as_str());
        if child.get("parent_id").and_then(|value| value.as_str()) != Some(context.job_id.as_str())
            || child.get("project_id").and_then(|value| value.as_str())
                != Some(context.project_id.as_str())
            || child.get("environment_id").and_then(|value| value.as_str())
                != Some(checkpoint.child.environment_id.as_str())
            || child.get("input") != Some(&checkpoint.child.input)
            || !matches!(state, Some("succeeded" | "failed" | "cancelled"))
        {
            return Err(invalid(
                "child result is not the terminal child requested by this checkpoint",
            ));
        }
        let output =
            serde_json::json!({"job_id":child_id,"state":state,"result":child.get("result")});
        let output_text = serde_json::to_string(&output)?;
        if output_text.len() > 128 * 1024 {
            return Err(invalid("child result exceeds the shared result bound"));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_paused(&transaction, context, &checkpoint)?;
        validate_canonical_tool_call_in_transaction(
            &transaction,
            checkpoint.session_id,
            checkpoint.turn_id,
            checkpoint.tool_call_id,
            crate::tool::ToolName::SharedDelegate,
        )?;
        let admission = AdmissionId::new();
        transaction.execute(
            "UPDATE sessions SET active_run_id = ?1, active_run_lease_expires_at_ms = ?2 WHERE id = ?3",
            params![admission.to_string(), run_lease_expiry_ms(SystemClock::now_ms(), RUN_ADMISSION_LEASE_DURATION_MS), checkpoint.session_id.to_string()],
        )?;
        let changed = transaction.execute("UPDATE tool_calls SET status = 'completed', finished_at_ms = ?1 WHERE id = ?2 AND status = 'pending'",
            params![SystemClock::now_ms(), checkpoint.tool_call_id.to_string()])?;
        if changed != 1 {
            return Err(invalid("shared child tool already settled or changed"));
        }
        let event = RunEvent::ToolCallCompleted {
            tool_call_id: checkpoint.tool_call_id,
            tool: crate::tool::ToolName::SharedDelegate,
            title: "Shared child settled".into(),
            summary: output_text,
            metadata: output,
        };
        insert_protocol_projection_if_requested(
            &transaction,
            &event,
            Some(checkpoint.session_id),
            checkpoint.turn_id,
            None,
        )?;
        transaction.execute(
            "UPDATE shared_run_checkpoints SET state = 'active', admission_id = ?1, checkpoint_json = NULL WHERE job_id = ?2",
            params![admission.to_string(), context.job_id],
        )?;
        let snapshot = AdmittedTurnSnapshot {
            admission_id: admission,
            admission_revision: session_admission_revision_in_connection(
                &transaction,
                checkpoint.session_id,
            )?,
            goal: None,
            initial_user_history_item_id: None,
        };
        transaction.commit()?;
        Ok(snapshot)
    }
}
