use super::*;

impl SqliteSessionRepository {
    pub async fn get_thread_goal(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        Ok(self
            .get_stored_thread_goal(thread_id)?
            .map(|stored| stored.goal))
    }

    pub async fn get_thread_goal_with_id(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<(ThreadGoal, String)>, StorageError> {
        Ok(self
            .get_stored_thread_goal(thread_id)?
            .map(|stored| (stored.goal, stored.goal_id)))
    }

    pub async fn replace_thread_goal(
        &self,
        thread_id: SessionId,
        objective: &str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> Result<ThreadGoal, StorageError> {
        validate_goal_objective_and_budget(objective, token_budget)?;
        let goal_id = ulid::Ulid::new().to_string();
        let now = SystemClock.now_ms();
        let status = status_after_budget_limit(status, 0, token_budget);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO thread_goals (
                 thread_id, goal_id, objective, status, token_budget, tokens_used,
                 time_used_seconds, created_at_ms, updated_at_ms
             )
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                 goal_id = excluded.goal_id,
                 objective = excluded.objective,
                 status = excluded.status,
                 token_budget = excluded.token_budget,
                 tokens_used = 0,
                 time_used_seconds = 0,
                 created_at_ms = excluded.created_at_ms,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                thread_id.to_string(),
                goal_id,
                objective,
                status.as_db_str(),
                token_budget,
                now,
                now
            ],
        )?;
        drop(connection);
        self.get_thread_goal(thread_id)
            .await?
            .ok_or_else(|| StorageError::Message("thread goal was not stored".to_string()))
    }

