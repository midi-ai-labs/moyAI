-- Recovery is implemented transactionally by the Rust migration runner because it must
-- recognize only the exact rollback-created orphan shape before removing child rows.
INSERT INTO moyai_schema_migrations(version, name)
VALUES (56, 'rollback_harness_orphan_recovery');
