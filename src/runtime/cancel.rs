use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use tokio_util::sync::CancellationToken;

use crate::protocol::TurnInterruptionCause;

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
        Self {
            inner: Arc::new(RunControlInner {
                wake: CancellationToken::new(),
                classification: Mutex::new(RunClassification::default()),
                terminal_router: Mutex::new(None),
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
}
