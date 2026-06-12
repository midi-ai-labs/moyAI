ALTER TABLE sessions
ADD COLUMN memory_mode TEXT NOT NULL DEFAULT 'enabled'
CHECK (memory_mode IN ('enabled', 'disabled'));
