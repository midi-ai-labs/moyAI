use super::*;

impl SqliteSessionRepository {
    pub async fn session_projection_state(
        &self,
        session_id: SessionId,
    ) -> Result<SessionProjectionState, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id),
                    (SELECT revision FROM session_admission_revisions
                     WHERE session_id = sessions.id)
             FROM sessions
             WHERE id = ?1",
        )?;
        let row =
            statement.query_row(params![session_id.to_string()], session_projection_from_row)?;
        validate_session_projection_state(row)
    }

    pub async fn list_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionProjectionState>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id),
                    (SELECT revision FROM session_admission_revisions
                     WHERE session_id = sessions.id)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM side_chat_bindings
                   WHERE conversation_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM remote_agent_jobs
                   WHERE session_id = sessions.id
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), sqlite_limit(limit)?],
                session_projection_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(validate_session_projection_state)
            .collect()
    }

    pub async fn search_sessions_with_projection_state(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionProjectionState>, StorageError> {
        let normalized = format!(
            "%{}%",
            escape_like_literal(&query.trim().to_ascii_lowercase())
        );
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let archived_filter = if include_archived {
            ""
        } else {
            " AND archived_at_ms IS NULL"
        };
        let sql = format!(
            "SELECT id, project_id, title, status, cwd_path, model_name, base_url, access_mode,
                    model_parameters_json, provider_connection_json,
                    created_at_ms, updated_at_ms, completed_at_ms,
                    session_settings_revision,
                    archived_at_ms IS NOT NULL, active_run_id, active_turn_id,
                    active_run_lease_expires_at_ms,
                    (SELECT COUNT(*) FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                    (SELECT terminal_event.msg_json FROM protocol_runtime_events AS terminal_event
                     WHERE terminal_event.session_id = sessions.id
                       AND terminal_event.turn_id = sessions.active_turn_id
                       AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                     ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC LIMIT 1),
                    (SELECT allocator.next_sequence_no
                     FROM protocol_turn_sequence_allocators AS allocator
                     WHERE allocator.session_id = sessions.id
                       AND allocator.turn_id = sessions.active_turn_id),
                    (SELECT revision FROM session_admission_revisions
                     WHERE session_id = sessions.id)
             FROM sessions
             WHERE project_id = ?1{archived_filter}
               AND NOT EXISTS (
                   SELECT 1 FROM session_spawn_edges
                   WHERE child_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM side_chat_bindings
                   WHERE conversation_session_id = sessions.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM remote_agent_jobs
                   WHERE session_id = sessions.id
               )
               AND (
                   lower(title) LIKE ?2 ESCAPE '\\'
                   OR lower(cwd_path) LIKE ?2 ESCAPE '\\'
                   OR lower(model_name) LIKE ?2 ESCAPE '\\'
                   OR lower(base_url) LIKE ?2 ESCAPE '\\'
                   OR lower(access_mode) LIKE ?2 ESCAPE '\\'
               )
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![project_id.to_string(), normalized, sqlite_limit(limit)?],
                session_projection_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(validate_session_projection_state)
            .collect()
    }
}

#[derive(Debug)]
struct RawSessionProjectionState {
    session: SessionRecord,
    archived: bool,
    active_run_id: Option<String>,
    active_turn_id: Option<String>,
    active_run_lease_expires_at_ms: Option<i64>,
    terminal_count: i64,
    terminal_json: Option<String>,
    active_turn_sequence_no: Option<i64>,
    admission_revision: i64,
}

fn session_projection_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RawSessionProjectionState> {
    Ok(RawSessionProjectionState {
        session: session_record_with_identity_from_row(row)?,
        archived: row.get(14)?,
        active_run_id: row.get(15)?,
        active_turn_id: row.get(16)?,
        active_run_lease_expires_at_ms: row.get(17)?,
        terminal_count: row.get(18)?,
        terminal_json: row.get(19)?,
        active_turn_sequence_no: row.get(20)?,
        admission_revision: row.get(21)?,
    })
}

fn validate_session_projection_state(
    raw: RawSessionProjectionState,
) -> Result<SessionProjectionState, StorageError> {
    let runtime_state = validate_raw_session_runtime_state(
        raw.session.id,
        RawSessionRuntimeState {
            status: session_status_text(raw.session.status).to_string(),
            active_run_id: raw.active_run_id,
            active_turn_id: raw.active_turn_id,
            active_run_lease_expires_at_ms: raw.active_run_lease_expires_at_ms,
            terminal_count: raw.terminal_count,
            terminal_json: raw.terminal_json,
        },
    )?;
    let (active_turn_id, active_turn_sequence_no) =
        if runtime_state.status == SessionStatus::Running {
            let active_turn_id = runtime_state
                .admission
                .expect("running session projection admission validated before projection")
                .turn_id;
            (
                Some(active_turn_id),
                Some(raw.active_turn_sequence_no.unwrap_or(0)),
            )
        } else {
            (None, None)
        };
    let admission_revision = u64::try_from(raw.admission_revision).map_err(|_| {
        StorageError::Message(format!(
            "session {} has invalid admission revision {}",
            raw.session.id, raw.admission_revision
        ))
    })?;
    Ok(SessionProjectionState {
        session: raw.session,
        archived: raw.archived,
        active_turn_id,
        active_turn_sequence_no,
        admission_revision,
    })
}
