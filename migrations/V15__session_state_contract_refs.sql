ALTER TABLE session_state
ADD COLUMN contract_refs_json TEXT NOT NULL DEFAULT '[]';
