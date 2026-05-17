ALTER TABLE session_state
ADD COLUMN token_accounting_json TEXT NOT NULL DEFAULT '{}';
