use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::protocol::{TurnId, TurnInterruptionCause};
use crate::session::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCancellationCause {
    Interruption(TurnInterruptionCause),
    Superseded,
    Failure(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCancelOutcome {
    Applied,
    Deferred(RunCancelDeferral),
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCancelDeferral {
    pub primary: RunReservationKind,
    pub secondary: Option<RunReservationKind>,
}

impl RunCancelDeferral {
    fn single(primary: RunReservationKind) -> Self {
        Self {
            primary,
            secondary: None,
        }
    }

    pub fn is_success_commit_only(self) -> bool {
        self.primary == RunReservationKind::SuccessCommit
            && self
                .secondary
                .is_none_or(|kind| kind == RunReservationKind::SuccessCommit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunReservationKind {
    SuccessCommit,
    ToolEffectAdmission,
    ToolEffectCommit,
    ToolSettlement,
}

#[derive(Clone, Debug)]
pub struct RunControl {
    inner: Arc<RunControlInner>,
}

#[derive(Debug)]
struct RunControlInner {
    wake: CancellationToken,
    classification: Mutex<RunClassification>,
    terminal_router: Mutex<Option<RunTerminalRouter>>,
    root_admission: Mutex<RootAdmissionState>,
    root_admission_activity: watch::Sender<u64>,
}

/// Opaque identity for one pre-database root admission attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionPlanId(u64);

/// Exact root admission identity published before the database call starts.
///
/// The durable revision is intentionally absent until the database accepts the admission. The
/// plan id prevents a delayed guard from settling a later attempt for the same session and turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionPlan {
    pub(crate) plan_id: RootAdmissionPlanId,
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: TurnId,
}

/// Exact durable identity returned by a successful root admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionReceipt {
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: TurnId,
    pub(crate) revision: u64,
}

/// Opaque identity for one single-flight root Stop seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionStopSealId(u64);

/// Immutable root-admission state captured at the Stop linearization point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionStopPlan {
    pub(crate) seal_id: RootAdmissionStopSealId,
    pub(crate) last_admitted: Option<RootAdmissionReceipt>,
    pub(crate) pending: Option<RootAdmissionPlan>,
}

/// Read-only canonical owner projection for non-mutating routing decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootAdmissionSnapshot {
    pub(crate) last_admitted: Option<RootAdmissionReceipt>,
    pub(crate) pending: Option<RootAdmissionPlan>,
    pub(crate) stop_seal_id: Option<RootAdmissionStopSealId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionStopSealOutcome {
    Acquired(RootAdmissionStopPlan),
    AlreadySealed,
    RejectedClosed,
    SequenceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionSettlement {
    Admitted(RootAdmissionReceipt),
    Aborted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionStopResolution {
    Continue,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionBeginError {
    RunClosed,
    StopSealed,
    AdmissionPending,
    SessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    SequenceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionSettleError {
    StalePlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootAdmissionWaitError {
    StaleStopPlan,
    NoPendingAdmission,
    SettlementLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RootAdmissionStopLease {
    plan: RootAdmissionStopPlan,
    pending_settlement: Option<RootAdmissionSettlement>,
    committed: bool,
}

#[derive(Debug)]
struct RootAdmissionState {
    next_plan_id: u64,
    next_stop_seal_id: u64,
    pending: Option<RootAdmissionPlan>,
    last_admitted: Option<RootAdmissionReceipt>,
    stop_lease: Option<RootAdmissionStopLease>,
}

impl Default for RootAdmissionState {
    fn default() -> Self {
        Self {
            next_plan_id: 1,
            next_stop_seal_id: 1,
            pending: None,
            last_admitted: None,
            stop_lease: None,
        }
    }
}

/// RAII owner for one exact pre-database root admission.
///
/// Dropping an unsettled guard aborts only its own plan and wakes an exact Stop waiter.
#[must_use = "a root admission must be committed after the database accepts it or aborted"]
#[derive(Debug)]
pub(crate) struct RootAdmissionGuard {
    control: RunControl,
    plan: RootAdmissionPlan,
    settled: bool,
}

pub(crate) type RunTerminalRoute = dyn Fn(&RunControl, RunTerminalRouteKind, RunCancellationCause) -> Option<RunCancelOutcome>
    + Send
    + Sync
    + 'static;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunTerminalRouteKind {
    Request,
    ResolveSuccessCommitAuthoritatively,
    AbandonSuccessCommit,
    ReleaseSuccessCommit,
}

#[derive(Clone)]
struct RunTerminalRouter(Weak<RunTerminalRoute>);

impl fmt::Debug for RunTerminalRouter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RunTerminalRouter(..)")
    }
}

#[derive(Debug, Default)]
enum RunClassification {
    #[default]
    Open,
    SuccessCommitting {
        pending_cause: Option<RunCancellationCause>,
    },
    EffectAdmitting {
        pending_cause: Option<RunCancellationCause>,
    },
    EffectCommitting {
        pending_cause: Option<RunCancellationCause>,
    },
    ToolSettling {
        pending_cause: Option<RunCancellationCause>,
    },
    SuccessSealed,
    Cancelled(RunCancellationCause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancellationPlan {
    Apply,
    Defer(RunReservationKind),
}

#[must_use = "a success commit reservation must be sealed or released"]
pub struct SuccessCommitReservation {
    control: RunControl,
    resolved: bool,
}

#[must_use = "a tool effect admission must be resolved before the tool body starts"]
pub struct ToolEffectAdmissionReservation {
    control: RunControl,
    resolved: bool,
}

#[must_use = "a tool effect commit reservation must be held through commit or rollback"]
#[derive(Debug)]
pub struct ToolEffectCommitReservation {
    control: RunControl,
    released: bool,
}

#[must_use = "a tool settlement reservation must be held through the durable commit"]
#[derive(Debug)]
pub struct ToolSettlementReservation {
    control: RunControl,
    released: bool,
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

impl RunControl {
    pub fn new() -> Self {
        let (root_admission_activity, _) = watch::channel(0);
        Self {
            inner: Arc::new(RunControlInner {
                wake: CancellationToken::new(),
                classification: Mutex::new(RunClassification::default()),
                terminal_router: Mutex::new(None),
                root_admission: Mutex::new(RootAdmissionState::default()),
                root_admission_activity,
            }),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.inner.wake.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.wake.is_cancelled()
    }

    pub fn cause(&self) -> Option<RunCancellationCause> {
        match &*self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            RunClassification::Cancelled(cause) => Some(cause.clone()),
            RunClassification::Open
            | RunClassification::SuccessCommitting { .. }
            | RunClassification::EffectAdmitting { .. }
            | RunClassification::EffectCommitting { .. }
            | RunClassification::ToolSettling { .. }
            | RunClassification::SuccessSealed => None,
        }
    }

    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Publishes one exact root admission before its database call starts.
    ///
    /// Admission publication and terminal classification share the classification lock order. A
    /// cancellation or sealed success that wins first rejects publication; publication that wins
    /// first remains visible to a later Stop snapshot until its guard settles.
    pub(crate) fn begin_root_admission(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<RootAdmissionGuard, RootAdmissionBeginError> {
        let classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return Err(RootAdmissionBeginError::RunClosed);
        }

        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.stop_lease.is_some() {
            return Err(RootAdmissionBeginError::StopSealed);
        }
        if admission.pending.is_some() {
            return Err(RootAdmissionBeginError::AdmissionPending);
        }
        if let Some(last_admitted) = admission.last_admitted {
            if last_admitted.session_id != session_id {
                return Err(RootAdmissionBeginError::SessionMismatch {
                    expected: last_admitted.session_id,
                    actual: session_id,
                });
            }
        }
        let Some(next_plan_id) = admission.next_plan_id.checked_add(1) else {
            return Err(RootAdmissionBeginError::SequenceExhausted);
        };
        let plan = RootAdmissionPlan {
            plan_id: RootAdmissionPlanId(admission.next_plan_id),
            session_id,
            turn_id,
        };
        admission.next_plan_id = next_plan_id;
        admission.pending = Some(plan);
        drop(admission);
        drop(classification);

        Ok(RootAdmissionGuard {
            control: self.clone(),
            plan,
            settled: false,
        })
    }

    /// Returns the current canonical root admission owner without acquiring a Stop seal.
    pub(crate) fn root_admission_snapshot(&self) -> RootAdmissionSnapshot {
        let admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RootAdmissionSnapshot {
            last_admitted: admission.last_admitted,
            pending: admission.pending,
            stop_seal_id: admission.stop_lease.map(|lease| lease.plan.seal_id),
        }
    }

    pub(crate) fn root_admission_stop_is_sealed(&self) -> bool {
        self.inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stop_lease
            .is_some()
    }

    /// Acquires the one Stop coordinator lease and snapshots all finite durable candidates.
    pub(crate) fn seal_root_admission_for_stop(&self) -> RootAdmissionStopSealOutcome {
        let classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.stop_lease.is_some() {
            return RootAdmissionStopSealOutcome::AlreadySealed;
        }
        if matches!(
            *classification,
            RunClassification::SuccessSealed | RunClassification::Cancelled(_)
        ) {
            return RootAdmissionStopSealOutcome::RejectedClosed;
        }
        let Some(next_stop_seal_id) = admission.next_stop_seal_id.checked_add(1) else {
            return RootAdmissionStopSealOutcome::SequenceExhausted;
        };
        let plan = RootAdmissionStopPlan {
            seal_id: RootAdmissionStopSealId(admission.next_stop_seal_id),
            last_admitted: admission.last_admitted,
            pending: admission.pending,
        };
        admission.next_stop_seal_id = next_stop_seal_id;
        admission.stop_lease = Some(RootAdmissionStopLease {
            plan,
            pending_settlement: None,
            committed: false,
        });
        RootAdmissionStopSealOutcome::Acquired(plan)
    }

    /// Waits for the exact pending admission captured by one still-owned Stop plan.
    ///
    /// The watch subscription is created before inspecting state, so settlement cannot be missed.
    /// The settlement remains stored in the Stop lease until exact release, allowing delayed or
    /// multiple waiters to observe the same result without polling or sleeping.
    pub(crate) async fn wait_root_admission_settlement(
        &self,
        stop_plan: RootAdmissionStopPlan,
    ) -> Result<RootAdmissionSettlement, RootAdmissionWaitError> {
        let expected_pending = stop_plan
            .pending
            .ok_or(RootAdmissionWaitError::NoPendingAdmission)?;
        let mut activity = self.inner.root_admission_activity.subscribe();

        loop {
            {
                let admission = self
                    .inner
                    .root_admission
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(stop_lease) = admission.stop_lease else {
                    return Err(RootAdmissionWaitError::StaleStopPlan);
                };
                if stop_lease.plan != stop_plan {
                    return Err(RootAdmissionWaitError::StaleStopPlan);
                }
                if let Some(settlement) = stop_lease.pending_settlement {
                    return Ok(settlement);
                }
                if admission.pending != Some(expected_pending) {
                    return Err(RootAdmissionWaitError::SettlementLost);
                }
            }

            activity
                .changed()
                .await
                .expect("RunControl retains its root-admission activity sender");
        }
    }

    /// Holds post-admission execution behind any Stop seal that already owns this root control.
    ///
    /// Durable Stop success is published through the outer cancellation token and returns
    /// `Cancelled`. Exact seal release after a target-change result returns `Continue`. If a new
    /// Stop seal wins immediately after a release, execution remains blocked behind that newer
    /// owner as well.
    pub(crate) async fn wait_for_root_admission_stop_resolution(
        &self,
    ) -> RootAdmissionStopResolution {
        let mut activity = self.inner.root_admission_activity.subscribe();
        loop {
            let stop_state = self
                .inner
                .root_admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .stop_lease
                .map(|lease| lease.committed);
            if stop_state == Some(true) || self.is_cancelled() {
                return RootAdmissionStopResolution::Cancelled;
            }
            if stop_state.is_none() {
                return RootAdmissionStopResolution::Continue;
            }

            tokio::select! {
                _ = self.inner.wake.cancelled() => {
                    return RootAdmissionStopResolution::Cancelled;
                }
                changed = activity.changed() => {
                    changed.expect("RunControl retains its root-admission activity sender");
                }
            }
        }
    }

    /// Commits only the exact root-admission Stop lease and wakes its post-admission waiter.
    ///
    /// This deliberately does not classify the stable outer `RunControl`: ordinary User Stop
    /// closes future root admissions but must not make independently-owned descendants inherit a
    /// tree-wide cancellation. The current root turn is classified separately by its exact local
    /// owner.
    pub(crate) fn commit_root_admission_stop_seal(
        &self,
        expected_seal_id: RootAdmissionStopSealId,
    ) -> bool {
        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(stop_lease) = admission.stop_lease.as_mut() else {
            return false;
        };
        if stop_lease.plan.seal_id != expected_seal_id {
            return false;
        }
        stop_lease.committed = true;
        drop(admission);
        self.notify_root_admission_activity();
        true
    }

    /// Releases only the currently-owned Stop seal. A delayed release cannot open a newer seal.
    pub(crate) fn release_root_admission_stop_seal(
        &self,
        expected_seal_id: RootAdmissionStopSealId,
    ) -> bool {
        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission
            .stop_lease
            .is_none_or(|lease| lease.plan.seal_id != expected_seal_id || lease.committed)
        {
            return false;
        }
        admission.stop_lease = None;
        drop(admission);
        self.notify_root_admission_activity();
        true
    }

    fn commit_root_admission(
        &self,
        expected_plan: RootAdmissionPlan,
        revision: u64,
    ) -> Result<RootAdmissionReceipt, RootAdmissionSettleError> {
        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.pending != Some(expected_plan) {
            return Err(RootAdmissionSettleError::StalePlan);
        }
        // The database result is authoritative once admission commits. Even an unexpected
        // revision must be published to Stop; rejecting it here would leave a durable B hidden
        // behind the older process-local A receipt.
        let receipt = RootAdmissionReceipt {
            session_id: expected_plan.session_id,
            turn_id: expected_plan.turn_id,
            revision,
        };
        admission.pending = None;
        admission.last_admitted = Some(receipt);
        if let Some(stop_lease) = &mut admission.stop_lease {
            if stop_lease.plan.pending == Some(expected_plan) {
                stop_lease.pending_settlement = Some(RootAdmissionSettlement::Admitted(receipt));
            }
        }
        drop(admission);
        self.notify_root_admission_activity();
        Ok(receipt)
    }

    fn abort_root_admission(
        &self,
        expected_plan: RootAdmissionPlan,
    ) -> Result<(), RootAdmissionSettleError> {
        let mut admission = self
            .inner
            .root_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if admission.pending != Some(expected_plan) {
            return Err(RootAdmissionSettleError::StalePlan);
        }
        admission.pending = None;
        if let Some(stop_lease) = &mut admission.stop_lease {
            if stop_lease.plan.pending == Some(expected_plan) {
                stop_lease.pending_settlement = Some(RootAdmissionSettlement::Aborted);
            }
        }
        drop(admission);
        self.notify_root_admission_activity();
        Ok(())
    }

    fn notify_root_admission_activity(&self) {
        self.inner
            .root_admission_activity
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    /// Classifies two run owners as one logical terminal action.
    ///
    /// Both classifications are locked in address order and are changed only when both can accept
    /// the requested cause. This is used when a child permission Abort must claim the root and the
    /// requesting child together; a competing Stop or failure on either owner therefore wins the
    /// whole action instead of leaving a half-classified tree.
    pub fn request_linked_cancellation(
        primary: &Self,
        primary_cause: RunCancellationCause,
        secondary: &Self,
        secondary_cause: RunCancellationCause,
    ) -> RunCancelOutcome {
        if primary.same_owner(secondary) {
            if primary_cause != secondary_cause {
                return RunCancelOutcome::Rejected;
            }
            let mut classification = primary
                .inner
                .classification
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(plan) = cancellation_plan(&classification) else {
                return RunCancelOutcome::Rejected;
            };
            let wake = apply_cancellation_plan(&mut classification, primary_cause, plan);
            drop(classification);
            if wake {
                primary.inner.wake.cancel();
                RunCancelOutcome::Applied
            } else {
                let CancellationPlan::Defer(kind) = plan else {
                    unreachable!("a non-waking cancellation plan must be deferred");
                };
                RunCancelOutcome::Deferred(RunCancelDeferral::single(kind))
            }
        } else {
            let primary_address = Arc::as_ptr(&primary.inner) as usize;
            let secondary_address = Arc::as_ptr(&secondary.inner) as usize;
            let (outcome, wake_primary, wake_secondary) = if primary_address < secondary_address {
                let mut primary_state = primary
                    .inner
                    .classification
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut secondary_state = secondary
                    .inner
                    .classification
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                linked_cancellation_locked(
                    &mut primary_state,
                    primary_cause,
                    &mut secondary_state,
                    secondary_cause,
                )
            } else {
                let mut secondary_state = secondary
                    .inner
                    .classification
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut primary_state = primary
                    .inner
                    .classification
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                linked_cancellation_locked(
                    &mut primary_state,
                    primary_cause,
                    &mut secondary_state,
                    secondary_cause,
                )
            };
            if wake_primary {
                primary.inner.wake.cancel();
            }
            if wake_secondary {
                secondary.inner.wake.cancel();
            }
            outcome
        }
    }

    /// Reserves the short boundary between an accepted permission decision and the first tool
    /// effect. Competing terminal causes are deferred only until [`ToolEffectAdmissionReservation::admit`].
    pub fn begin_tool_effect_admission(&self) -> Option<ToolEffectAdmissionReservation> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return None;
        }
        *classification = RunClassification::EffectAdmitting {
            pending_cause: None,
        };
        Some(ToolEffectAdmissionReservation {
            control: self.clone(),
            resolved: false,
        })
    }

    /// Linearizes a tool's durable terminal commit against Stop, Abort, failure, and
    /// supersession. A competing cause is deferred until this short reservation is released.
    pub fn begin_tool_settlement(&self) -> Option<ToolSettlementReservation> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return None;
        }
        *classification = RunClassification::ToolSettling {
            pending_cause: None,
        };
        Some(ToolSettlementReservation {
            control: self.clone(),
            released: false,
        })
    }

    /// Protects the short filesystem-mutation plus durable-evidence commit window. Cancellation
    /// remains cooperative during preparation and formatter execution, but a terminal producer
    /// that arrives after this reservation is acquired is published only after commit or rollback
    /// restores a consistent state.
    pub fn begin_tool_effect_commit(&self) -> Option<ToolEffectCommitReservation> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return None;
        }
        *classification = RunClassification::EffectCommitting {
            pending_cause: None,
        };
        Some(ToolEffectCommitReservation {
            control: self.clone(),
            released: false,
        })
    }

    pub fn begin_success_commit(&self) -> Option<SuccessCommitReservation> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return None;
        }
        *classification = RunClassification::SuccessCommitting {
            pending_cause: None,
        };
        Some(SuccessCommitReservation {
            control: self.clone(),
            resolved: false,
        })
    }

    pub fn seal_success(&self) -> bool {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*classification, RunClassification::Open) {
            *classification = RunClassification::SuccessSealed;
            true
        } else {
            false
        }
    }

    pub fn success_is_sealed(&self) -> bool {
        matches!(
            *self
                .inner
                .classification
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            RunClassification::SuccessSealed
        )
    }

    /// Records the first terminal cause and wakes all token-based consumers.
    ///
    /// Later competing cancellation producers cannot overwrite the classification chosen by the
    /// first producer.
    pub fn cancel(&self, cause: RunCancellationCause) -> bool {
        self.request_cancel(cause) == RunCancelOutcome::Applied
    }

    pub fn request_cancel(&self, cause: RunCancellationCause) -> RunCancelOutcome {
        if let Some(outcome) = self.route_terminal(RunTerminalRouteKind::Request, cause.clone()) {
            return outcome;
        }
        self.request_cancel_local(cause)
    }

    fn route_terminal(
        &self,
        kind: RunTerminalRouteKind,
        cause: RunCancellationCause,
    ) -> Option<RunCancelOutcome> {
        let router = self
            .inner
            .terminal_router
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        router
            .and_then(|router| router.0.upgrade())
            .and_then(|router| router(self, kind, cause))
    }

    /// Applies a classification to this exact run owner without invoking its root-scoped
    /// terminal router. AgentControl uses this only while it is already classifying the whole
    /// tree, which prevents recursive routing back into `fail_tree`.
    pub(crate) fn request_cancel_local(&self, cause: RunCancellationCause) -> RunCancelOutcome {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &mut *classification {
            RunClassification::Open => {
                *classification = RunClassification::Cancelled(cause);
                drop(classification);
                self.inner.wake.cancel();
                RunCancelOutcome::Applied
            }
            RunClassification::SuccessCommitting { pending_cause } => match pending_cause {
                None => {
                    *pending_cause = Some(cause);
                    RunCancelOutcome::Deferred(RunCancelDeferral::single(
                        RunReservationKind::SuccessCommit,
                    ))
                }
                Some(pending) if pending == &cause => RunCancelOutcome::Deferred(
                    RunCancelDeferral::single(RunReservationKind::SuccessCommit),
                ),
                Some(_) => RunCancelOutcome::Rejected,
            },
            RunClassification::EffectAdmitting { pending_cause } => match pending_cause {
                None => {
                    *pending_cause = Some(cause);
                    RunCancelOutcome::Deferred(RunCancelDeferral::single(
                        RunReservationKind::ToolEffectAdmission,
                    ))
                }
                Some(pending) if pending == &cause => RunCancelOutcome::Deferred(
                    RunCancelDeferral::single(RunReservationKind::ToolEffectAdmission),
                ),
                Some(_) => RunCancelOutcome::Rejected,
            },
            RunClassification::EffectCommitting { pending_cause } => match pending_cause {
                None => {
                    *pending_cause = Some(cause);
                    RunCancelOutcome::Deferred(RunCancelDeferral::single(
                        RunReservationKind::ToolEffectCommit,
                    ))
                }
                Some(pending) if pending == &cause => RunCancelOutcome::Deferred(
                    RunCancelDeferral::single(RunReservationKind::ToolEffectCommit),
                ),
                Some(_) => RunCancelOutcome::Rejected,
            },
            RunClassification::ToolSettling { pending_cause } => match pending_cause {
                None => {
                    *pending_cause = Some(cause);
                    RunCancelOutcome::Deferred(RunCancelDeferral::single(
                        RunReservationKind::ToolSettlement,
                    ))
                }
                Some(pending) if pending == &cause => RunCancelOutcome::Deferred(
                    RunCancelDeferral::single(RunReservationKind::ToolSettlement),
                ),
                Some(_) => RunCancelOutcome::Rejected,
            },
            RunClassification::SuccessSealed => RunCancelOutcome::Rejected,
            RunClassification::Cancelled(_) => RunCancelOutcome::Rejected,
        }
    }

    /// Commits durable evidence and the exact local cancellation as one classification boundary.
    ///
    /// The callback runs only while this owner is still open and while its classification mutex is
    /// retained. A concurrent success commit, Stop, failure, or supersession therefore cannot win
    /// between durable authorization and publishing the matching cancellation cause. On callback
    /// failure the owner remains open.
    pub(crate) fn commit_cancel_local<E>(
        &self,
        cause: RunCancellationCause,
        durable_commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<RunCancelOutcome, E> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*classification, RunClassification::Open) {
            return Ok(RunCancelOutcome::Rejected);
        }
        durable_commit()?;
        *classification = RunClassification::Cancelled(cause);
        drop(classification);
        self.inner.wake.cancel();
        Ok(RunCancelOutcome::Applied)
    }

    pub(crate) fn install_terminal_router(&self, router: &Arc<RunTerminalRoute>) -> Result<(), ()> {
        let mut installed = self
            .inner
            .terminal_router
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = installed.as_ref().and_then(|router| router.0.upgrade()) {
            return if Arc::ptr_eq(&existing, router) {
                Ok(())
            } else {
                Err(())
            };
        }
        *installed = Some(RunTerminalRouter(Arc::downgrade(router)));
        Ok(())
    }

    pub fn interrupt(&self, cause: TurnInterruptionCause) -> bool {
        self.cancel(RunCancellationCause::Interruption(cause))
    }

    pub fn supersede(&self) -> bool {
        self.cancel(RunCancellationCause::Superseded)
    }

    pub fn fail(&self, message: impl Into<String>) -> bool {
        self.cancel(RunCancellationCause::Failure(message.into()))
    }

    fn seal_reserved_success(&self) -> bool {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::SuccessCommitting { .. } = &*classification else {
            return false;
        };
        *classification = RunClassification::SuccessSealed;
        true
    }

    pub(crate) fn resolve_success_commit_authoritatively_local(
        &self,
        cause: RunCancellationCause,
    ) -> bool {
        self.resolve_success_commit_local(cause, false).is_some()
    }

    pub(crate) fn abandon_success_commit_local(
        &self,
        fallback_cause: RunCancellationCause,
    ) -> Option<RunCancellationCause> {
        self.resolve_success_commit_local(fallback_cause, true)
    }

    fn resolve_success_commit_local(
        &self,
        fallback_cause: RunCancellationCause,
        preserve_pending: bool,
    ) -> Option<RunCancellationCause> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::SuccessCommitting { pending_cause, .. } = &mut *classification
        else {
            return None;
        };
        let cause = if preserve_pending {
            pending_cause.take().unwrap_or(fallback_cause)
        } else {
            fallback_cause
        };
        *classification = RunClassification::Cancelled(cause.clone());
        drop(classification);
        self.inner.wake.cancel();
        Some(cause)
    }

    fn release_success_commit(&self) {
        let published = self.release_success_commit_local();
        if let Some(cause) = published {
            let _ = self.route_terminal(RunTerminalRouteKind::ReleaseSuccessCommit, cause);
        }
    }

    fn release_success_commit_local(&self) -> Option<RunCancellationCause> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::SuccessCommitting { pending_cause, .. } = &mut *classification
        else {
            return None;
        };
        let pending_cause = pending_cause.take();
        if let Some(cause) = pending_cause {
            *classification = RunClassification::Cancelled(cause.clone());
            drop(classification);
            self.inner.wake.cancel();
            Some(cause)
        } else {
            *classification = RunClassification::Open;
            None
        }
    }

    fn release_tool_settlement(&self) {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::ToolSettling { pending_cause } = &mut *classification else {
            return;
        };
        let pending_cause = pending_cause.take();
        if let Some(cause) = pending_cause {
            *classification = RunClassification::Cancelled(cause);
            drop(classification);
            self.inner.wake.cancel();
        } else {
            *classification = RunClassification::Open;
        }
    }

    fn release_tool_effect_commit(&self) {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::EffectCommitting { pending_cause } = &mut *classification else {
            return;
        };
        let pending_cause = pending_cause.take();
        if let Some(cause) = pending_cause {
            *classification = RunClassification::Cancelled(cause);
            drop(classification);
            self.inner.wake.cancel();
        } else {
            *classification = RunClassification::Open;
        }
    }

    fn resolve_tool_effect_admission(&self) -> Result<(), RunCancellationCause> {
        let mut classification = self
            .inner
            .classification
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let RunClassification::EffectAdmitting { pending_cause } = &mut *classification else {
            return Err(RunCancellationCause::Failure(
                "tool effect admission lost its runtime owner".to_string(),
            ));
        };
        let pending_cause = pending_cause.take();
        if let Some(cause) = pending_cause {
            *classification = RunClassification::Cancelled(cause.clone());
            drop(classification);
            self.inner.wake.cancel();
            Err(cause)
        } else {
            *classification = RunClassification::Open;
            Ok(())
        }
    }
}

