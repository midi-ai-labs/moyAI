use super::*;

impl SqliteSessionRepository {
    /// Includes archived and child sessions because project deletion removes
    /// their local history too. A truncated scan must never authorize deletion.
    pub(crate) fn project_session_ids_for_origin_guard(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection
            .prepare("SELECT id FROM sessions WHERE project_id = ?1 ORDER BY id LIMIT ?2")?;
        let sql_limit = i64::try_from(limit.saturating_add(1))
            .map_err(|_| StorageError::Message("project chat scan limit is invalid".into()))?;
        let rows = statement
            .query_map(params![project_id.to_string(), sql_limit], |row| {
                parse_session_id_column(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > limit {
            return Err(StorageError::Message(
                "project has too many chats to verify remote work before deletion".into(),
            ));
        }
        Ok(rows)
    }

    /// Capture the complete local mutation target before consulting Hub. The
    /// checked mutation compares this exact set again inside its write
    /// transaction, including sessions with no delegation history yet.
    pub(crate) fn origin_history_revisions_for_session_tree(
        &self,
        session_id: SessionId,
        limit: usize,
    ) -> Result<Vec<(SessionId, u64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        origin_history_revisions_for_tree_in_connection(&connection, session_id, limit)
    }

    pub(crate) fn origin_history_revisions_for_project(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<(SessionId, u64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        origin_history_revisions_for_project_in_connection(&connection, project_id, limit)
    }

    /// Accepted Hub job identities recorded by this chat's canonical tool
    /// results. The caller must also check durable pending submission receipts:
    /// a failed tool result alone cannot prove an uncertain POST was rejected.
    pub(crate) fn origin_delegated_job_ids(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<String>, StorageError> {
        const MAX_TOOL_ROWS: usize = 32_768;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT json_extract(payload_json, '$.kind'),
                    json_extract(payload_json, '$.call_id'),
                    json_extract(payload_json, '$.tool_name'),
                    json_extract(payload_json, '$.status'),
                    json_extract(payload_json, '$.metadata')
             FROM protocol_history_items
             WHERE session_id = ?1
               AND (json_extract(payload_json, '$.kind') = 'tool_output'
                    OR (json_extract(payload_json, '$.kind') = 'tool_call'
                        AND json_extract(payload_json, '$.tool_name') = 'team_delegate'))
             ORDER BY rowid LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![session_id.to_string(), (MAX_TOOL_ROWS + 1) as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > MAX_TOOL_ROWS {
            return Err(StorageError::Message(
                "chat tool history exceeds the remote-work deletion guard limit".into(),
            ));
        }
        let mut delegate_calls = HashSet::new();
        let mut outputs = HashMap::new();
        for (kind, call_id, tool_name, status, metadata) in rows {
            if call_id.parse::<ToolCallId>().is_err() {
                return Err(StorageError::Message(
                    "chat tool history contains an invalid call identity".into(),
                ));
            }
            match kind.as_str() {
                "tool_call" if tool_name.as_deref() == Some("team_delegate") => {
                    if !delegate_calls.insert(call_id) {
                        return Err(StorageError::Message(
                            "chat tool history contains a repeated delegation".into(),
                        ));
                    }
                }
                "tool_output" => {
                    if outputs.insert(call_id, (status, metadata)).is_some() {
                        return Err(StorageError::Message(
                            "chat tool history contains a repeated tool result".into(),
                        ));
                    }
                }
                _ => {}
            }
        }
        let mut job_ids = Vec::new();
        for call_id in delegate_calls {
            let Some((status, metadata)) = outputs.get(&call_id) else {
                return Err(StorageError::Message(
                    "a remote job submission has no durable result".into(),
                ));
            };
            if status.as_deref() != Some("completed") {
                continue;
            }
            let metadata: serde_json::Value = metadata
                .as_deref()
                .ok_or_else(|| StorageError::Message("remote job result is missing".into()))
                .and_then(|value| serde_json::from_str(value).map_err(StorageError::from))?;
            let result_metadata = metadata.get("tool_metadata").unwrap_or(&metadata);
            let job_id = result_metadata
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte))
                })
                .ok_or_else(|| StorageError::Message("remote job ID is missing".into()))?;
            job_ids.push(job_id.to_owned());
        }
        job_ids.sort();
        job_ids.dedup();
        Ok(job_ids)
    }

    pub(crate) fn session_attempted_team_delegation(
        &self,
        session_id: SessionId,
    ) -> Result<bool, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let found: i64 = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM protocol_history_items AS call
                 WHERE call.session_id = ?1
                   AND json_extract(call.payload_json, '$.kind') = 'tool_call'
                   AND json_extract(call.payload_json, '$.tool_name') = 'team_delegate'
                   AND NOT EXISTS (
                       SELECT 1 FROM protocol_history_items AS output
                       WHERE output.session_id = call.session_id
                         AND json_extract(output.payload_json, '$.kind') = 'tool_output'
                         AND json_extract(output.payload_json, '$.call_id') = json_extract(call.payload_json, '$.call_id')
                         AND json_extract(output.payload_json, '$.status') = 'declined'
                   )
            )",
            params![session_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(found != 0)
    }
}

fn checked_origin_revision_rows(
    rows: Vec<(String, Option<i64>)>,
    limit: usize,
) -> Result<Vec<(SessionId, u64)>, StorageError> {
    if rows.len() > limit {
        return Err(StorageError::Message(
            "too many chats to verify their remote work before changing history".into(),
        ));
    }
    rows.into_iter()
        .map(|(session_id, revision)| {
            let id = parse_session_id_text(&session_id, "origin history mutation target")?;
            let revision = revision.ok_or_else(|| {
                StorageError::Message(format!("session {id} has no canonical admission revision"))
            })?;
            let revision = u64::try_from(revision).map_err(|_| {
                StorageError::Message(format!(
                    "session {id} has invalid admission revision {revision}"
                ))
            })?;
            Ok((id, revision))
        })
        .collect()
}

pub(super) fn origin_history_revisions_for_tree_in_connection(
    connection: &Connection,
    session_id: SessionId,
    limit: usize,
) -> Result<Vec<(SessionId, u64)>, StorageError> {
    let sql_limit = i64::try_from(limit.saturating_add(1)).map_err(|_| {
        StorageError::Message("origin history verification limit is too large".into())
    })?;
    let mut statement = connection.prepare(
        "WITH RECURSIVE tree_root(root_session_id) AS (
             SELECT COALESCE(
                 (SELECT root_session_id FROM session_spawn_edges WHERE child_session_id = ?1),
                 ?1
             )
         ), subtree(session_id) AS (
             SELECT id FROM sessions WHERE id = ?1
             UNION
             SELECT edge.child_session_id
             FROM session_spawn_edges AS edge
             INNER JOIN subtree ON edge.parent_session_id = subtree.session_id
             INNER JOIN tree_root ON edge.root_session_id = tree_root.root_session_id
         )
         SELECT subtree.session_id,
                (SELECT revision FROM session_admission_revisions
                 WHERE session_id = subtree.session_id)
         FROM subtree ORDER BY subtree.session_id LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![session_id.to_string(), sql_limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    checked_origin_revision_rows(rows, limit)
}

fn origin_history_revisions_for_project_in_connection(
    connection: &Connection,
    project_id: ProjectId,
    limit: usize,
) -> Result<Vec<(SessionId, u64)>, StorageError> {
    let sql_limit = i64::try_from(limit.saturating_add(1)).map_err(|_| {
        StorageError::Message("origin history verification limit is too large".into())
    })?;
    let mut statement = connection.prepare(
        "SELECT session.id,
                (SELECT revision FROM session_admission_revisions
                 WHERE session_id = session.id)
         FROM sessions AS session
         WHERE session.project_id = ?1 ORDER BY session.id LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![project_id.to_string(), sql_limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    checked_origin_revision_rows(rows, limit)
}

pub(super) fn verify_origin_history_revisions(
    actual: Vec<(SessionId, u64)>,
    expected: &[(SessionId, u64)],
) -> Result<(), StorageError> {
    let mut expected = expected.to_vec();
    expected.sort_unstable_by_key(|(session_id, _)| session_id.to_string());
    if actual != expected {
        return Err(StorageError::Message(
            "chat history changed while Hub work was being checked; refresh and retry".into(),
        ));
    }
    Ok(())
}
