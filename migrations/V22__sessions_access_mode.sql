ALTER TABLE sessions
ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'default'
CHECK (access_mode IN ('default', 'auto_review', 'full_access'));