fn cancellation_plan(classification: &RunClassification) -> Option<CancellationPlan> {
    match classification {
        RunClassification::Open => Some(CancellationPlan::Apply),
        RunClassification::SuccessCommitting { pending_cause, .. } => match pending_cause {
            None => Some(CancellationPlan::Defer(RunReservationKind::SuccessCommit)),
            Some(_) => None,
        },
        RunClassification::EffectAdmitting { pending_cause } => match pending_cause {
            None => Some(CancellationPlan::Defer(
                RunReservationKind::ToolEffectAdmission,
            )),
            Some(_) => None,
        },
        RunClassification::EffectCommitting { pending_cause } => match pending_cause {
            None => Some(CancellationPlan::Defer(
                RunReservationKind::ToolEffectCommit,
            )),
            Some(_) => None,
        },
        RunClassification::ToolSettling { pending_cause } => match pending_cause {
            None => Some(CancellationPlan::Defer(RunReservationKind::ToolSettlement)),
            Some(_) => None,
        },
        RunClassification::SuccessSealed | RunClassification::Cancelled(_) => None,
    }
}

fn apply_cancellation_plan(
    classification: &mut RunClassification,
    cause: RunCancellationCause,
    plan: CancellationPlan,
) -> bool {
    match plan {
        CancellationPlan::Apply => {
            *classification = RunClassification::Cancelled(cause);
            true
        }
        CancellationPlan::Defer(_) => {
            match classification {
                RunClassification::SuccessCommitting { pending_cause } => {
                    debug_assert!(pending_cause.is_none());
                    *pending_cause = Some(cause);
                }
                RunClassification::EffectAdmitting { pending_cause }
                | RunClassification::EffectCommitting { pending_cause }
                | RunClassification::ToolSettling { pending_cause } => {
                    debug_assert!(pending_cause.is_none());
                    *pending_cause = Some(cause);
                }
                _ => unreachable!("a deferred cancellation plan requires a reservation"),
            }
            false
        }
    }
}

