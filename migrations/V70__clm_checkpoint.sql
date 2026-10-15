-- CLM adds a typed replay snapshot to canonical Compaction JSON. Existing
-- summaries and source rows are preserved; no table or payload rewrite occurs.
INSERT OR IGNORE INTO moyai_schema_migrations(version, name)
VALUES (70, 'clm_checkpoint');