    pub async fn insert_thread_goal(
        &self,
        thread_id: SessionId,
        objective: &str,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        validate_goal_objective_and_budget(objective, token_budget)?;
        let goal_id = ulid::Ulid::new().to_string();
        let now = SystemClock.now_ms();
        let status = status_after_budget_limit(status, 0, token_budget);
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = connection.execute(
            "INSERT INTO thread_goals (
                 thread_id, goal_id, objective, status, token_budget, tokens_used,
                 time_used_seconds, created_at_ms, updated_at_ms
             )
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                 goal_id = excluded.goal_id,
                 objective = excluded.objective,
                 status = excluded.status,
                 token_budget = excluded.token_budget,
                 tokens_used = 0,
                 time_used_seconds = 0,
                 created_at_ms = excluded.created_at_ms,
                 updated_at_ms = excluded.updated_at_ms
             WHERE thread_goals.status = 'complete'",
            params![
                thread_id.to_string(),
                goal_id,
                objective,
                status.as_db_str(),
                token_budget,
                now,
                now
            ],
        )?;
        drop(connection);
        if changed == 0 {
            return Ok(None);
        }
        self.get_thread_goal(thread_id).await
    }

    pub async fn update_thread_goal(
        &self,
        thread_id: SessionId,
        objective: Option<&str>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        self.update_thread_goal_for_goal(thread_id, objective, status, token_budget, None)
            .await
    }

    pub async fn update_thread_goal_for_goal(
        &self,
        thread_id: SessionId,
        objective: Option<&str>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
        expected_goal_id: Option<&str>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        for _ in 0..8 {
            let Some(stored) = self.get_stored_thread_goal(thread_id)? else {
                return Ok(None);
            };
            if expected_goal_id.is_some_and(|expected| expected != stored.goal_id) {
                return Ok(Some(stored.goal));
            }
            let next_objective = objective
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(stored.goal.objective.as_str())
                .to_string();
            let next_token_budget = token_budget.unwrap_or(stored.goal.token_budget);
            validate_goal_objective_and_budget(&next_objective, next_token_budget)?;
            let requested_status = status.unwrap_or(stored.goal.status);
            let next_status = if stored.goal.status == ThreadGoalStatus::BudgetLimited
                && matches!(
                    requested_status,
                    ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked
                ) {
                ThreadGoalStatus::BudgetLimited
            } else {
                status_after_budget_limit(
                    requested_status,
                    stored.goal.tokens_used,
                    next_token_budget,
                )
            };
            let now = SystemClock::now_ms().max(stored.updated_at_ms.saturating_add(1));
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let changed = connection.execute(
                "UPDATE thread_goals
                 SET objective = ?2,
                     status = ?3,
                     token_budget = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    next_objective,
                    next_status.as_db_str(),
                    next_token_budget,
                    now,
                    stored.goal_id,
                    stored.updated_at_ms
                ],
            )?;
            drop(connection);
            if changed == 1 {
                return self.get_thread_goal(thread_id).await;
            }
        }
        Err(StorageError::Message(
            "thread goal changed repeatedly while applying an update; retry the operation"
                .to_string(),
        ))
    }

    pub async fn delete_thread_goal(&self, thread_id: SessionId) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = connection.execute(
            "DELETE FROM thread_goals WHERE thread_id = ?1",
            params![thread_id.to_string()],
        )?;
        Ok(changed > 0)
    }

    /// Creates or updates a direct CLI goal only while the captured idle owner remains exact.
    ///
    /// Idle validation, elapsed-time accounting, and the goal mutation share one immediate
    /// transaction. A turn admission or revision change therefore cannot slip between the
    /// caller's captured owner and the write.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn set_thread_goal_at_idle_expectation(
        &self,
        thread_id: SessionId,
        objective: Option<&str>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        let objective = objective.map(str::trim);
        let wall_clock_now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_idle_latest_turn_expectation_in_transaction(
            &transaction,
            thread_id,
            expected_latest_turn_id,
            expected_admission_revision,
        )?;

        let goal = match stored_thread_goal_from_connection(&transaction, thread_id)? {
            Some(stored) => {
                let next_objective = objective.unwrap_or(stored.goal.objective.as_str());
                let next_token_budget = token_budget.unwrap_or(stored.goal.token_budget);
                validate_goal_objective_and_budget(next_objective, next_token_budget)?;
                let elapsed_seconds = if matches!(
                    stored.goal.status,
                    ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
                ) {
                    wall_clock_now.saturating_sub(stored.updated_at_ms).max(0) / 1_000
                } else {
                    0
                };
                let time_used_seconds = stored
                    .goal
                    .time_used_seconds
                    .saturating_add(elapsed_seconds);
                let requested_status = status.unwrap_or(stored.goal.status);
                let next_status = if stored.goal.status == ThreadGoalStatus::BudgetLimited
                    && matches!(
                        requested_status,
                        ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked
                    ) {
                    ThreadGoalStatus::BudgetLimited
                } else {
                    status_after_budget_limit(
                        requested_status,
                        stored.goal.tokens_used,
                        next_token_budget,
                    )
                };
                let updated_at_ms = wall_clock_now.max(stored.updated_at_ms.saturating_add(1));
                let changed = transaction.execute(
                    "UPDATE thread_goals
                     SET objective = ?2,
                         status = ?3,
                         token_budget = ?4,
                         time_used_seconds = ?5,
                         updated_at_ms = ?6
                     WHERE thread_id = ?1
                       AND goal_id = ?7
                       AND updated_at_ms = ?8",
                    params![
                        thread_id.to_string(),
                        next_objective,
                        next_status.as_db_str(),
                        next_token_budget,
                        time_used_seconds,
                        updated_at_ms,
                        stored.goal_id,
                        stored.updated_at_ms,
                    ],
                )?;
                if changed != 1 {
                    return Err(StorageError::Message(
                        "thread goal changed while applying an exact idle mutation".to_string(),
                    ));
                }
                stored_thread_goal_from_connection(&transaction, thread_id)?
                    .expect("updated exact idle goal remains present")
                    .goal
            }
            None => {
                let Some(objective) = objective else {
                    transaction.commit()?;
                    return Ok(None);
                };
                let token_budget = token_budget.unwrap_or(None);
                validate_goal_objective_and_budget(objective, token_budget)?;
                let status = status_after_budget_limit(
                    status.unwrap_or(ThreadGoalStatus::Active),
                    0,
                    token_budget,
                );
                transaction.execute(
                    "INSERT INTO thread_goals (
                         thread_id, goal_id, objective, status, token_budget, tokens_used,
                         time_used_seconds, created_at_ms, updated_at_ms
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?6)",
                    params![
                        thread_id.to_string(),
                        ulid::Ulid::new().to_string(),
                        objective,
                        status.as_db_str(),
                        token_budget,
                        wall_clock_now,
                    ],
                )?;
                stored_thread_goal_from_connection(&transaction, thread_id)?
                    .expect("inserted exact idle goal remains present")
                    .goal
            }
        };
        transaction.commit()?;
        Ok(Some(goal))
    }

    pub(crate) async fn delete_thread_goal_at_idle_expectation(
        &self,
        thread_id: SessionId,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_idle_latest_turn_expectation_in_transaction(
            &transaction,
            thread_id,
            expected_latest_turn_id,
            expected_admission_revision,
        )?;
        let changed = transaction.execute(
            "DELETE FROM thread_goals WHERE thread_id = ?1",
            params![thread_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub(crate) async fn update_thread_goal_status_at_idle_expectation(
        &self,
        thread_id: SessionId,
        status: ThreadGoalStatus,
        expected_latest_turn_id: Option<TurnId>,
        expected_admission_revision: u64,
    ) -> Result<ThreadGoal, StorageError> {
        let wall_clock_now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_idle_latest_turn_expectation_in_transaction(
            &transaction,
            thread_id,
            expected_latest_turn_id,
            expected_admission_revision,
        )?;
        let stored =
            stored_thread_goal_from_connection(&transaction, thread_id)?.ok_or_else(|| {
                StorageError::Message(format!("session {thread_id} has no goal to update"))
            })?;
        let elapsed_seconds = if matches!(
            stored.goal.status,
            ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
        ) {
            wall_clock_now.saturating_sub(stored.updated_at_ms).max(0) / 1_000
        } else {
            0
        };
        let time_used_seconds = stored
            .goal
            .time_used_seconds
            .saturating_add(elapsed_seconds);
        let next_status = if stored.goal.status == ThreadGoalStatus::BudgetLimited
            && matches!(status, ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked)
        {
            ThreadGoalStatus::BudgetLimited
        } else {
            status_after_budget_limit(status, stored.goal.tokens_used, stored.goal.token_budget)
        };
        let updated_at_ms = wall_clock_now.max(stored.updated_at_ms.saturating_add(1));
        let changed = transaction.execute(
            "UPDATE thread_goals
             SET status = ?2,
                 time_used_seconds = ?3,
                 updated_at_ms = ?4
             WHERE thread_id = ?1
               AND goal_id = ?5
               AND updated_at_ms = ?6",
            params![
                thread_id.to_string(),
                next_status.as_db_str(),
                time_used_seconds,
                updated_at_ms,
                stored.goal_id,
                stored.updated_at_ms,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Message(
                "thread goal changed while applying an exact idle mutation".to_string(),
            ));
        }
        let goal = stored_thread_goal_from_connection(&transaction, thread_id)?
            .expect("updated goal remains present inside the exact transaction")
            .goal;
        transaction.commit()?;
        Ok(goal)
    }

    pub async fn account_thread_goal_usage(
        &self,
        thread_id: SessionId,
        token_delta: i64,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        self.account_thread_goal_usage_for_goal(thread_id, token_delta, None)
            .await
    }

    pub async fn account_thread_goal_usage_for_goal(
        &self,
        thread_id: SessionId,
        token_delta: i64,
        expected_goal_id: Option<&str>,
    ) -> Result<Option<ThreadGoal>, StorageError> {
        let token_delta = token_delta.max(0);
        for _ in 0..8 {
            let Some(stored) = self.get_stored_thread_goal(thread_id)? else {
                return Ok(None);
            };
            if expected_goal_id.is_some_and(|expected| expected != stored.goal_id) {
                return Ok(Some(stored.goal));
            }
            if !matches!(
                stored.goal.status,
                ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
            ) {
                return Ok(Some(stored.goal));
            }
            let wall_clock_now = SystemClock.now_ms();
            let time_delta_seconds = ((wall_clock_now - stored.updated_at_ms).max(0)) / 1000;
            if time_delta_seconds == 0 && token_delta == 0 {
                return Ok(Some(stored.goal));
            }
            let tokens_used = stored.goal.tokens_used.saturating_add(token_delta);
            let time_used_seconds = stored
                .goal
                .time_used_seconds
                .saturating_add(time_delta_seconds);
            let status = status_after_budget_limit(
                stored.goal.status,
                tokens_used,
                stored.goal.token_budget,
            );
            let now = wall_clock_now.max(stored.updated_at_ms.saturating_add(1));
            let connection = self.connection.lock().expect("sqlite mutex poisoned");
            let changed = connection.execute(
                "UPDATE thread_goals
                 SET status = ?2,
                     tokens_used = ?3,
                     time_used_seconds = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    status.as_db_str(),
                    tokens_used,
                    time_used_seconds,
                    now,
                    stored.goal_id,
                    stored.updated_at_ms
                ],
            )?;
            drop(connection);
            if changed == 1 {
                return self.get_thread_goal(thread_id).await;
            }
        }
        Err(StorageError::Message(
            "thread goal changed repeatedly while accounting usage; retry the operation"
                .to_string(),
        ))
    }

    fn get_stored_thread_goal(
        &self,
        thread_id: SessionId,
    ) -> Result<Option<StoredThreadGoal>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        stored_thread_goal_from_connection(&connection, thread_id)
    }
}