fn linked_cancellation_locked(
    primary: &mut RunClassification,
    primary_cause: RunCancellationCause,
    secondary: &mut RunClassification,
    secondary_cause: RunCancellationCause,
) -> (RunCancelOutcome, bool, bool) {
    let Some(primary_plan) = cancellation_plan(primary) else {
        return (RunCancelOutcome::Rejected, false, false);
    };
    let Some(secondary_plan) = cancellation_plan(secondary) else {
        return (RunCancelOutcome::Rejected, false, false);
    };
    let wake_primary = apply_cancellation_plan(primary, primary_cause, primary_plan);
    let wake_secondary = apply_cancellation_plan(secondary, secondary_cause, secondary_plan);
    let outcome = if wake_primary || wake_secondary {
        RunCancelOutcome::Applied
    } else {
        let CancellationPlan::Defer(primary) = primary_plan else {
            unreachable!("a non-waking primary cancellation plan must be deferred");
        };
        let CancellationPlan::Defer(secondary) = secondary_plan else {
            unreachable!("a non-waking secondary cancellation plan must be deferred");
        };
        RunCancelOutcome::Deferred(RunCancelDeferral {
            primary,
            secondary: Some(secondary),
        })
    };
    (outcome, wake_primary, wake_secondary)
}

impl RootAdmissionGuard {
    /// Settles this exact plan with the durable revision returned by the database.
    pub(crate) fn commit(
        mut self,
        actual_revision: u64,
    ) -> Result<RootAdmissionReceipt, RootAdmissionSettleError> {
        let result = self
            .control
            .commit_root_admission(self.plan, actual_revision);
        self.settled = true;
        result
    }

