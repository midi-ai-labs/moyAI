use super::*;

impl SqliteSessionRepository {
    pub(crate) async fn settle_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<AdmittedTerminalSettlement, StorageError> {
        self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            event,
            TerminalOwnerGuard::Captured(target),
            None,
            true,
            false,
            true,
            None,
        )?
        .admitted_settlement()
    }

    pub(crate) async fn settle_orphaned_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<AdmittedTerminalSettlement, StorageError> {
        self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            event,
            TerminalOwnerGuard::Captured(target),
            None,
            false,
            true,
            true,
            None,
        )?
        .admitted_settlement()
    }

    #[cfg(test)]
    pub(crate) async fn terminalize_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, StorageError> {
        Ok(self
            .settle_captured_running_session_with_protocol_event(session_id, event, target)
            .await?
            .commit()
            .was_applied())
    }

    #[cfg(test)]
    pub(crate) async fn recover_captured_running_session_with_protocol_event(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, StorageError> {
        Ok(self
            .settle_orphaned_captured_running_session_with_protocol_event(session_id, event, target)
            .await?
            .commit()
            .was_applied())
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) async fn terminalize_admitted_turn_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        Ok(self
            .settle_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                event,
                protocol_turn_id,
                protocol_sequence_no,
                expected_active_goal_id_to_block,
            )
            .await?
            .commit())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn settle_admitted_turn_with_protocol_event(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalSettlement, StorageError> {
        self.settle_admitted_turn_with_protocol_event_and_mailbox_phase(
            session_id,
            admission_id,
            event,
            protocol_turn_id,
            protocol_sequence_no,
            true,
            expected_active_goal_id_to_block,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) async fn terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        accepts_mailbox_delivery_current_turn: bool,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalCommit, StorageError> {
        Ok(self
            .settle_admitted_turn_with_protocol_event_and_mailbox_phase(
                session_id,
                admission_id,
                event,
                protocol_turn_id,
                protocol_sequence_no,
                accepts_mailbox_delivery_current_turn,
                expected_active_goal_id_to_block,
            )
            .await?
            .commit())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn settle_admitted_turn_with_protocol_event_and_mailbox_phase(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        event: &RunEvent,
        protocol_turn_id: TurnId,
        protocol_sequence_no: Option<i64>,
        accepts_mailbox_delivery_current_turn: bool,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<AdmittedTerminalSettlement, StorageError> {
        self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            event,
            TerminalOwnerGuard::Admitted {
                admission_id,
                turn_id: protocol_turn_id,
            },
            protocol_sequence_no,
            false,
            false,
            accepts_mailbox_delivery_current_turn,
            expected_active_goal_id_to_block,
        )?
        .admitted_settlement()
    }

    pub(crate) fn settle_agent_execution_wake_with_terminal(
        &self,
        session_id: SessionId,
        wake: AgentExecutionWakeTerminalOwner,
        terminal: DurableTurnTerminal,
    ) -> Result<AgentExecutionWakeTerminalSettlement, StorageError> {
        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            return Err(StorageError::Message(
                "agent execution wake settlement only accepts an interrupted terminal".to_string(),
            ));
        }
        let event = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(terminal),
        };
        let settlement = self.terminalize_turn_with_protocol_event_guarded(
            session_id,
            &event,
            TerminalOwnerGuard::AgentWake(wake),
            None,
            false,
            false,
            false,
            None,
        )?;
        Ok(match settlement {
            GuardedTerminalization::Settled {
                commit: AdmittedTerminalCommit::Applied,
                turn_id,
                terminal,
            } => AgentExecutionWakeTerminalSettlement::Applied { turn_id, terminal },
            GuardedTerminalization::Settled {
                commit:
                    AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission
                    | AdmittedTerminalCommit::NotOwned,
                turn_id,
                terminal,
            } => AgentExecutionWakeTerminalSettlement::AlreadyTerminal { turn_id, terminal },
            GuardedTerminalization::BlockedByPendingDeferredCompletion { deferred_turn_id } => {
                AgentExecutionWakeTerminalSettlement::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                }
            }
            GuardedTerminalization::NotOwned => {
                AgentExecutionWakeTerminalSettlement::WakeUnavailable
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn terminalize_turn_with_protocol_event_guarded(
        &self,
        session_id: SessionId,
        event: &RunEvent,
        owner_guard: TerminalOwnerGuard<'_>,
        protocol_sequence_no: Option<i64>,
        retain_active_admission: bool,
        orphan_recovery: bool,
        accepts_mailbox_delivery_current_turn: bool,
        expected_active_goal_id_to_block: Option<&str>,
    ) -> Result<GuardedTerminalization, StorageError> {
        let requested_terminal = validate_terminal_event(session_id, event)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let shared_admission = if let TerminalOwnerGuard::SharedCheckpoint(checkpoint) = owner_guard
        {
            match shared_runs::pending_checkpoint_admission(&transaction, checkpoint)? {
                Some(admission) => {
                    validate_canonical_tool_call_in_transaction(
                        &transaction,
                        session_id,
                        checkpoint.turn_id,
                        checkpoint.tool_call_id,
                        crate::tool::ToolName::SharedDelegate,
                    )?;
                    if count_unfinished_tool_calls_for_turn_in_transaction(
                        &transaction,
                        session_id,
                        checkpoint.turn_id,
                    )? != 1
                    {
                        return Err(StorageError::Message(
                            "paused shared turn must retain exactly one unfinished child call"
                                .into(),
                        ));
                    }
                    Some(admission)
                }
                None => {
                    transaction.commit()?;
                    return Ok(GuardedTerminalization::NotOwned);
                }
            }
        } else {
            None
        };
        let Some(runtime_state) = session_runtime_state_from_connection(&transaction, session_id)?
        else {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        };
        let wake_claim = match owner_guard {
            TerminalOwnerGuard::AgentWake(AgentExecutionWakeTerminalOwner::ExplicitTask(
                history_item_id,
            )) => {
                agent_trigger_turn_claim_in_connection(&transaction, session_id, history_item_id)?
                    .map(|(admission_id, turn_id)| (Some(admission_id), turn_id))
            }
            TerminalOwnerGuard::AgentWake(AgentExecutionWakeTerminalOwner::OwnerResume(
                request_id,
            )) => owner_resume_claimed_turn_in_connection(&transaction, session_id, request_id)?
                .map(|turn_id| (None, turn_id)),
            TerminalOwnerGuard::Admitted { .. }
            | TerminalOwnerGuard::Captured(_)
            | TerminalOwnerGuard::SharedCheckpoint(_) => None,
        };
        if let TerminalOwnerGuard::AgentWake(wake) = owner_guard
            && wake_claim.is_none()
        {
            let settlement = match wake {
                AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id) => {
                    settle_pending_agent_trigger_in_transaction(
                        &transaction,
                        session_id,
                        history_item_id,
                        now,
                        requested_terminal.clone(),
                        None,
                    )?
                }
                AgentExecutionWakeTerminalOwner::OwnerResume(request_id) => {
                    settle_pending_owner_resume_in_transaction(
                        &transaction,
                        session_id,
                        request_id,
                        now,
                        requested_terminal.clone(),
                    )?
                }
            };
            let result = match settlement {
                PendingAgentTriggerSettlement::Applied { turn_id, .. } => {
                    let terminal = terminal_for_turn_in_connection(
                        &transaction,
                        session_id,
                        turn_id,
                    )?
                    .ok_or_else(|| {
                        StorageError::Message(format!(
                            "applied agent wake settlement omitted terminal for turn {turn_id}"
                        ))
                    })?;
                    GuardedTerminalization::Settled {
                        commit: AdmittedTerminalCommit::Applied,
                        turn_id,
                        terminal,
                    }
                }
                PendingAgentTriggerSettlement::WakeOwnedOrResolved => {
                    GuardedTerminalization::NotOwned
                }
                PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                    deferred_turn_id,
                } => {
                    GuardedTerminalization::BlockedByPendingDeferredCompletion { deferred_turn_id }
                }
            };
            transaction.commit()?;
            return Ok(result);
        }
        let Some(durable_admission) = runtime_state.admission else {
            let settled = match wake_claim {
                Some((_, turn_id)) => {
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                        .map(|terminal| (turn_id, terminal))
                }
                None => None,
            };
            transaction.commit()?;
            return Ok(match settled {
                Some((turn_id, terminal)) => GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id,
                    terminal,
                },
                None => GuardedTerminalization::NotOwned,
            });
        };
        let admitted_guard = matches!(
            owner_guard,
            TerminalOwnerGuard::Admitted { .. } | TerminalOwnerGuard::AgentWake(_)
        );
        let owner_matches = match owner_guard {
            TerminalOwnerGuard::Admitted {
                admission_id,
                turn_id,
            } => {
                durable_admission.admission_id == admission_id
                    && durable_admission.turn_id == turn_id
                    && durable_admission.is_fresh_at(now)
            }
            TerminalOwnerGuard::Captured(target) => target.matches(durable_admission),
            TerminalOwnerGuard::SharedCheckpoint(_) => shared_admission == Some(durable_admission),
            TerminalOwnerGuard::AgentWake(_) => {
                wake_claim.is_some_and(|(expected_admission_id, expected_turn_id)| {
                    expected_turn_id == durable_admission.turn_id
                        && expected_admission_id.is_none_or(|admission_id| {
                            admission_id == durable_admission.admission_id
                        })
                })
            }
        };
        if !owner_matches {
            let settled = match wake_claim {
                Some((_, turn_id)) => {
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                        .map(|terminal| (turn_id, terminal))
                }
                None => None,
            };
            transaction.commit()?;
            return Ok(match settled {
                Some((turn_id, terminal)) => GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id,
                    terminal,
                },
                None => GuardedTerminalization::NotOwned,
            });
        }
        let protocol_turn_id = durable_admission.turn_id;
        let admission_id_text = durable_admission.admission_id.to_string();
        let applicable_tree_stop_fence = first_applicable_tree_stop_fence_for_turn_in_connection(
            &transaction,
            session_id,
            protocol_turn_id,
        )?;
        let pending_exact_execution_interrupt_request =
            exact_execution_interrupt_request_for_admission_in_connection(
                &transaction,
                session_id,
                durable_admission,
            )?;
        if matches!(
            requested_terminal.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped
            }
        ) && applicable_tree_stop_fence.is_none()
        {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        let reconstructed_wake_event = if matches!(owner_guard, TerminalOwnerGuard::AgentWake(_))
            && matches!(
                requested_terminal.outcome,
                TurnTerminalOutcome::Interrupted { .. }
            ) {
            let snapshot =
                canonical_turn_snapshot_in_transaction(&transaction, session_id, protocol_turn_id)?;
            let mut terminal = requested_terminal.clone();
            terminal.final_response_id = None;
            terminal.tool_call_count = snapshot.tool_call_count;
            terminal.failed_tool_count = snapshot.failed_tool_count;
            terminal.change_count = snapshot.change_count;
            terminal.metrics = Default::default();
            Some(RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(terminal),
            })
        } else {
            None
        };
        let requested_event = reconstructed_wake_event.as_ref().unwrap_or(event);
        let requested_terminal = validate_terminal_event(session_id, requested_event)?;
        let tree_fence_recovery_event = orphan_recovery
            .then(|| {
                applicable_tree_stop_fence.map(|fence| {
                    let mut terminal = requested_terminal.clone();
                    terminal.outcome =
                        recovery_terminal_outcome_for_tree_stop_fence(session_id, fence);
                    terminal.final_response_id = None;
                    RunEvent::TurnTerminal {
                        session_id,
                        terminal: Box::new(terminal),
                    }
                })
            })
            .flatten();
        // The storage transaction is the final terminal linearization owner. A cross-process
        // exact interrupt that committed before this transaction must beat a stale normal
        // Completed/Failed producer, including one that began a local success reservation first.
        // The status CAS below consumes the request before the terminal INSERT; if that CAS won
        // first, a later request instead observes the terminal state and returns TargetChanged.
        let exact_interrupt_event = (applicable_tree_stop_fence.is_none()
            && !matches!(
                requested_terminal.outcome,
                TurnTerminalOutcome::Interrupted { .. }
            ))
        .then(|| pending_exact_execution_interrupt_request)
        .flatten()
        .map(|request| {
            let mut terminal = requested_terminal.clone();
            terminal.outcome = TurnTerminalOutcome::Interrupted {
                cause: request.cause,
            };
            terminal.final_response_id = None;
            RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(terminal),
            }
        });
        let authoritative_event = tree_fence_recovery_event.or(exact_interrupt_event);
        let event = authoritative_event.as_ref().unwrap_or(requested_event);
        let terminal = validate_terminal_event(session_id, event)?;
        let status = terminal.session_status();
        if let Some(fence) = applicable_tree_stop_fence
            && !terminal_is_compatible_with_tree_stop_fence(session_id, terminal, fence)
        {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        if runtime_state.status != SessionStatus::Running {
            terminal_for_retained_admission_in_connection(
                &transaction,
                session_id,
                runtime_state.status,
                durable_admission,
            )?;
            let actual_terminal = terminal_for_turn_in_connection(
                &transaction,
                session_id,
                protocol_turn_id,
            )?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "retained terminal owner for turn {protocol_turn_id} has no durable terminal"
                ))
            })?;
            if admitted_guard {
                transaction.execute(
                    "UPDATE sessions
                     SET active_run_id = NULL,
                         active_turn_id = NULL,
                         active_run_lease_expires_at_ms = NULL
                     WHERE id = ?1
                       AND active_run_id = ?2
                       AND active_turn_id = ?3
                       AND active_run_lease_expires_at_ms = ?4",
                    params![
                        session_id.to_string(),
                        admission_id_text,
                        protocol_turn_id.to_string(),
                        durable_admission.lease_expires_at_ms,
                    ],
                )?;
                transaction.commit()?;
                return Ok(GuardedTerminalization::Settled {
                    commit: AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission,
                    turn_id: protocol_turn_id,
                    terminal: actual_terminal,
                });
            }
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }
        if terminal_for_turn_in_connection(&transaction, session_id, protocol_turn_id)?.is_some() {
            return Err(StorageError::Message(format!(
                "running session {session_id} active turn {protocol_turn_id} already has a durable terminal"
            )));
        }
        let terminal_resolves_owner_resume = transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                   AND state = 'claimed'
                   AND claimed_turn_id = ?2
             )",
            params![session_id.to_string(), protocol_turn_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?;
        let pending_deferred_before_terminal = deferred_agent_completion_in_connection(
            &transaction,
            session_id,
            None,
            Some("pending"),
        )?;
        let terminal_recovers_pending_crash = !orphan_recovery
            && pending_deferred_before_terminal
                .as_ref()
                .is_some_and(|deferred| deferred.kind == DeferredAgentCompletionKind::CrashFailed);

        let recovering_owner_resume = orphan_recovery && terminal_resolves_owner_resume;

        let pending_direct_child_result =
            pending_direct_child_result_terminal_in_connection(&transaction, session_id)?;
        let explicit_wake_claim = agent_trigger_history_item_for_turn_in_connection(
            &transaction,
            session_id,
            protocol_turn_id,
        )?;

        // Codex does not reject a model terminal and force another sample merely because input
        // raced the final response. Mail already eligible for this turn is atomically recorded in
        // canonical history before the terminal; mail assigned to the next-turn phase remains
        // pending for the next explicit turn. Direct-child FINAL messages follow the same rule.
        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // Admission already selected this one immutable wake. Even if setup fails before the
            // normal safe mailbox boundary, its Completed/Failed terminal must consume exactly
            // that input. Leaving it pending would make a later scheduler retry collide with the
            // immutable V53 claim. Mail accepted after this selected wake remains next-turn work
            // unless the regular current-turn delivery phase was reached.
            if let Some(history_item_id) = explicit_wake_claim {
                deliver_claimed_explicit_agent_wake_in_transaction(
                    &transaction,
                    session_id,
                    protocol_turn_id,
                    history_item_id,
                    now,
                )?;
            }
            if accepts_mailbox_delivery_current_turn {
                deliver_all_pending_agent_mail_in_transaction(
                    &transaction,
                    session_id,
                    protocol_turn_id,
                    now,
                )?;
            }
        }

        let retained_parent = retained_agent_parent_in_connection(&transaction, session_id)?;
        // Like Codex threads, every agent owns its own terminal independently of descendant
        // liveness. Waiting for a result is an explicit model/tool decision; a normal Completed
        // terminal neither waits for descendants nor defers its direct-parent handoff.
        //
        // Descendant liveness remains relevant only to crash recovery. A recovered non-root owner
        // may need one exact deferred failure generation so a retry can supersede it without
        // publishing a synthetic result upstream.
        let has_durable_descendant_work = orphan_recovery
            && retained_parent.is_some()
            && session_has_durable_descendant_work_in_connection(&transaction, session_id)?;
        let deferred_kind = retained_parent.and_then(|_| {
            if orphan_recovery
                && matches!(terminal.outcome, TurnTerminalOutcome::Failed { .. })
                && (has_durable_descendant_work
                    || pending_direct_child_result.is_some()
                    || recovering_owner_resume)
            {
                Some(DeferredAgentCompletionKind::CrashFailed)
            } else {
                None
            }
        });

        if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // Codex records steering that won the narrow race after the
            // post-response pending-input check when the task finishes
            // normally or unexpectedly. It is history evidence, but it is not
            // sampled by another model request.
            deliver_all_pending_turn_steers_in_transaction(
                &transaction,
                session_id,
                durable_admission.admission_id,
                protocol_turn_id,
                now,
            )?;
        }

        let status_text = session_status_text(status);
        let clear_admission = !retain_active_admission;
        let terminalized = if clear_admission {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?5,
                     updated_at_ms = ?6,
                     completed_at_ms = ?6,
                     active_run_id = NULL,
                     active_turn_id = NULL,
                     active_run_lease_expires_at_ms = NULL
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms = ?4
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id_text,
                    protocol_turn_id.to_string(),
                    durable_admission.lease_expires_at_ms,
                    status_text,
                    now,
                ],
            )? == 1
        } else {
            transaction.execute(
                "UPDATE sessions
                 SET status = ?5, updated_at_ms = ?6, completed_at_ms = ?6
                 WHERE id = ?1
                   AND active_run_id = ?2
                   AND active_turn_id = ?3
                   AND active_run_lease_expires_at_ms = ?4
                   AND status = 'running'",
                params![
                    session_id.to_string(),
                    admission_id_text,
                    protocol_turn_id.to_string(),
                    durable_admission.lease_expires_at_ms,
                    status_text,
                    now,
                ],
            )? == 1
        };
        if !terminalized {
            transaction.commit()?;
            return Ok(GuardedTerminalization::NotOwned);
        }

        if status == SessionStatus::Failed
            && let Some(expected_goal_id) = expected_active_goal_id_to_block
        {
            transaction.execute(
                "UPDATE thread_goals
                 SET status = 'blocked', updated_at_ms = MAX(updated_at_ms + 1, ?3)
                 WHERE thread_id = ?1 AND goal_id = ?2 AND status = 'active'",
                params![session_id.to_string(), expected_goal_id, now],
            )?;
        }

        let protocol_sequence_no = resolve_terminal_protocol_sequence_in_transaction(
            &transaction,
            session_id,
            protocol_turn_id,
            protocol_sequence_no,
        )?;
        let terminal_sequence_no = settle_unfinished_tool_calls_for_terminal_event(
            &transaction,
            session_id,
            event,
            protocol_turn_id,
            protocol_sequence_no,
            now,
        )?;
        insert_protocol_projection_if_requested(
            &transaction,
            event,
            Some(session_id),
            protocol_turn_id,
            Some(terminal_sequence_no),
        )?;
        if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            let terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            if let Some(history_item_id) = explicit_wake_claim {
                discard_pending_explicit_agent_wake_in_transaction(
                    &transaction,
                    session_id,
                    history_item_id,
                    terminal_event_id,
                    now,
                )?;
            }
            discard_all_pending_turn_steers_in_transaction(
                &transaction,
                session_id,
                durable_admission.admission_id,
                protocol_turn_id,
                terminal_event_id,
                now,
            )?;
        }
        let terminal_is_tree_stop_fenced =
            turn_started_before_applicable_tree_stop_fence_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
            )?;
        if terminal_is_tree_stop_fenced {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            discard_pending_deferred_completions_for_fenced_terminal_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        if !terminal_is_tree_stop_fenced
            && (terminal_resolves_owner_resume || terminal_recovers_pending_crash)
            && matches!(
                terminal.outcome,
                TurnTerminalOutcome::Completed | TurnTerminalOutcome::Failed { .. }
            )
        {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            supersede_pending_deferred_completion_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        if !terminal_is_tree_stop_fenced
            && (terminal_resolves_owner_resume || terminal_recovers_pending_crash)
            && matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
        {
            let resolver_terminal_event_id =
                exact_terminal_event_id_in_transaction(&transaction, session_id, protocol_turn_id)?;
            discard_pending_crash_deferred_completion_in_transaction(
                &transaction,
                session_id,
                resolver_terminal_event_id,
                now,
            )?;
        }
        let recovery_has_pending_owner_resume = if orphan_recovery {
            repend_claimed_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
            has_pending_owner_resume_requests_in_connection(&transaction, session_id)?
        } else {
            resolve_claimed_owner_resume_requests_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
            false
        };
        // Only the first fence's compatible destructive terminal can be fenced here. It
        // closes the old generation without publishing a result, deferring ownership, or
        // recreating an OwnerResume.
        publish_or_defer_terminal_handoff(
            &transaction,
            TerminalHandoff {
                session_id,
                turn_id: protocol_turn_id,
                tree_stop_fenced: terminal_is_tree_stop_fenced,
                deferred: retained_parent.zip(deferred_kind),
                pending_direct_child_result,
                owner_resume_pending: recovery_has_pending_owner_resume,
                root_label: "deferred crash owner tree root",
                now_ms: now,
            },
            || Ok(terminal),
        )?;
        if terminal_is_tree_stop_fenced {
            // The durable fence already discarded only the generations at or before its
            // boundary. A late compatible terminal must not broaden that scope to newer work.
        } else if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. }) {
            // An ordinary Stop/approval abort owns only this exact task. It may release an
            // ancestor that is now genuinely quiescent, but it never classifies descendants.
            release_quiescent_deferred_completions_after_interruption_in_transaction(
                &transaction,
                session_id,
                protocol_turn_id,
                now,
            )?;
        }
        let committed_terminal = terminal.clone();
        if let TerminalOwnerGuard::SharedCheckpoint(checkpoint) = owner_guard {
            // Keep the shared-job binding so local admission cannot borrow this history.
            // The checkpoint and canonical terminal are cleared/committed as one fact.
            transaction.execute(
                "UPDATE shared_run_checkpoints SET state = 'active', checkpoint_json = NULL
                 WHERE job_id = ?1 AND session_id = ?2 AND turn_id = ?3 AND admission_id = ?4",
                params![
                    checkpoint.job_id,
                    session_id.to_string(),
                    protocol_turn_id.to_string(),
                    admission_id_text
                ],
            )?;
        }
        transaction.commit()?;
        Ok(GuardedTerminalization::Settled {
            commit: AdmittedTerminalCommit::Applied,
            turn_id: protocol_turn_id,
            terminal: committed_terminal,
        })
    }
}
