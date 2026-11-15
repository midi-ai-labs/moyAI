use super::*;

impl SqliteSessionRepository {
    #[cfg(test)]
    pub(crate) async fn insert_session_spawn_edge(
        &self,
        root_session_id: SessionId,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        agent_path: &str,
        task_name: &str,
    ) -> Result<SessionSpawnEdge, StorageError> {
        validate_session_spawn_edge_shape(
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path,
            task_name,
        )
        .map_err(StorageError::Message)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let edge = insert_session_spawn_edge_in_transaction(
            &transaction,
            root_session_id,
            parent_session_id,
            child_session_id,
            agent_path,
            task_name,
        )?;
        transaction.commit()?;
        Ok(edge)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_agent_spawn_with_initial_task_for_caller_turn(
        &self,
        root_session_id: SessionId,
        caller_session_id: SessionId,
        child_session_id: SessionId,
        child_draft: NewSession,
        agent_path: &str,
        task_name: &str,
        caller_admission_id: AdmissionId,
        caller_turn_id: TurnId,
        context_fork: SpawnContextFork,
        initial_task: InterAgentCommunication,
    ) -> Result<StoredAgentSpawn, StorageError> {
        validate_session_spawn_edge_shape(
            root_session_id,
            caller_session_id,
            child_session_id,
            agent_path,
            task_name,
        )
        .map_err(StorageError::Message)?;
        if !initial_task.trigger_turn {
            return Err(StorageError::Message(
                "an initial agent task must trigger the child turn".to_string(),
            ));
        }
        if initial_task.recipient != agent_path {
            return Err(StorageError::Message(format!(
                "initial agent task targets `{}` instead of durable child path `{agent_path}`",
                initial_task.recipient
            )));
        }
        let child_draft = normalize_new_session_draft(child_draft)?;
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());

        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_admission_in_transaction(
            &transaction,
            caller_session_id,
            caller_admission_id,
            caller_turn_id,
        )?;
        let root_session = session_record_from_connection(&transaction, root_session_id)?;
        let caller_session = session_record_from_connection(&transaction, caller_session_id)?;
        validate_agent_child_session_draft(
            &root_session,
            &caller_session,
            &child_draft,
            agent_path,
            task_name,
            &initial_task,
        )?;
        let child_session =
            insert_session_in_transaction(&transaction, child_session_id, &child_draft, now)?;
        let edge = insert_session_spawn_edge_in_transaction(
            &transaction,
            root_session_id,
            caller_session_id,
            child_session_id,
            agent_path,
            task_name,
        )?;
        match context_fork {
            SpawnContextFork::None => {}
            SpawnContextFork::All => {
                fork_agent_context_in_transaction_for_spawn(
                    &transaction,
                    caller_session_id,
                    child_session_id,
                    None,
                )?;
            }
            SpawnContextFork::Recent(turns) => {
                fork_agent_context_in_transaction_for_spawn(
                    &transaction,
                    caller_session_id,
                    child_session_id,
                    Some(turns),
                )?;
            }
        }
        let initial_task_history_item_id = insert_agent_mailbox_message_in_transaction(
            &transaction,
            root_session_id,
            caller_session_id,
            child_session_id,
            initial_task,
            now,
            true,
        )?;
        transaction.commit()?;
        Ok(StoredAgentSpawn {
            child_session,
            edge,
            initial_task_history_item_id,
        })
    }

