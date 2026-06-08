#[test]
fn manual_st_reference_exports_do_not_preserve_scope_shrink_wording() {
    assert!(
        moyai::harness::preflight::manual_st_reference_exports_scope_hygiene_fixture_passes(),
        "manual_ST spec/reference surfaces must not preserve scope-shrinking wording as authority"
    );
}