    /// Explicitly aborts this exact plan while retaining the prior admitted receipt.
    pub(crate) fn abort(mut self) -> Result<(), RootAdmissionSettleError> {
        let result = self.control.abort_root_admission(self.plan);
        self.settled = true;
        result
    }
}

impl Drop for RootAdmissionGuard {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.control.abort_root_admission(self.plan);
            self.settled = true;
        }
    }
}

impl SuccessCommitReservation {
    pub fn seal(mut self) -> bool {
        let sealed = self.control.seal_reserved_success();
        self.resolved = sealed;
        sealed
    }

    pub fn resolve_authoritative_cancellation(mut self, cause: RunCancellationCause) -> bool {
        let routed = self.control.route_terminal(
            RunTerminalRouteKind::ResolveSuccessCommitAuthoritatively,
            cause.clone(),
        );
        let resolved = routed.map_or_else(
            || {
                self.control
                    .resolve_success_commit_authoritatively_local(cause)
            },
            |outcome| outcome == RunCancelOutcome::Applied,
        );
        self.resolved = resolved;
        resolved
    }

    pub fn abandon_with_cancellation(mut self, fallback_cause: RunCancellationCause) -> bool {
        let routed = self.control.route_terminal(
            RunTerminalRouteKind::AbandonSuccessCommit,
            fallback_cause.clone(),
        );
        let resolved = routed.map_or_else(
            || {
                self.control
                    .abandon_success_commit_local(fallback_cause)
                    .is_some()
            },
            |outcome| outcome == RunCancelOutcome::Applied,
        );
        self.resolved = resolved;
        resolved
    }

