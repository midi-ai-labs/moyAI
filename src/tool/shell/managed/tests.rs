use super::*;

fn owner() -> Owner {
    Owner {
        workspace: "fixture".into(),
        authority: Authority::Local(SessionId::new()),
    }
}
fn stopped_output() -> CommandOutput {
    CommandOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        timed_out: false,
        cancelled: true,
        effect_started: true,
        cleanup_failed: false,
        stdout_truncated: false,
        stderr_truncated: false,
    }
}

impl ManagedShells {
    pub(crate) async fn start_preview_for_test(&self, root: SessionId) -> RetainedService {
        let (mut snapshot, _) = self
            .start_with_retention(
                Owner {
                    workspace: "fixture".into(),
                    authority: Authority::Local(root),
                },
                "controlled preview".into(),
                "fixture".into(),
                60_000,
                json!(null),
                CancellationToken::new(),
                false,
                true,
                |cancel, started| async move {
                    started(1);
                    cancel.cancelled().await;
                    Ok(stopped_output())
                },
            )
            .unwrap();
        while snapshot.borrow_and_update().state == State::Starting {
            snapshot.changed().await.unwrap();
        }
        assert_eq!(snapshot.borrow().state, State::Running);
        self.completion_service_in_scope(root, self.receiver_scope_id.expect("receiver scope"))
            .expect("finite preview")
    }
}

#[tokio::test]
async fn terminal_snapshot_is_not_a_live_retained_server_while_worker_finishes() {
    let registry = ManagedShells::default().with_lifetime(CancellationToken::new(), Ulid::new());
    let service = registry.start_preview_for_test(SessionId::new()).await;
    let mut terminal = {
        let records = registry.inner.lock().unwrap();
        let record = &records.records[&service.service_id];
        assert!(!record.worker.is_finished());
        record.snapshot.borrow().clone()
    };
    assert_eq!(
        classify_retained_state(false, false, &terminal),
        RetainedServiceState::Running
    );
    terminal.state = State::Completed;
    assert_eq!(
        classify_retained_state(false, false, &terminal),
        RetainedServiceState::Stopped
    );
    assert!(registry.cancel_retained_service(service.service_id));
    registry.shutdown().await;
}

fn start_waiter(
    registry: &ManagedShells,
    owner: Owner,
    parent: CancellationToken,
) -> watch::Receiver<Snapshot> {
    registry
        .start(
            owner,
            "controlled pending work".into(),
            "fixture".into(),
            60000,
            json!(null),
            parent,
            |cancel, started| async move {
                if !cancel.is_cancelled() {
                    started(1);
                }
                cancel.cancelled().await;
                Ok(stopped_output())
            },
        )
        .unwrap()
        .0
}

