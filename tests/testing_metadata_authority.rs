#[test]
fn test_metadata_includes_current_preflight_and_manual_st_guards() {
    assert!(
        moyai::harness::preflight::testing_metadata_current_guard_index_fixture_passes(),
        "docs/testing/test-metadata.json must index current active deterministic convergence guards"
    );
}