    pub fn release(mut self) {
        self.control.release_success_commit();
        self.resolved = true;
    }
}

impl Drop for SuccessCommitReservation {
    fn drop(&mut self) {
        if !self.resolved {
            self.control.release_success_commit();
            self.resolved = true;
        }
    }
}

impl ToolEffectAdmissionReservation {
    /// Opens the effect boundary only if no terminal producer arrived after the decision was
    /// accepted. Once this returns `Ok`, later Stop/failure producers cancel normally and do not
    /// retroactively revoke an effect that has started.
    pub fn admit(mut self) -> Result<(), RunCancellationCause> {
        let outcome = self.control.resolve_tool_effect_admission();
        self.resolved = true;
        outcome
    }
}

impl Drop for ToolEffectAdmissionReservation {
    fn drop(&mut self) {
        if !self.resolved {
            let _ = self.control.resolve_tool_effect_admission();
            self.resolved = true;
        }
    }
}

impl ToolSettlementReservation {
    pub fn release(mut self) {
        self.control.release_tool_settlement();
        self.released = true;
    }
}

impl ToolEffectCommitReservation {
    pub fn release(mut self) {
        self.control.release_tool_effect_commit();
        self.released = true;
    }
}

impl Drop for ToolEffectCommitReservation {
    fn drop(&mut self) {
        if !self.released {
            self.control.release_tool_effect_commit();
            self.released = true;
        }
    }
}

