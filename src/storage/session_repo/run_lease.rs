use super::*;

impl SqliteSessionRepository {
    pub async fn renew_admitted_run_lease(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        self.renew_admitted_run_lease_with_clock(
            session_id,
            admission_id,
            turn_id,
            RUN_ADMISSION_LEASE_DURATION_MS,
            SystemClock::now_ms,
        )
    }

    pub async fn renew_admitted_run_lease_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        self.renew_admitted_run_lease_with_clock(
            session_id,
            admission_id,
            turn_id,
            lease_duration_ms,
            || now_ms,
        )
    }

    pub(super) fn renew_admitted_run_lease_with_clock(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        lease_duration_ms: i64,
        now_ms: impl FnOnce() -> i64,
    ) -> Result<RunAdmissionLeaseRenewalOutcome, StorageError> {
        let admission_id_text = admission_id.to_string();
        let turn_id_text = turn_id.to_string();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Both the shared connection and SQLite's writer lock can delay renewal.
        // Check freshness and extend the lease using the time after those waits.
        let now = normalize_run_lease_now_ms(now_ms());
        let requested_expiry = run_lease_expiry_ms(now, lease_duration_ms);
        let state = session_runtime_state_from_connection(&transaction, session_id)?;
        let outcome = match state {
            Some(runtime_state) if runtime_state.status == SessionStatus::Running => {
                let active_admission = runtime_state
                    .admission
                    .expect("running session admission validated before lease renewal");
                if active_admission.admission_id != admission_id
                    || active_admission.turn_id != turn_id
                {
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                } else if let Some(fence) = first_applicable_tree_stop_fence_for_turn_in_connection(
                    &transaction,
                    session_id,
                    turn_id,
                )? {
                    RunAdmissionLeaseRenewalOutcome::StopFenced(
                        recovery_terminal_outcome_for_tree_stop_fence(session_id, fence),
                    )
                } else if let Some(request) =
                    exact_execution_interrupt_request_for_admission_in_connection(
                        &transaction,
                        session_id,
                        active_admission,
                    )?
                {
                    RunAdmissionLeaseRenewalOutcome::InterruptRequested(request.cause)
                } else if !active_admission.is_fresh_at(now) {
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                } else {
                    let renewed = transaction.execute(
                        "UPDATE sessions
                         SET active_run_lease_expires_at_ms = MAX(
                                 active_run_lease_expires_at_ms,
                                 ?4
                             )
                          WHERE id = ?1
                            AND active_run_id = ?2
                            AND active_turn_id = ?3
                            AND active_run_lease_expires_at_ms > ?5
                            AND status = 'running'",
                        params![
                            session_id.to_string(),
                            admission_id_text,
                            turn_id_text,
                            requested_expiry,
                            now
                        ],
                    )?;
                    if renewed == 1 {
                        RunAdmissionLeaseRenewalOutcome::Renewed
                    } else {
                        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                    }
                }
            }
            Some(runtime_state)
                if matches!(
                    runtime_state.status,
                    SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
                ) =>
            {
                let retained_admission = runtime_state.admission;
                if let Some(retained_admission) = retained_admission {
                    let terminal = terminal_for_retained_admission_in_connection(
                        &transaction,
                        session_id,
                        runtime_state.status,
                        retained_admission,
                    )?;
                    if retained_admission.admission_id == admission_id
                        && retained_admission.turn_id == turn_id
                    {
                        RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                    } else {
                        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                    }
                } else if let Some(terminal) =
                    terminal_for_turn_in_connection(&transaction, session_id, turn_id)?
                {
                    RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                } else {
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
                }
            }
            _ => RunAdmissionLeaseRenewalOutcome::SupersededOrExpired,
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub async fn admitted_run_status(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<Option<SessionStatus>, StorageError> {
        self.admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub async fn admitted_run_status_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<Option<SessionStatus>, StorageError> {
        Ok(
            match self
                .admitted_run_state_at(session_id, admission_id, turn_id, now_ms)
                .await?
            {
                AdmittedRunState::OwnedRunning => Some(SessionStatus::Running),
                AdmittedRunState::Terminal(terminal) => Some(terminal.session_status()),
                AdmittedRunState::StopFenced(_) | AdmittedRunState::SupersededOrExpired => None,
            },
        )
    }

    pub(crate) async fn admitted_run_state(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<AdmittedRunState, StorageError> {
        self.admitted_run_state_at(session_id, admission_id, turn_id, SystemClock::now_ms())
            .await
    }

    pub(crate) async fn admitted_run_state_at(
        &self,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        now_ms: i64,
    ) -> Result<AdmittedRunState, StorageError> {
        let now = normalize_run_lease_now_ms(now_ms);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let Some(runtime_state) = session_runtime_state_from_connection(&connection, session_id)?
        else {
            return Ok(AdmittedRunState::SupersededOrExpired);
        };
        if runtime_state.status == SessionStatus::Running {
            let Some(admission) = runtime_state.fresh_admission_at(now).filter(|admission| {
                admission.admission_id == admission_id && admission.turn_id == turn_id
            }) else {
                return Ok(AdmittedRunState::SupersededOrExpired);
            };
            debug_assert_eq!(admission.turn_id, turn_id);
            if let Some(fence) = first_applicable_tree_stop_fence_for_turn_in_connection(
                &connection,
                session_id,
                turn_id,
            )? {
                return Ok(AdmittedRunState::StopFenced(
                    recovery_terminal_outcome_for_tree_stop_fence(session_id, fence),
                ));
            }
            return Ok(AdmittedRunState::OwnedRunning);
        }
        if matches!(
            runtime_state.status,
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
        ) && let Some(terminal) =
            terminal_for_turn_in_connection(&connection, session_id, turn_id)?
        {
            return Ok(AdmittedRunState::Terminal(terminal));
        }
        Ok(AdmittedRunState::SupersededOrExpired)
    }
}
