pub(crate) fn required_write_content_shape_mismatch_is_nonprogress() -> bool {
    crate::agent::loop_impl::content_shape_mismatch_feedback_carries_positive_test_contract()
        && crate::agent::loop_impl::test_target_content_shape_write_lifecycle_enforced_fixture_passes()
        && crate::agent::loop_impl::corrective_content_shape_no_progress_terminal_guard_fixture_passes()
}