impl Drop for ToolSettlementReservation {
    fn drop(&mut self) {
        if !self.released {
            self.control.release_tool_settlement();
            self.released = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_cancellation_cause_wins_and_wakes_token_consumers() {
        let control = RunControl::new();
        let token = control.token();

        assert!(control.interrupt(TurnInterruptionCause::ApprovalAborted));
        assert!(!control.interrupt(TurnInterruptionCause::UserStop));
        assert!(token.is_cancelled());
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::ApprovalAborted
            ))
        );
    }

    #[test]
    fn operational_failure_is_not_an_interruption() {
        let control = RunControl::new();
        assert!(control.fail("permission broker disconnected"));
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Failure(
                "permission broker disconnected".to_string()
            ))
        );
    }

    #[test]
    fn sealed_success_rejects_late_stop_abort_failure_and_supersession() {
        let control = RunControl::new();
        assert!(control.seal_success());
        assert!(!control.interrupt(TurnInterruptionCause::UserStop));
        assert!(!control.interrupt(TurnInterruptionCause::ApprovalAborted));
        assert!(!control.fail("late failure"));
        assert!(!control.supersede());
        assert!(control.success_is_sealed());
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());
    }

    #[test]
    fn sealed_success_is_permanent_for_one_turn_and_does_not_change_a_fresh_turn() {
        let completed_turn = RunControl::new();
        assert!(completed_turn.seal_success());
        assert!(!completed_turn.interrupt(TurnInterruptionCause::UserStop));
        assert!(completed_turn.success_is_sealed());

        let next_turn = RunControl::new();
        assert!(!next_turn.same_owner(&completed_turn));
        assert!(!next_turn.success_is_sealed());
        assert_eq!(next_turn.cause(), None);
        assert!(!next_turn.is_cancelled());
    }

    #[test]
    fn success_commit_reservation_defers_cancellation_and_discards_it_when_applied() {
        let control = RunControl::new();
        let reservation = control.begin_success_commit().expect("reserve success");

        assert!(matches!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert!(matches!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::ApprovalAborted
            )),
            RunCancelOutcome::Rejected
        );
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());

        assert!(reservation.seal());
        assert!(control.success_is_sealed());
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());
    }

    #[test]
    fn releasing_success_commit_publishes_only_the_first_deferred_cause() {
        let control = RunControl::new();
        let reservation = control.begin_success_commit().expect("reserve success");

        assert!(matches!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(
            control.request_cancel(RunCancellationCause::Failure("late failure".to_string())),
            RunCancelOutcome::Rejected
        );
        reservation.release();

        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(control.is_cancelled());
    }

    #[test]
    fn standalone_success_abandonment_preserves_the_pending_first_cause() {
        let control = RunControl::new();
        let reservation = control.begin_success_commit().expect("reserve success");
        let pending = RunCancellationCause::Interruption(TurnInterruptionCause::UserStop);

        assert!(matches!(
            control.request_cancel(pending.clone()),
            RunCancelOutcome::Deferred(_)
        ));
        assert!(
            reservation.abandon_with_cancellation(RunCancellationCause::Failure(
                "internal commit failure".to_string(),
            ))
        );

        assert_eq!(control.cause(), Some(pending));
    }

    #[test]
    fn standalone_authoritative_success_resolution_uses_the_exact_cause() {
        let control = RunControl::new();
        let reservation = control.begin_success_commit().expect("reserve success");
        let pending = RunCancellationCause::Interruption(TurnInterruptionCause::UserStop);
        let authoritative = RunCancellationCause::Failure("durable failure".to_string());

        assert!(matches!(
            control.request_cancel(pending),
            RunCancelOutcome::Deferred(_)
        ));
        assert!(reservation.resolve_authoritative_cancellation(authoritative.clone()));

        assert_eq!(control.cause(), Some(authoritative));
    }

    #[test]
    fn tool_settlement_linearizes_the_commit_before_the_first_deferred_cause() {
        let control = RunControl::new();
        let settlement = control
            .begin_tool_settlement()
            .expect("reserve tool settlement");

        assert!(matches!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(
            control.request_cancel(RunCancellationCause::Failure("late failure".to_string())),
            RunCancelOutcome::Rejected
        );
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());

        settlement.release();
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(control.is_cancelled());
    }

    #[test]
    fn tool_effect_commit_defers_terminal_publication_until_consistency_is_restored() {
        let control = RunControl::new();
        let commit = control
            .begin_tool_effect_commit()
            .expect("reserve effect commit");

        assert_eq!(
            control.request_cancel(RunCancellationCause::Failure(
                "failure during effect commit".to_string()
            )),
            RunCancelOutcome::Deferred(RunCancelDeferral::single(
                RunReservationKind::ToolEffectCommit
            ))
        );
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());

        commit.release();
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Failure(
                "failure during effect commit".to_string()
            ))
        );
        assert!(control.is_cancelled());
    }

    #[test]
    fn tool_effect_admission_and_terminal_producer_have_one_lock_order() {
        let admission_first = RunControl::new();
        admission_first
            .begin_tool_effect_admission()
            .expect("reserve effect admission")
            .admit()
            .expect("effect starts");
        assert!(admission_first.interrupt(TurnInterruptionCause::UserStop));

        let stop_first = RunControl::new();
        assert!(stop_first.interrupt(TurnInterruptionCause::UserStop));
        assert!(stop_first.begin_tool_effect_admission().is_none());

        let failure_first = RunControl::new();
        assert!(failure_first.fail("provider failed"));
        assert!(failure_first.begin_tool_effect_admission().is_none());

        let stop_during_admission = RunControl::new();
        let admission = stop_during_admission
            .begin_tool_effect_admission()
            .expect("reserve admission before Stop");
        assert!(matches!(
            stop_during_admission.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Deferred(_)
        ));
        assert_eq!(
            admission.admit(),
            Err(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(stop_during_admission.is_cancelled());
    }

    #[test]
    fn root_admission_guard_commits_exact_receipts_and_continuation_replaces_last() {
        let control = RunControl::new();
        let session_id = SessionId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();

        let admission_a = control
            .begin_root_admission(session_id, turn_a)
            .expect("begin A before its database admission");
        let plan_a = admission_a.plan;
        assert_eq!(control.root_admission_snapshot().pending, Some(plan_a));
        let receipt_a = admission_a.commit(11).expect("commit admitted A");
        assert_eq!(
            receipt_a,
            RootAdmissionReceipt {
                session_id,
                turn_id: turn_a,
                revision: 11,
            }
        );
        assert_eq!(
            control.root_admission_snapshot(),
            RootAdmissionSnapshot {
                last_admitted: Some(receipt_a),
                pending: None,
                stop_seal_id: None,
            }
        );

        let receipt_b = control
            .begin_root_admission(session_id, turn_b)
            .expect("begin continuation B")
            .commit(12)
            .expect("commit admitted B");
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal after B, got {outcome:?}"),
        };
        assert_eq!(stop_plan.last_admitted, Some(receipt_b));
        assert_eq!(stop_plan.pending, None);
        assert!(control.release_root_admission_stop_seal(stop_plan.seal_id));
    }

    #[tokio::test]
    async fn dropped_root_admission_guard_aborts_exact_pending_and_preserves_last() {
        let control = RunControl::new();
        let session_id = SessionId::new();
        let receipt_a = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin A")
            .commit(1)
            .expect("commit A");
        let admission_b = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin B");
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal over pending B, got {outcome:?}"),
        };
        assert_eq!(stop_plan.last_admitted, Some(receipt_a));
        assert_eq!(stop_plan.pending, Some(admission_b.plan));

        let waiting_control = control.clone();
        let waiter = tokio::spawn(async move {
            waiting_control
                .wait_root_admission_settlement(stop_plan)
                .await
        });
        tokio::task::yield_now().await;
        drop(admission_b);

        assert_eq!(
            waiter.await.expect("settlement waiter task"),
            Ok(RootAdmissionSettlement::Aborted)
        );
        let snapshot = control.root_admission_snapshot();
        assert_eq!(snapshot.last_admitted, Some(receipt_a));
        assert_eq!(snapshot.pending, None);
        assert_eq!(snapshot.stop_seal_id, Some(stop_plan.seal_id));
        assert!(control.release_root_admission_stop_seal(stop_plan.seal_id));
    }

    #[tokio::test]
    async fn root_admission_wait_retains_actual_commit_even_before_waiter_subscribes() {
        let control = RunControl::new();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let admission = control
            .begin_root_admission(session_id, turn_id)
            .expect("begin admission");
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal, got {outcome:?}"),
        };
        let receipt = admission.commit(41).expect("database accepted admission");

        assert_eq!(
            control.wait_root_admission_settlement(stop_plan).await,
            Ok(RootAdmissionSettlement::Admitted(receipt))
        );
        assert!(control.release_root_admission_stop_seal(stop_plan.seal_id));
    }

    #[tokio::test]
    async fn stale_root_admission_stop_plan_fails_closed_after_exact_release() {
        let control = RunControl::new();
        let session_id = SessionId::new();
        let admission_a = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin A");
        let stop_a = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal A, got {outcome:?}"),
        };
        admission_a.abort().expect("abort A");
        assert!(control.release_root_admission_stop_seal(stop_a.seal_id));

        let admission_b = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin B");
        let stop_b = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal B, got {outcome:?}"),
        };
        assert_ne!(stop_a.seal_id, stop_b.seal_id);
        assert_eq!(
            control.wait_root_admission_settlement(stop_a).await,
            Err(RootAdmissionWaitError::StaleStopPlan)
        );
        admission_b.abort().expect("abort B");
        assert!(control.release_root_admission_stop_seal(stop_b.seal_id));
    }

    #[tokio::test]
    async fn post_admission_stop_resolution_waits_for_exact_release() {
        let control = RunControl::new();
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal, got {outcome:?}"),
        };
        let waiting_control = control.clone();
        let waiter = tokio::spawn(async move {
            waiting_control
                .wait_for_root_admission_stop_resolution()
                .await
        });
        tokio::task::yield_now().await;
        assert!(control.release_root_admission_stop_seal(stop_plan.seal_id));

        assert_eq!(
            waiter.await.expect("Stop resolution waiter task"),
            RootAdmissionStopResolution::Continue
        );
    }

    #[tokio::test]
    async fn post_admission_stop_resolution_observes_outer_cancellation() {
        let control = RunControl::new();
        assert!(matches!(
            control.seal_root_admission_for_stop(),
            RootAdmissionStopSealOutcome::Acquired(_)
        ));
        let waiting_control = control.clone();
        let waiter = tokio::spawn(async move {
            waiting_control
                .wait_for_root_admission_stop_resolution()
                .await
        });
        tokio::task::yield_now().await;
        assert!(control.interrupt(TurnInterruptionCause::UserStop));

        assert_eq!(
            waiter.await.expect("Stop resolution waiter task"),
            RootAdmissionStopResolution::Cancelled
        );
    }

    #[tokio::test]
    async fn committed_root_admission_stop_wakes_waiter_without_cancelling_outer_scope() {
        let control = RunControl::new();
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal, got {outcome:?}"),
        };
        let waiting_control = control.clone();
        let waiter = tokio::spawn(async move {
            waiting_control
                .wait_for_root_admission_stop_resolution()
                .await
        });
        tokio::task::yield_now().await;

        assert!(control.commit_root_admission_stop_seal(stop_plan.seal_id));
        assert_eq!(
            waiter.await.expect("Stop resolution waiter task"),
            RootAdmissionStopResolution::Cancelled
        );
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());
        assert!(control.root_admission_stop_is_sealed());
        assert!(!control.release_root_admission_stop_seal(stop_plan.seal_id));
        assert!(matches!(
            control.begin_root_admission(SessionId::new(), TurnId::new()),
            Err(RootAdmissionBeginError::StopSealed)
        ));
    }

    #[tokio::test]
    async fn post_admission_stop_resolution_is_immediate_without_a_seal() {
        let control = RunControl::new();
        assert_eq!(
            control.wait_for_root_admission_stop_resolution().await,
            RootAdmissionStopResolution::Continue
        );
        assert!(control.interrupt(TurnInterruptionCause::UserStop));
        assert_eq!(
            control.wait_for_root_admission_stop_resolution().await,
            RootAdmissionStopResolution::Cancelled
        );
    }

    #[test]
    fn root_admission_stop_seal_is_single_flight_and_release_is_exact() {
        let control = RunControl::new();
        let stop_a = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected first Stop seal, got {outcome:?}"),
        };
        assert_eq!(
            control.seal_root_admission_for_stop(),
            RootAdmissionStopSealOutcome::AlreadySealed
        );
        assert!(matches!(
            control.begin_root_admission(SessionId::new(), TurnId::new()),
            Err(RootAdmissionBeginError::StopSealed)
        ));
        assert!(
            !control
                .release_root_admission_stop_seal(RootAdmissionStopSealId(stop_a.seal_id.0 + 1))
        );
        assert_eq!(
            control.seal_root_admission_for_stop(),
            RootAdmissionStopSealOutcome::AlreadySealed
        );
        assert!(control.release_root_admission_stop_seal(stop_a.seal_id));

        let stop_b = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected replacement Stop seal, got {outcome:?}"),
        };
        assert_ne!(stop_a.seal_id, stop_b.seal_id);
        assert!(!control.release_root_admission_stop_seal(stop_a.seal_id));
        assert!(control.release_root_admission_stop_seal(stop_b.seal_id));
    }

    #[test]
    fn cancelled_or_success_sealed_control_rejects_new_root_admission() {
        let cancelled = RunControl::new();
        assert!(cancelled.interrupt(TurnInterruptionCause::UserStop));
        assert!(matches!(
            cancelled.begin_root_admission(SessionId::new(), TurnId::new()),
            Err(RootAdmissionBeginError::RunClosed)
        ));

        let success_sealed = RunControl::new();
        assert!(success_sealed.seal_success());
        assert!(matches!(
            success_sealed.begin_root_admission(SessionId::new(), TurnId::new()),
            Err(RootAdmissionBeginError::RunClosed)
        ));
    }

    #[test]
    fn root_admission_settlement_is_exact_to_plan_id() {
        let control = RunControl::new();
        let guard = control
            .begin_root_admission(SessionId::new(), TurnId::new())
            .expect("begin admission");
        let exact = guard.plan;
        let stale = RootAdmissionPlan {
            plan_id: RootAdmissionPlanId(exact.plan_id.0 + 1),
            ..exact
        };

        assert_eq!(
            control.abort_root_admission(stale),
            Err(RootAdmissionSettleError::StalePlan)
        );
        assert_eq!(control.root_admission_snapshot().pending, Some(exact));
        guard.abort().expect("exact guard aborts its pending plan");
        assert_eq!(control.root_admission_snapshot().pending, None);
    }

    #[test]
    fn root_admission_rejects_cross_session_continuation() {
        let control = RunControl::new();
        let owned_session = SessionId::new();
        let other_session = SessionId::new();
        let receipt = control
            .begin_root_admission(owned_session, TurnId::new())
            .expect("begin owned session")
            .commit(7)
            .expect("commit owned session");

        assert!(matches!(
            control.begin_root_admission(other_session, TurnId::new()),
            Err(RootAdmissionBeginError::SessionMismatch { expected, actual })
                if expected == owned_session && actual == other_session
        ));
        assert_eq!(
            control.root_admission_snapshot(),
            RootAdmissionSnapshot {
                last_admitted: Some(receipt),
                pending: None,
                stop_seal_id: None,
            }
        );
    }

    #[tokio::test]
    async fn root_admission_publishes_non_monotonic_durable_receipt_to_stop_waiter() {
        let control = RunControl::new();
        let session_id = SessionId::new();
        let receipt_a = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin A")
            .commit(20)
            .expect("commit A");
        let admission_b = control
            .begin_root_admission(session_id, TurnId::new())
            .expect("begin B");
        let stop_plan = match control.seal_root_admission_for_stop() {
            RootAdmissionStopSealOutcome::Acquired(plan) => plan,
            outcome => panic!("expected Stop seal over pending B, got {outcome:?}"),
        };
        assert_eq!(stop_plan.last_admitted, Some(receipt_a));
        assert_eq!(stop_plan.pending, Some(admission_b.plan));

        let receipt_b = admission_b
            .commit(19)
            .expect("durable B must remain canonical even with a non-monotonic revision");
        assert_eq!(
            control.root_admission_snapshot(),
            RootAdmissionSnapshot {
                last_admitted: Some(receipt_b),
                pending: None,
                stop_seal_id: Some(stop_plan.seal_id),
            }
        );
        assert_eq!(
            control.wait_root_admission_settlement(stop_plan).await,
            Ok(RootAdmissionSettlement::Admitted(receipt_b))
        );
        assert!(control.release_root_admission_stop_seal(stop_plan.seal_id));
    }

    #[test]
    fn root_admission_value_types_are_copy_debug_and_eq() {
        fn assert_copy_debug_eq<T: Copy + fmt::Debug + Eq>() {}

        assert_copy_debug_eq::<RootAdmissionPlanId>();
        assert_copy_debug_eq::<RootAdmissionPlan>();
        assert_copy_debug_eq::<RootAdmissionReceipt>();
        assert_copy_debug_eq::<RootAdmissionStopSealId>();
        assert_copy_debug_eq::<RootAdmissionStopPlan>();
        assert_copy_debug_eq::<RootAdmissionSnapshot>();
        assert_copy_debug_eq::<RootAdmissionStopSealOutcome>();
        assert_copy_debug_eq::<RootAdmissionSettlement>();
        assert_copy_debug_eq::<RootAdmissionStopResolution>();
        assert_copy_debug_eq::<RootAdmissionBeginError>();
        assert_copy_debug_eq::<RootAdmissionSettleError>();
        assert_copy_debug_eq::<RootAdmissionWaitError>();
    }
}
