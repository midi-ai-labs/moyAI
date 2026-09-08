-- Delivery receipts are independent of the canonical job/turn state.
-- No bearer credential is stored; grant_id identifies Hub's durable authority.
CREATE TABLE remote_network_receipts (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES remote_agent_jobs(id) ON DELETE CASCADE,
    grant_id TEXT NOT NULL CHECK(length(grant_id) BETWEEN 1 AND 128),
    settlement_delivered INTEGER NOT NULL DEFAULT 0 CHECK(settlement_delivered IN (0,1)),
    last_attempt_ms INTEGER NOT NULL DEFAULT 0 CHECK(last_attempt_ms>=0)
);
CREATE INDEX remote_network_receipts_pending ON remote_network_receipts(settlement_delivered,last_attempt_ms,job_id);
CREATE TRIGGER remote_network_receipts_immutable
BEFORE UPDATE ON remote_network_receipts
WHEN OLD.job_id IS NOT NEW.job_id OR OLD.grant_id IS NOT NEW.grant_id
    OR OLD.settlement_delivered > NEW.settlement_delivered
BEGIN
    SELECT RAISE(ABORT,'remote network receipt identity is immutable');
END;
INSERT INTO moyai_schema_migrations(version,name) VALUES(64,'remote_network_receipts');
