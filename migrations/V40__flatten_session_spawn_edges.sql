CREATE TABLE session_spawn_edges_v40 (
    root_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    child_session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_path TEXT NOT NULL,
    task_name TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(root_session_id, agent_path),
    CHECK(parent_session_id = root_session_id),
    CHECK(child_session_id <> root_session_id),
    CHECK(task_name <> ''),
    CHECK(task_name <> 'root'),
    CHECK(task_name NOT GLOB '*[^a-z0-9_]*'),
    CHECK(agent_path = '/root/' || task_name)
);

-- Nested lineage cannot be represented truthfully by the flat root namespace. Preserve only
-- already-valid direct edges; sessions whose invalid edge is discarded remain independent
-- durable sessions and are not deleted or silently reparented.
WITH valid_flat_edges AS (
    SELECT
        root_session_id,
        parent_session_id,
        child_session_id,
        agent_path,
        task_name,
        created_at_ms,
        ROW_NUMBER() OVER (
            PARTITION BY root_session_id
            ORDER BY created_at_ms ASC, child_session_id ASC
        ) AS retained_order
    FROM session_spawn_edges
    WHERE parent_session_id = root_session_id
      AND child_session_id <> root_session_id
      AND task_name <> ''
      AND task_name <> 'root'
      AND task_name NOT GLOB '*[^a-z0-9_]*'
      AND agent_path = '/root/' || task_name
)
INSERT INTO session_spawn_edges_v40 (
    root_session_id,
    parent_session_id,
    child_session_id,
    agent_path,
    task_name,
    created_at_ms
)
SELECT
    root_session_id,
    parent_session_id,
    child_session_id,
    agent_path,
    task_name,
    created_at_ms
FROM valid_flat_edges
WHERE retained_order <= 255;

DROP TABLE session_spawn_edges;
ALTER TABLE session_spawn_edges_v40 RENAME TO session_spawn_edges;

CREATE INDEX idx_session_spawn_edges_root_created
    ON session_spawn_edges(root_session_id, created_at_ms, child_session_id);

CREATE INDEX idx_session_spawn_edges_parent_created
    ON session_spawn_edges(parent_session_id, created_at_ms, child_session_id);

-- MAX_RETAINED_AGENTS is 256 including the root, leaving at most 255 direct children.
CREATE TRIGGER limit_session_spawn_edges_per_root
BEFORE INSERT ON session_spawn_edges
WHEN (
    SELECT COUNT(*)
    FROM session_spawn_edges
    WHERE root_session_id = NEW.root_session_id
) >= 255
BEGIN
    SELECT RAISE(ABORT, 'agent tree reached its retained direct-child capacity of 255');
END;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (40, 'flatten_session_spawn_edges');