async fn terminal(receiver: &mut watch::Receiver<Snapshot>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if receiver.borrow_and_update().state.terminal() {
                return;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .expect("worker must settle");
}

#[tokio::test]
async fn delegation_retains_only_one_explicit_finite_running_service() {
    let registry = ManagedShells::default();
    let owner = owner();
    let session = match &owner.authority {
        Authority::Local(session) => *session,
        _ => unreachable!(),
    };
    let (mut service, _) = registry
        .start_with_retention(
            owner.clone(),
            "test server".into(),
            "fixture".into(),
            60_000,
            json!(null),
            CancellationToken::new(),
            true,
            false,
            |cancel, started| async move {
                started(1);
                cancel.cancelled().await;
                Ok(stopped_output())
            },
        )
        .unwrap();
    while service.borrow_and_update().state == State::Starting {
        service.changed().await.unwrap();
    }
    let retained = registry.delegable_service(session).expect("marked service");
    assert_eq!(retained.service_id, service.borrow().process_id);
    assert!(registry.retained_service_live(retained));
    let mut ordinary = start_waiter(&registry, owner, CancellationToken::new());
    while ordinary.borrow_and_update().state == State::Starting {
        ordinary.changed().await.unwrap();
    }
    assert!(registry.delegable_service(session).is_none());
    let ordinary_id = ordinary.borrow().process_id;
    let ordinary_cancel = registry.inner.lock().unwrap().records[&ordinary_id]
        .cancel
        .clone();
    ordinary_cancel.cancel();
    terminal(&mut ordinary).await;
    assert_eq!(registry.delegable_service(session), Some(retained));
    assert!(registry.cancel_retained_service(retained.service_id));
    terminal(&mut service).await;
    assert!(!registry.retained_service_live(retained));
    assert!(registry.delegable_service(session).is_none());
    registry.shutdown().await;
}

#[tokio::test]
async fn finite_preview_is_visible_only_to_its_exact_receiver_scope() {
    let registry = ManagedShells::default();
    let scope = Ulid::new();
    let other_scope = Ulid::new();
    let scoped = registry.with_lifetime(CancellationToken::new(), scope);
    let owner = owner();
    let session = match &owner.authority {
        Authority::Local(session) => *session,
        _ => unreachable!(),
    };
    let (mut preview, _) = scoped
        .start_with_retention(
            owner,
            "preview server".into(),
            "fixture".into(),
            60_000,
            json!(null),
            CancellationToken::new(),
            false,
            true,
            |cancel, started| async move {
                started(1);
                cancel.cancelled().await;
                Ok(stopped_output())
            },
        )
        .unwrap();
    while preview.borrow_and_update().state == State::Starting {
        preview.changed().await.unwrap();
    }
    let service = registry
        .completion_service_in_scope(session, scope)
        .expect("explicit finite preview");
    assert_eq!(service.service_id, preview.borrow().process_id);
    assert!(service.retain_after_turn);
    assert!(registry.has_local_work_in_scope(session, scope));
    assert!(!registry.has_local_work_in_scope(session, other_scope));
    assert!(
        registry
            .completion_service_in_scope(session, other_scope)
            .is_none()
    );
    assert!(registry.cancel_retained_service(service.service_id));
    terminal(&mut preview).await;
    assert!(!registry.has_local_work_in_scope(session, scope));
    registry.shutdown().await;
}

#[tokio::test]
async fn managed_owner_isolates_local_sessions_workspaces_and_remote_principals() {
    let registry = ManagedShells::default();
    let local = owner();
    let receiver = start_waiter(&registry, local.clone(), CancellationToken::new());
    let id = receiver.borrow().process_id;
    assert!(registry.lookup(&local, id).is_ok());
    assert!(registry.lookup(&owner(), id).is_err());
    let mut other_workspace = local.clone();
    other_workspace.workspace = "other".into();
    assert!(registry.lookup(&other_workspace, id).is_err());
    assert!(registry.lookup(&local, Ulid::new()).is_err());
    let remote = Owner {
        workspace: "fixture".into(),
        authority: Authority::Remote {
            profile: Ulid::new(),
            principal: "authenticated-caller".into(),
            origin: Some("device-a".into()),
            task: "root-a".into(),
        },
    };
    let receiver = start_waiter(&registry, remote.clone(), CancellationToken::new());
    let remote_id = receiver.borrow().process_id;
    assert!(registry.lookup(&remote, remote_id).is_ok());
    for different in ["profile", "principal", "origin", "task"] {
        let mut stranger = remote.clone();
        if let Authority::Remote {
            profile,
            principal,
            origin,
            task,
        } = &mut stranger.authority
        {
            match different {
                "profile" => *profile = Ulid::new(),
                "principal" => *principal = "other".into(),
                "origin" => *origin = Some("other".into()),
                _ => *task = "other".into(),
            }
        }
        assert!(registry.lookup(&stranger, remote_id).is_err());
    }
    registry.shutdown().await;
}

#[tokio::test]
async fn managed_profile_and_lineage_revocation_stop_only_matching_owned_work() {
    let registry = ManagedShells::default();
    let profile = Ulid::new();
    let make_owner = |origin: &str, task: &str| Owner {
        workspace: "fixture".into(),
        authority: Authority::Remote {
            profile,
            principal: "caller".into(),
            origin: Some(origin.into()),
            task: task.into(),
        },
    };
    let mut a = start_waiter(&registry, make_owner("a", "root"), CancellationToken::new());
    let b_owner = make_owner("b", "root");
    let mut b = start_waiter(&registry, b_owner.clone(), CancellationToken::new());
    registry.cancel_lineages(&[("a".into(), "root".into())]);
    terminal(&mut a).await;
    assert_eq!(a.borrow().state, State::Cancelled);
    assert!(
        !registry
            .lookup(&b_owner, b.borrow().process_id)
            .unwrap()
            .1
            .is_cancelled()
    );
    assert!(registry.has_profile_work(profile));
    registry.cancel_profile(profile);
    terminal(&mut b).await;
    registry.shutdown().await;
    assert!(!registry.has_profile_work(profile));
}

#[tokio::test]
async fn managed_capacity_shutdown_and_parent_cancellation_preserve_worker_ownership() {
    let registry = ManagedShells::default();
    let original = owner();
    let parent = CancellationToken::new();
    let mut first = start_waiter(&registry, original.clone(), parent.clone());
    let mut rest = Vec::new();
    for _ in 1..MAX_ACTIVE {
        rest.push(start_waiter(
            &registry,
            original.clone(),
            CancellationToken::new(),
        ));
    }
    let excess = registry.start(
        original.clone(),
        "not started".into(),
        "fixture".into(),
        1,
        json!(null),
        CancellationToken::new(),
        |_, _| async { panic!("capacity must reject before invoking worker") },
    );
    assert!(excess.is_err());
    parent.cancel();
    terminal(&mut first).await;
    assert_eq!(first.borrow().state, State::Cancelled);
    registry.begin_shutdown();
    let rejected = registry.start(
        original,
        "not started".into(),
        "fixture".into(),
        1,
        json!(null),
        CancellationToken::new(),
        |_, _| async { panic!("shutdown must reject before invoking worker") },
    );
    assert!(rejected.is_err());
    registry.shutdown().await;
    for receiver in rest {
        assert_eq!(receiver.borrow().state, State::Cancelled);
    }
    registry.shutdown().await;
}

#[tokio::test]
async fn managed_cancelled_start_handoff_never_leaves_an_unclaimed_worker() {
    let registry = ManagedShells::default();
    let (mut receiver, cancel) = registry
        .start(
            owner(),
            "pending setup".into(),
            "fixture".into(),
            60000,
            json!(null),
            CancellationToken::new(),
            |cancel, _| async move {
                cancel.cancelled().await;
                let mut output = stopped_output();
                output.effect_started = false;
                Ok(output)
            },
        )
        .unwrap();
    drop(StartHandoff {
        cancel,
        committed: false,
    });
    terminal(&mut receiver).await;
    assert_eq!(receiver.borrow().state, State::Cancelled);
    assert!(receiver.borrow().pid.is_none());
    assert_eq!(
        receiver.borrow().result.as_ref().unwrap().effect_started,
        false
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn managed_revocation_between_registration_and_spawn_cannot_escape_a_completed_turn() {
    let registry = ManagedShells::default();
    let scope = CancellationToken::new();
    let receiver_registry = registry.with_lifetime(scope.child_token(), Ulid::new());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (mut result, _) = receiver_registry
        .start(
            owner(),
            "delayed spawn".into(),
            "fixture".into(),
            60000,
            json!(null),
            CancellationToken::new(),
            move |cancel, _started| async move {
                entered_tx.send(()).unwrap();
                release_rx.await.unwrap();
                // Equivalent to the common shell runner's check immediately before spawning.
                assert!(cancel.is_cancelled());
                let mut output = stopped_output();
                output.effect_started = false;
                Ok(output)
            },
        )
        .unwrap();
    entered_rx.await.unwrap();
    scope.cancel();
    release_tx.send(()).unwrap();
    terminal(&mut result).await;
    assert_eq!(result.borrow().state, State::Cancelled);
    assert!(result.borrow().pid.is_none());
    let attempted = receiver_registry.start(
        owner(),
        "later call".into(),
        "fixture".into(),
        60000,
        json!(null),
        CancellationToken::new(),
        |_, _| async { panic!("revoked authority cannot start") },
    );
    assert!(attempted.is_err());
    registry.shutdown().await;
}
