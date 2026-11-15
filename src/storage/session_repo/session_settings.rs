use super::*;

impl SqliteSessionRepository {
    pub async fn compare_and_set_root_session_access_mode(
        &self,
        session_id: SessionId,
        expected_access_mode: AccessMode,
        access_mode: AccessMode,
    ) -> Result<Option<SessionSettingsUpdate>, StorageError> {
        let current = self.get_session(session_id).await?;
        self.compare_and_set_root_session_access_mode_at_revision(
            session_id,
            current.session_settings_revision,
            expected_access_mode,
            access_mode,
        )
        .await
    }

    pub async fn compare_and_set_root_session_access_mode_at_revision(
        &self,
        session_id: SessionId,
        expected_session_settings_revision: u64,
        expected_access_mode: AccessMode,
        access_mode: AccessMode,
    ) -> Result<Option<SessionSettingsUpdate>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_record_from_connection(&transaction, session_id)?;
        let owned_session_kind = transaction
            .query_row(
                "SELECT 'child agent session' FROM session_spawn_edges WHERE child_session_id = ?1
                 UNION ALL SELECT 'remote job session' FROM remote_agent_jobs WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(owned_session_kind) = owned_session_kind {
            return Err(StorageError::Message(format!(
                "session {session_id} is a {owned_session_kind}; root access mode ownership was rejected"
            )));
        }
        if current.session_settings_revision != expected_session_settings_revision
            || current.access_mode != expected_access_mode
        {
            transaction.commit()?;
            return Ok(None);
        }
        if current.access_mode == access_mode {
            transaction.commit()?;
            return Ok(Some(SessionSettingsUpdate {
                session: current,
                changed: false,
            }));
        }
        if current.session_settings_revision >= i64::MAX as u64 {
            return Err(StorageError::Message(format!(
                "root session {session_id} settings revision is exhausted"
            )));
        }
        let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
        let updated = transaction.execute(
            "UPDATE sessions
             SET access_mode = ?3, updated_at_ms = ?4,
                 session_settings_revision = session_settings_revision + 1
             WHERE id = ?1
               AND access_mode = ?2
               AND session_settings_revision = ?5
               AND session_settings_revision < 9223372036854775807
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )",
            params![
                session_id.to_string(),
                expected_access_mode.as_str(),
                access_mode.as_str(),
                now,
                i64::try_from(expected_session_settings_revision).map_err(|_| {
                    StorageError::Message(format!(
                        "root session {session_id} settings revision is out of range"
                    ))
                })?
            ],
        )?;
        if updated != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let session = session_record_from_connection(&transaction, session_id)?;
        transaction.commit()?;
        Ok(Some(SessionSettingsUpdate {
            session,
            changed: true,
        }))
    }

    pub async fn compare_and_set_root_session_settings(
        &self,
        session_id: SessionId,
        expected_session_settings_revision: u64,
        patch: &SessionSettingsPatch,
    ) -> Result<Option<SessionSettingsUpdate>, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_record_from_connection(&transaction, session_id)?;
        let owned_session_kind = transaction
            .query_row(
                "SELECT 'child agent session' FROM session_spawn_edges WHERE child_session_id = ?1
                 UNION ALL SELECT 'remote job session' FROM remote_agent_jobs WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(owned_session_kind) = owned_session_kind {
            return Err(StorageError::Message(format!(
                "session {session_id} is a {owned_session_kind}; root settings ownership was rejected"
            )));
        }
        if current.session_settings_revision != expected_session_settings_revision {
            transaction.commit()?;
            return Ok(None);
        }

        let next_cwd = patch.cwd.clone().unwrap_or_else(|| current.cwd.clone());
        let next_model = patch.model.clone().unwrap_or_else(|| current.model.clone());
        let next_base_url = patch
            .base_url
            .clone()
            .unwrap_or_else(|| current.base_url.clone());
        let next_base_url = ProviderEndpoint::parse(&next_base_url)
            .map_err(|error| StorageError::Message(error.to_string()))?
            .as_str()
            .to_string();
        let next_access_mode = patch.access_mode.unwrap_or(current.access_mode);
        let next_provider_connection = provider_connection_for_settings_update(
            &current,
            &next_base_url,
            patch.provider_connection.as_ref(),
        );
        let next_model_parameters = patch.apply_to_model_parameters(&current.model_parameters);
        let changed = next_cwd != current.cwd
            || next_model != current.model
            || next_base_url != current.base_url
            || next_access_mode != current.access_mode
            || next_provider_connection != current.provider_connection
            || next_model_parameters != current.model_parameters;
        if !changed {
            transaction.commit()?;
            return Ok(Some(SessionSettingsUpdate {
                session: current,
                changed: false,
            }));
        }
        if let Some(active_session_id) =
            active_session_for_mutation_branch(&transaction, session_id, true)?
        {
            return Err(StorageError::SessionSettingsActiveTree {
                root_session_id: session_id,
                active_session_id,
            });
        }
        let expected_revision =
            i64::try_from(expected_session_settings_revision).map_err(|_| {
                StorageError::Message(format!(
                    "root session {session_id} settings revision is out of range"
                ))
            })?;
        if expected_revision == i64::MAX {
            return Err(StorageError::Message(format!(
                "root session {session_id} settings revision is exhausted"
            )));
        }
        let now = SystemClock::now_ms().max(current.updated_at_ms.saturating_add(1));
        let updated = transaction.execute(
            "UPDATE sessions
             SET cwd_path = ?2, model_name = ?3, base_url = ?4, access_mode = ?5,
                 model_parameters_json = ?6, provider_connection_json = ?7,
                 updated_at_ms = ?8,
                 session_settings_revision = session_settings_revision + 1
             WHERE id = ?1
               AND session_settings_revision = ?9
               AND session_settings_revision < 9223372036854775807
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )",
            params![
                session_id.to_string(),
                next_cwd.as_str(),
                next_model,
                next_base_url,
                next_access_mode.as_str(),
                serde_json::to_string(&next_model_parameters)?,
                serialize_provider_connection(&next_provider_connection)?,
                now,
                expected_revision,
            ],
        )?;
        if updated != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let session = session_record_from_connection(&transaction, session_id)?;
        transaction.commit()?;
        Ok(Some(SessionSettingsUpdate {
            session,
            changed: true,
        }))
    }
}
