CREATE INDEX idx_tool_calls_truncated_output_path
    ON tool_calls(truncated_output_path)
    WHERE truncated_output_path IS NOT NULL;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (43, 'indexed_internal_file_ownership');
