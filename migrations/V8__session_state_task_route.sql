ALTER TABLE session_state
ADD COLUMN task_route TEXT NOT NULL DEFAULT 'code';