    pub async fn session_spawn_edge_for_child(
        &self,
        child_session_id: SessionId,
    ) -> Result<Option<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let edge = connection
            .query_row(
                "SELECT root_session_id, parent_session_id, child_session_id,
                        agent_path, task_name, spawn_order, created_at_ms
                 FROM session_spawn_edges
                 WHERE child_session_id = ?1",
                params![child_session_id.to_string()],
                session_spawn_edge_from_row,
            )
            .optional()
            .map_err(StorageError::from)?;
        if let Some(edge) = edge.as_ref() {
            validate_session_spawn_edge_parent(&connection, edge)?;
        }
        Ok(edge)
    }

    pub async fn list_session_spawn_edges(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<SessionSpawnEdge>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT root_session_id, parent_session_id, child_session_id,
                    agent_path, task_name, spawn_order, created_at_ms
             FROM session_spawn_edges
             WHERE root_session_id = ?1
             ORDER BY spawn_order ASC, child_session_id ASC",
        )?;
        let edges = statement
            .query_map(
                params![root_session_id.to_string()],
                session_spawn_edge_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)?;
        validate_session_spawn_edge_tree(&edges)?;
        Ok(edges)
    }

    pub async fn list_session_subtree_ids(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SessionId>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "WITH RECURSIVE tree_root(root_session_id) AS (
                 SELECT COALESCE(
                     (SELECT root_session_id
                      FROM session_spawn_edges
                      WHERE child_session_id = ?1),
                     ?1
                 )
             ),
             subtree(session_id) AS (
                 SELECT session.id
                 FROM sessions AS session
                 WHERE session.id = ?1
                 UNION
                 SELECT edge.child_session_id
                 FROM session_spawn_edges AS edge
                 INNER JOIN subtree
                   ON edge.parent_session_id = subtree.session_id
                 INNER JOIN tree_root
                   ON edge.root_session_id = tree_root.root_session_id
             )
             SELECT subtree.session_id
             FROM subtree
             LEFT JOIN session_spawn_edges AS edge
               ON edge.child_session_id = subtree.session_id
              AND edge.root_session_id = (SELECT root_session_id FROM tree_root)
             ORDER BY
                 CASE WHEN subtree.session_id = ?1 THEN 0 ELSE 1 END,
                 (LENGTH(edge.agent_path) - LENGTH(REPLACE(edge.agent_path, '/', ''))) ASC,
                 edge.spawn_order ASC,
                 subtree.session_id ASC",
        )?;
        statement
            .query_map(params![session_id.to_string()], |row| {
                parse_session_id_column(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Reads every retained descendant and its validated durable runtime state in one SQL
    /// snapshot. The caller may combine this semantic projection with its process-local run
    /// registry without issuing one query per child.
    pub async fn list_descendant_run_admission_states(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<DescendantRunAdmissionState>, StorageError> {
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT edge.root_session_id, edge.parent_session_id, edge.child_session_id,
                     edge.agent_path, edge.task_name, edge.spawn_order, edge.created_at_ms,
                     child.status, child.active_run_id, child.active_turn_id,
                     child.active_run_lease_expires_at_ms,
                     (SELECT COUNT(*)
                      FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = child.id
                        AND terminal_event.turn_id = child.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'),
                     (SELECT terminal_event.msg_json
                      FROM protocol_runtime_events AS terminal_event
                      WHERE terminal_event.session_id = child.id
                        AND terminal_event.turn_id = child.active_turn_id
                        AND json_extract(terminal_event.msg_json, '$.kind') = 'turn_terminal'
                      ORDER BY terminal_event.sequence_no DESC, terminal_event.rowid DESC
                      LIMIT 1)
              FROM session_spawn_edges AS edge
             INNER JOIN sessions AS child ON child.id = edge.child_session_id
             WHERE edge.root_session_id = ?1
             ORDER BY edge.spawn_order ASC, edge.child_session_id ASC",
        )?;
        let rows = statement
            .query_map(params![root_session_id.to_string()], |row| {
                Ok((
                    session_spawn_edge_from_row(row)?,
                    raw_session_runtime_state_from_row(row, 7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        validate_session_spawn_edge_tree(
            &rows
                .iter()
                .map(|(edge, _)| edge.clone())
                .collect::<Vec<_>>(),
        )?;
        rows.into_iter()
            .map(|(edge, raw)| {
                let runtime_state = validate_raw_session_runtime_state(edge.child_session_id, raw)?;
                let admission_is_tree_stop_fenced =
                    runtime_state_admission_started_before_tree_stop_fence(
                        &connection,
                        edge.child_session_id,
                        runtime_state,
                    )?;
                Ok(DescendantRunAdmissionState {
                    edge,
                    blocks_new_root_turn: runtime_state.blocks_mutation_at(now)
                        && !admission_is_tree_stop_fenced,
                })
            })
            .collect()
    }
}

fn session_spawn_edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSpawnEdge> {
    let raw_spawn_order = row.get::<_, i64>(5)?;
    if raw_spawn_order <= 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(5, raw_spawn_order));
    }
    let edge = SessionSpawnEdge {
        root_session_id: parse_session_id_column(row, 0)?,
        parent_session_id: parse_session_id_column(row, 1)?,
        child_session_id: parse_session_id_column(row, 2)?,
        agent_path: row.get(3)?,
        task_name: row.get(4)?,
        spawn_order: raw_spawn_order
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, raw_spawn_order))?,
        created_at_ms: row.get(6)?,
    };
    validate_session_spawn_edge_shape(
        edge.root_session_id,
        edge.parent_session_id,
        edge.child_session_id,
        &edge.agent_path,
        &edge.task_name,
    )
    .map_err(|message| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    })?;
    Ok(edge)
}

fn validate_session_spawn_edge_shape(
    root_session_id: SessionId,
    parent_session_id: SessionId,
    child_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> Result<(), String> {
    if child_session_id == root_session_id {
        return Err(format!(
            "root session {root_session_id} cannot also be its own child session"
        ));
    }
    if child_session_id == parent_session_id {
        return Err(format!(
            "child session {child_session_id} cannot also be its own parent session"
        ));
    }
    AgentPath::root()
        .join(task_name)
        .map_err(|error| format!("invalid task name `{task_name}`: {error}"))?;
    let path = AgentPath::try_from(agent_path)
        .map_err(|error| format!("invalid spawn edge path `{agent_path}`: {error}"))?;
    if path.is_root() || path.name() != task_name {
        return Err(format!(
            "spawn edge path `{agent_path}` does not end with task name `{task_name}`"
        ));
    }
    Ok(())
}

fn insert_session_spawn_edge_in_transaction(
    transaction: &Transaction<'_>,
    root_session_id: SessionId,
    parent_session_id: SessionId,
    child_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> Result<SessionSpawnEdge, StorageError> {
    let next_spawn_order = transaction.query_row(
        "SELECT COALESCE(MAX(spawn_order), 0) + 1
         FROM session_spawn_edges
         WHERE root_session_id = ?1",
        params![root_session_id.to_string()],
        |row| row.get::<_, i64>(0),
    )?;
    let spawn_order = u64::try_from(next_spawn_order).map_err(|_| {
        StorageError::Message("durable agent spawn-order sequence is exhausted".to_string())
    })?;
    let edge = SessionSpawnEdge {
        root_session_id,
        parent_session_id,
        child_session_id,
        agent_path: agent_path.to_string(),
        task_name: task_name.to_string(),
        spawn_order,
        created_at_ms: SystemClock::now_ms(),
    };
    validate_session_spawn_edge_parent(transaction, &edge)?;
    transaction.execute(
        "INSERT INTO session_spawn_edges
         (root_session_id, parent_session_id, child_session_id, agent_path, task_name,
          spawn_order, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            edge.root_session_id.to_string(),
            edge.parent_session_id.to_string(),
            edge.child_session_id.to_string(),
            edge.agent_path,
            edge.task_name,
            i64::try_from(edge.spawn_order).map_err(|_| {
                StorageError::Message(
                    "agent spawn order exceeds the SQLite INTEGER domain".to_string(),
                )
            })?,
            edge.created_at_ms,
        ],
    )?;
    Ok(edge)
}

fn validate_session_spawn_edge_parent(
    connection: &Connection,
    edge: &SessionSpawnEdge,
) -> Result<(), StorageError> {
    validate_session_spawn_edge_shape(
        edge.root_session_id,
        edge.parent_session_id,
        edge.child_session_id,
        &edge.agent_path,
        &edge.task_name,
    )
    .map_err(StorageError::Message)?;
    let projects = connection
        .query_row(
            "SELECT root.project_id, parent.project_id, child.project_id
             FROM sessions AS root
             INNER JOIN sessions AS parent ON parent.id = ?2
             INNER JOIN sessions AS child ON child.id = ?3
             WHERE root.id = ?1",
            params![
                edge.root_session_id.to_string(),
                edge.parent_session_id.to_string(),
                edge.child_session_id.to_string()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::Message(
                "spawn edge root, parent, and child sessions must all exist".to_string(),
            )
        })?;
    if projects.0 != projects.1 || projects.0 != projects.2 {
        return Err(StorageError::Message(format!(
            "spawn edge root {}, parent {}, and child {} must belong to one project",
            edge.root_session_id, edge.parent_session_id, edge.child_session_id
        )));
    }
    let root_is_owned = connection
        .query_row(
            "SELECT 1
             FROM session_spawn_edges
             WHERE child_session_id = ?1",
            params![edge.root_session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if root_is_owned {
        return Err(StorageError::Message(format!(
            "session {} is already a retained descendant and cannot own another agent tree",
            edge.root_session_id
        )));
    }
    let child_owns_tree = connection
        .query_row(
            "SELECT 1
             FROM session_spawn_edges
             WHERE root_session_id = ?1
             LIMIT 1",
            params![edge.child_session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if child_owns_tree {
        return Err(StorageError::Message(format!(
            "session {} already owns an agent tree and cannot become a retained descendant",
            edge.child_session_id
        )));
    }
    let parent_path = if edge.parent_session_id == edge.root_session_id {
        AgentPath::root()
    } else {
        let parent_path = connection
            .query_row(
                "SELECT agent_path
                 FROM session_spawn_edges
                 WHERE root_session_id = ?1 AND child_session_id = ?2",
                params![
                    edge.root_session_id.to_string(),
                    edge.parent_session_id.to_string()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::Message(format!(
                    "spawn parent session {} is not a retained agent in root tree {}",
                    edge.parent_session_id, edge.root_session_id
                ))
            })?;
        AgentPath::try_from(parent_path.as_str()).map_err(|error| {
            StorageError::Message(format!(
                "spawn parent session {} has invalid canonical path `{parent_path}`: {error}",
                edge.parent_session_id
            ))
        })?
    };
    let expected_path = parent_path
        .join(&edge.task_name)
        .map_err(StorageError::Message)?;
    if expected_path.as_str() != edge.agent_path {
        return Err(StorageError::Message(format!(
            "spawn edge path `{}` does not match canonical parent/task path `{expected_path}`",
            edge.agent_path
        )));
    }
    Ok(())
}

fn validate_session_spawn_edge_tree(edges: &[SessionSpawnEdge]) -> Result<(), StorageError> {
    let paths = edges
        .iter()
        .map(|edge| {
            validate_session_spawn_edge_shape(
                edge.root_session_id,
                edge.parent_session_id,
                edge.child_session_id,
                &edge.agent_path,
                &edge.task_name,
            )
            .map_err(StorageError::Message)?;
            AgentPath::try_from(edge.agent_path.as_str())
                .map(|path| ((edge.root_session_id, edge.child_session_id), path))
                .map_err(StorageError::Message)
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    for edge in edges {
        let parent_path = if edge.parent_session_id == edge.root_session_id {
            AgentPath::root()
        } else {
            paths
                .get(&(edge.root_session_id, edge.parent_session_id))
                .cloned()
                .ok_or_else(|| {
                    StorageError::Message(format!(
                        "spawn parent session {} is not retained in root tree {}",
                        edge.parent_session_id, edge.root_session_id
                    ))
                })?
        };
        let expected_path = parent_path
            .join(&edge.task_name)
            .map_err(StorageError::Message)?;
        if expected_path.as_str() != edge.agent_path {
            return Err(StorageError::Message(format!(
                "spawn edge path `{}` does not match canonical parent/task path `{expected_path}`",
                edge.agent_path
            )));
        }
    }
    Ok(())
}