pub(super) struct StoredThreadGoal {
    pub(super) goal: ThreadGoal,
    pub(super) goal_id: String,
    updated_at_ms: i64,
}

pub(super) fn stored_thread_goal_from_connection(
    connection: &Connection,
    thread_id: SessionId,
) -> Result<Option<StoredThreadGoal>, StorageError> {
    let row = connection
        .query_row(
            "SELECT thread_id, goal_id, objective, status, token_budget, tokens_used,
                    time_used_seconds, created_at_ms, updated_at_ms
             FROM thread_goals
             WHERE thread_id = ?1",
            params![thread_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    row.map(stored_thread_goal_from_row).transpose()
}

pub(super) fn set_thread_goal_objective_in_transaction(
    transaction: &Transaction<'_>,
    thread_id: SessionId,
    objective: &str,
    now_ms: i64,
) -> Result<(), StorageError> {
    let objective = objective.trim();
    let stored = stored_thread_goal_from_connection(transaction, thread_id)?;
    match stored {
        Some(stored) => {
            validate_goal_objective_and_budget(objective, stored.goal.token_budget)?;
            let elapsed_seconds = if matches!(
                stored.goal.status,
                ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
            ) {
                now_ms.saturating_sub(stored.updated_at_ms).max(0) / 1_000
            } else {
                0
            };
            let time_used_seconds = stored
                .goal
                .time_used_seconds
                .saturating_add(elapsed_seconds);
            let status = status_after_budget_limit(
                ThreadGoalStatus::Active,
                stored.goal.tokens_used,
                stored.goal.token_budget,
            );
            let updated_at_ms = now_ms.max(stored.updated_at_ms.saturating_add(1));
            let changed = transaction.execute(
                "UPDATE thread_goals
                 SET objective = ?2,
                     status = ?3,
                     time_used_seconds = ?4,
                     updated_at_ms = ?5
                 WHERE thread_id = ?1
                   AND goal_id = ?6
                   AND updated_at_ms = ?7",
                params![
                    thread_id.to_string(),
                    objective,
                    status.as_db_str(),
                    time_used_seconds,
                    updated_at_ms,
                    stored.goal_id,
                    stored.updated_at_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StorageError::Message(
                    "thread goal changed while admitting its owning turn".to_string(),
                ));
            }
        }
        None => {
            validate_goal_objective_and_budget(objective, None)?;
            transaction.execute(
                "INSERT INTO thread_goals (
                     thread_id, goal_id, objective, status, token_budget, tokens_used,
                     time_used_seconds, created_at_ms, updated_at_ms
                 )
                 VALUES (?1, ?2, ?3, 'active', NULL, 0, 0, ?4, ?4)",
                params![
                    thread_id.to_string(),
                    ulid::Ulid::new().to_string(),
                    objective,
                    now_ms,
                ],
            )?;
        }
    }
    Ok(())
}

fn stored_thread_goal_from_row(
    row: (
        String,
        String,
        String,
        String,
        Option<i64>,
        i64,
        i64,
        i64,
        i64,
    ),
) -> Result<StoredThreadGoal, StorageError> {
    let (
        thread_id,
        goal_id,
        objective,
        status,
        token_budget,
        tokens_used,
        time_used_seconds,
        created_at_ms,
        updated_at_ms,
    ) = row;
    let thread_id = thread_id
        .parse::<SessionId>()
        .map_err(|error| StorageError::Message(format!("invalid thread goal id: {error}")))?;
    let status = ThreadGoalStatus::parse_db(&status).ok_or_else(|| {
        StorageError::Message(format!("invalid thread goal status `{status}` in storage"))
    })?;
    Ok(StoredThreadGoal {
        goal: ThreadGoal {
            thread_id,
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            created_at: created_at_ms / 1000,
            updated_at: updated_at_ms / 1000,
        },
        goal_id,
        updated_at_ms,
    })
}

pub(super) fn validate_goal_objective_and_budget(
    objective: &str,
    token_budget: Option<i64>,
) -> Result<(), StorageError> {
    validate_thread_goal_objective(objective).map_err(StorageError::Message)?;
    if token_budget.is_some_and(|budget| budget <= 0) {
        return Err(StorageError::Message(
            "goal token budget must be positive".to_string(),
        ));
    }
    Ok(())
}

fn status_after_budget_limit(
    status: ThreadGoalStatus,
    tokens_used: i64,
    token_budget: Option<i64>,
) -> ThreadGoalStatus {
    if token_budget.is_some_and(|budget| tokens_used >= budget) {
        ThreadGoalStatus::BudgetLimited
    } else {
        status
    }
}
