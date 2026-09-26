use super::*;

#[test]
fn endpoint_move_preserves_saved_consent_but_invalidates_native_review_and_async_target() {
    let old = format!("hub|device|https://old.example:9471|{}", "a".repeat(64));
    let new = format!("hub|device|https://new.example:9471|{}", "a".repeat(64));
    assert!(desktop_consent_matches(&old, &new));
    let mut state = ExecutionRuntime::default();
    state.bind(&old);
    let mut view = state.view.clone();
    view.review = Some(ExecutionReview {
        id: "review".into(),
        directory: "C:/fixture".into(),
        access_mode: AccessMode::Default,
    });
    state.revise(view);
    let old_revision = state.view.revision.clone();
    state.bind(&new);
    assert_ne!(state.binding, old);
    assert_ne!(state.view.revision, old_revision);
    assert!(state.view.review.is_none());
}

fn executable_project() -> DeviceProject {
    DeviceProject {
        id: "project".into(),
        label: "Shared project".into(),
        can_execute: true,
        preparation_state: "waiting_setup".into(),
        ..Default::default()
    }
}

#[test]
fn pending_setup_failure_remains_visible_across_project_refresh() {
    let mut state = ExecutionRuntime::default();
    state.bind("hub/device/trust");
    let review = ExecutionReview {
        id: "native-review".into(),
        directory: "C:/fixture".into(),
        access_mode: AccessMode::Default,
    };
    let failure = "Could not connect to the execution host";
    state.revise(DeviceExecutionProjection {
        state: DeviceExecutionState::Unavailable,
        review: Some(review.clone()),
        error: Some(failure.into()),
        projects: vec![executable_project()],
        ..Default::default()
    });
    let failed_revision = state.view.revision.clone();

    for _ in 0..2 {
        state.refresh_projects(vec![executable_project()], false);
        assert_eq!(state.view.error.as_deref(), Some(failure));
        assert_eq!(state.view.state, DeviceExecutionState::Unavailable);
        assert_eq!(state.view.review, Some(review.clone()));
        assert_eq!(state.view.revision, failed_revision);
        assert!(!state.view.accepting);
        assert!(state.view.directory.is_none());
    }

    state.bind("another-hub/device/trust");
    state.refresh_projects(vec![executable_project()], false);
    assert_eq!(state.view.state, DeviceExecutionState::NeedsSetup);
    assert!(state.view.error.is_none());
    assert!(state.view.review.is_none());
}

#[test]
fn project_refresh_without_a_pending_setup_recovers_from_a_read_failure() {
    let mut state = ExecutionRuntime::default();
    state.bind("hub/device/trust");
    state.revise(DeviceExecutionProjection {
        state: DeviceExecutionState::Unavailable,
        error: Some("Could not read project configuration".into()),
        ..Default::default()
    });

    state.refresh_projects(vec![executable_project()], false);
    assert_eq!(state.view.state, DeviceExecutionState::NeedsSetup);
    assert!(state.view.error.is_none());
    assert!(!state.view.accepting);
}

#[test]
fn execution_binding_change_discards_review_and_old_unknown_mutation_targets() {
    let mut state = ExecutionRuntime::default();
    state.bind("first-hub/device/trust");
    let mut view = DeviceExecutionProjection::default();
    view.review = Some(ExecutionReview {
        id: "review".into(),
        directory: "C:/fixture".into(),
        access_mode: AccessMode::Default,
    });
    view.unknown_attempts.push(SharedAttemptProjection {
        attempt_id: "attempt".into(),
        generation: 2,
        job_id: "job".into(),
        project_id: "project".into(),
        environment_id: "env".into(),
        run_id: ulid::Ulid::new(),
        state: "unknown".into(),
        local_state: None,
    });
    state.revise(view);
    let revision = state.view.revision.clone();
    state.bind("first-hub/device/trust");
    state.revise(state.view.clone());
    assert_eq!(state.view.revision, revision);
    assert!(state.view.review.is_some());
    state.bind("second-hub/device/trust");
    assert_ne!(state.view.revision, revision);
    assert!(state.view.review.is_none());
    assert!(state.view.unknown_attempts.is_empty());
    assert!(!state.view.accepting);
}

#[test]
fn execution_commands_accept_only_native_review_receipts_and_exact_reconciliation() {
    assert!(
        serde_json::from_value::<DeviceExecutionCommand>(
            serde_json::json!({"kind":"enable","review_id":"r","directory":"C:/arbitrary"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<DeviceExecutionCommand>(serde_json::json!({"kind":"reconcile","attempt_id":"a","reason":"verified","evidence":{"kind":"process_drain"}})).is_err());
    assert!(serde_json::from_value::<DeviceExecutionCommand>(serde_json::json!({"kind":"reconcile","attempt_id":"a","generation":2,"reason":"verified","evidence":{"kind":"operator_confirmed_stopped","effects_reviewed":true,"processes_stopped":true}})).is_ok());
}

#[test]
fn hub_project_refresh_cannot_supply_or_replace_a_local_folder() {
    let mut state = ExecutionRuntime::default();
    state.bind("hub/device/trust");
    let mut current = executable_project();
    current.environment_id = Some("environment-a".into());
    current.directory = Some("C:/real-folder".into());
    current.access_mode = Some(AccessMode::FullAccess);
    state.view.projects = vec![current];

    let mut hub = executable_project();
    hub.environment_id = Some("environment-a".into());
    hub.directory = Some("C:/hub-supplied-folder".into());
    hub.access_mode = Some(AccessMode::Default);
    state.refresh_projects(vec![hub.clone()], true);
    assert_eq!(
        state.view.projects[0].directory.as_deref(),
        Some(camino::Utf8Path::new("C:/real-folder"))
    );
    assert_eq!(
        state.view.projects[0].access_mode,
        Some(AccessMode::FullAccess)
    );

    hub.environment_id = Some("new-participation-environment".into());
    state.refresh_projects(vec![hub], true);
    assert!(state.view.projects[0].directory.is_none());
    assert!(state.view.projects[0].access_mode.is_none());
}
