#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopAsyncOperationKind {
    AgentRun,
    SnapshotRefresh,
    ProviderModelCatalogLoad,
    WorkspaceLoad,
    SessionLoad,
    TurnPageLoad,
    TerminalRunRefresh,
    HistoryExport,
    ProjectDelete,
    SessionDelete,
    SessionArchive,
    SessionRollback,
    SessionMaintenance,
    SessionSearch,
    PromptEnhance,
    SteerSubmission,
    AccessModePersistence,
}

impl DesktopAsyncOperationKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::AgentRun => "agent_run",
            Self::SnapshotRefresh => "snapshot_refresh",
            Self::ProviderModelCatalogLoad => "provider_model_catalog_load",
            Self::WorkspaceLoad => "workspace_load",
            Self::SessionLoad => "session_load",
            Self::TurnPageLoad => "turn_page_load",
            Self::TerminalRunRefresh => "terminal_run_refresh",
            Self::HistoryExport => "history_export",
            Self::ProjectDelete => "project_delete",
            Self::SessionDelete => "session_delete",
            Self::SessionArchive => "session_archive",
            Self::SessionRollback => "session_rollback",
            Self::SessionMaintenance => "session_maintenance",
            Self::SessionSearch => "session_search",
            Self::PromptEnhance => "prompt_enhance",
            Self::SteerSubmission => "steer_submission",
            Self::AccessModePersistence => "access_mode_persistence",
        }
    }

    pub fn requires_polling(self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopAsyncOperationId(u64);

impl DesktopAsyncOperationId {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test_value(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionSearchRequestId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LatestRequestId(u64);

#[derive(Debug, Clone)]
pub struct LatestRequestTracker<T> {
    next_id: u64,
    latest: Option<(LatestRequestId, T)>,
}

impl<T> Default for LatestRequestTracker<T> {
    fn default() -> Self {
        Self {
            next_id: 1,
            latest: None,
        }
    }
}

impl<T: PartialEq> LatestRequestTracker<T> {
    pub fn begin(&mut self, target: T) -> LatestRequestId {
        let request_id = LatestRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.latest = Some((request_id, target));
        request_id
    }

    pub fn finish_if_current(&mut self, request_id: LatestRequestId, target: &T) -> bool {
        if self.is_current(request_id, target) {
            self.latest = None;
            true
        } else {
            false
        }
    }

    pub fn is_current(&self, request_id: LatestRequestId, target: &T) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|(latest_id, latest_target)| {
                *latest_id == request_id && latest_target == target
            })
    }

    pub fn is_pending(&self) -> bool {
        self.latest.is_some()
    }

    pub fn clear(&mut self) {
        self.latest = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSearchDispatch<T> {
    pub request_id: SessionSearchRequestId,
    pub target: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSearchAdmission<T> {
    pub dispatch: Option<SessionSearchDispatch<T>>,
    pub superseded_operation_id: Option<DesktopAsyncOperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSearchCompletion<T> {
    pub operation_id: Option<DesktopAsyncOperationId>,
    pub is_latest: bool,
    pub next_dispatch: Option<SessionSearchDispatch<T>>,
}

#[derive(Debug, Clone)]
struct TrackedSessionSearch<T> {
    request_id: SessionSearchRequestId,
    operation_id: Option<DesktopAsyncOperationId>,
    target: T,
}

impl<T: Clone> TrackedSessionSearch<T> {
    fn dispatch(&self) -> SessionSearchDispatch<T> {
        SessionSearchDispatch {
            request_id: self.request_id,
            target: self.target.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSearchRequestTracker<T> {
    next_id: u64,
    running: Option<TrackedSessionSearch<T>>,
    queued: Option<TrackedSessionSearch<T>>,
}

impl<T> Default for SessionSearchRequestTracker<T> {
    fn default() -> Self {
        Self {
            next_id: 1,
            running: None,
            queued: None,
        }
    }
}

impl<T: Clone> SessionSearchRequestTracker<T> {
    pub fn begin(
        &mut self,
        operation_id: DesktopAsyncOperationId,
        target: T,
    ) -> SessionSearchAdmission<T> {
        let request_id = SessionSearchRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        let request = TrackedSessionSearch {
            request_id,
            operation_id: Some(operation_id),
            target,
        };
        if self.running.is_none() {
            let dispatch = request.dispatch();
            self.running = Some(request);
            return SessionSearchAdmission {
                dispatch: Some(dispatch),
                superseded_operation_id: None,
            };
        }

        let superseded_operation_id = self
            .queued
            .replace(request)
            .and_then(|queued| queued.operation_id);
        SessionSearchAdmission {
            dispatch: None,
            superseded_operation_id,
        }
    }

    pub fn finish(
        &mut self,
        request_id: SessionSearchRequestId,
    ) -> Option<SessionSearchCompletion<T>> {
        if self.running.as_ref()?.request_id != request_id {
            return None;
        }
        let completed = self.running.take()?;
        let next = self.queued.take();
        let next_dispatch = next.as_ref().map(TrackedSessionSearch::dispatch);
        self.running = next;
        Some(SessionSearchCompletion {
            operation_id: completed.operation_id,
            is_latest: completed.operation_id.is_some() && self.running.is_none(),
            next_dispatch,
        })
    }

    pub fn clear(&mut self) -> Vec<DesktopAsyncOperationId> {
        let mut operation_ids = Vec::with_capacity(2);
        if let Some(running) = self.running.as_mut()
            && let Some(operation_id) = running.operation_id.take()
        {
            operation_ids.push(operation_id);
        }
        if let Some(queued) = self.queued.take()
            && let Some(operation_id) = queued.operation_id
        {
            operation_ids.push(operation_id);
        }
        operation_ids
    }

    #[cfg(test)]
    fn running_request_id(&self) -> Option<SessionSearchRequestId> {
        self.running.as_ref().map(|request| request.request_id)
    }

    #[cfg(test)]
    fn queued_request_id(&self) -> Option<SessionSearchRequestId> {
        self.queued.as_ref().map(|request| request.request_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAsyncOperation {
    pub id: DesktopAsyncOperationId,
    pub kind: DesktopAsyncOperationKind,
}

#[derive(Debug, Clone)]
pub struct DesktopAsyncOperationRegistry {
    next_id: u64,
    active: Vec<DesktopAsyncOperation>,
}

impl Default for DesktopAsyncOperationRegistry {
    fn default() -> Self {
        Self {
            next_id: 1,
            active: Vec::new(),
        }
    }
}

impl DesktopAsyncOperationRegistry {
    pub fn begin(&mut self, kind: DesktopAsyncOperationKind) -> DesktopAsyncOperationId {
        let id = DesktopAsyncOperationId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.active.push(DesktopAsyncOperation { id, kind });
        id
    }

    pub fn begin_unique(&mut self, kind: DesktopAsyncOperationKind) -> DesktopAsyncOperationId {
        self.finish_kind(kind);
        self.begin(kind)
    }

    pub fn finish(&mut self, id: DesktopAsyncOperationId) -> bool {
        let before = self.active.len();
        self.active.retain(|operation| operation.id != id);
        before != self.active.len()
    }

    pub fn contains(&self, id: DesktopAsyncOperationId) -> bool {
        self.active.iter().any(|operation| operation.id == id)
    }

    pub fn finish_one_kind(&mut self, kind: DesktopAsyncOperationKind) -> bool {
        if let Some(index) = self
            .active
            .iter()
            .rposition(|operation| operation.kind == kind)
        {
            self.active.remove(index);
            true
        } else {
            false
        }
    }

    pub fn finish_kind(&mut self, kind: DesktopAsyncOperationKind) -> usize {
        let before = self.active.len();
        self.active.retain(|operation| operation.kind != kind);
        before - self.active.len()
    }

    pub fn is_pending(&self, kind: DesktopAsyncOperationKind) -> bool {
        self.active.iter().any(|operation| operation.kind == kind)
    }

    pub fn count(&self, kind: DesktopAsyncOperationKind) -> usize {
        self.active
            .iter()
            .filter(|operation| operation.kind == kind)
            .count()
    }

    pub fn polling_required(&self) -> bool {
        self.active
            .iter()
            .any(|operation| operation.kind.requires_polling())
    }

    pub fn active_kinds(&self) -> Vec<DesktopAsyncOperationKind> {
        let mut kinds = Vec::new();
        for operation in &self.active {
            if !kinds.contains(&operation.kind) {
                kinds.push(operation.kind);
            }
        }
        kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_tracks_unique_and_counted_operations() {
        let mut registry = DesktopAsyncOperationRegistry::default();
        registry.begin_unique(DesktopAsyncOperationKind::TerminalRunRefresh);
        registry.begin_unique(DesktopAsyncOperationKind::TerminalRunRefresh);
        assert_eq!(
            registry.count(DesktopAsyncOperationKind::TerminalRunRefresh),
            1
        );

        registry.begin(DesktopAsyncOperationKind::SessionDelete);
        registry.begin(DesktopAsyncOperationKind::SessionDelete);
        assert_eq!(registry.count(DesktopAsyncOperationKind::SessionDelete), 2);
        assert!(registry.polling_required());

        assert!(registry.finish_one_kind(DesktopAsyncOperationKind::SessionDelete));
        assert_eq!(registry.count(DesktopAsyncOperationKind::SessionDelete), 1);
        assert_eq!(
            registry.finish_kind(DesktopAsyncOperationKind::SessionDelete),
            1
        );
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionDelete));

        let first_search = registry.begin_unique(DesktopAsyncOperationKind::SessionSearch);
        let latest_search = registry.begin_unique(DesktopAsyncOperationKind::SessionSearch);
        assert!(!registry.finish(first_search));
        assert!(registry.is_pending(DesktopAsyncOperationKind::SessionSearch));
        assert!(registry.finish(latest_search));
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionSearch));
    }

    #[test]
    fn session_search_tracker_bounds_work_to_one_running_and_one_latest_queued() {
        let mut registry = DesktopAsyncOperationRegistry::default();
        let mut tracker = SessionSearchRequestTracker::default();
        let first = tracker.begin(
            registry.begin(DesktopAsyncOperationKind::SessionSearch),
            "a".to_string(),
        );
        let first_dispatch = first.dispatch.expect("first request starts immediately");
        assert_eq!(first.superseded_operation_id, None);

        let second = tracker.begin(
            registry.begin(DesktopAsyncOperationKind::SessionSearch),
            "ab".to_string(),
        );
        assert_eq!(second.dispatch, None);
        assert_eq!(second.superseded_operation_id, None);
        let second_request_id = tracker.queued_request_id().expect("second request queued");

        let latest_operation = registry.begin(DesktopAsyncOperationKind::SessionSearch);
        let latest = tracker.begin(latest_operation, "abc".to_string());
        assert_eq!(latest.dispatch, None);
        assert!(
            registry.finish(
                latest
                    .superseded_operation_id
                    .expect("second request is superseded")
            )
        );
        assert_eq!(registry.count(DesktopAsyncOperationKind::SessionSearch), 2);
        assert_ne!(tracker.queued_request_id(), Some(second_request_id));

        assert_eq!(tracker.finish(second_request_id), None);
        let first_completion = tracker
            .finish(first_dispatch.request_id)
            .expect("running request completes");
        assert!(!first_completion.is_latest);
        assert!(
            registry.finish(
                first_completion
                    .operation_id
                    .expect("running operation remains counted")
            )
        );
        let latest_dispatch = first_completion
            .next_dispatch
            .expect("latest queued request starts next");
        assert_eq!(latest_dispatch.target, "abc");
        assert_eq!(
            tracker.running_request_id(),
            Some(latest_dispatch.request_id)
        );

        let latest_completion = tracker
            .finish(latest_dispatch.request_id)
            .expect("latest request completes");
        assert!(latest_completion.is_latest);
        assert_eq!(latest_completion.next_dispatch, None);
        assert!(
            registry.finish(
                latest_completion
                    .operation_id
                    .expect("latest operation remains counted")
            )
        );
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionSearch));
    }

    #[test]
    fn clearing_session_search_invalidates_running_without_admitting_a_second_worker() {
        let mut registry = DesktopAsyncOperationRegistry::default();
        let mut tracker = SessionSearchRequestTracker::default();
        let abandoned = tracker.begin(
            registry.begin(DesktopAsyncOperationKind::SessionSearch),
            "old".to_string(),
        );
        let abandoned_dispatch = abandoned.dispatch.expect("old request starts");
        let obsolete = tracker.begin(
            registry.begin(DesktopAsyncOperationKind::SessionSearch),
            "obsolete".to_string(),
        );
        assert_eq!(obsolete.dispatch, None);
        assert_eq!(registry.count(DesktopAsyncOperationKind::SessionSearch), 2);
        let mut cleared = 0;
        for operation_id in tracker.clear() {
            assert!(registry.finish(operation_id));
            cleared += 1;
        }
        assert_eq!(cleared, 2);
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionSearch));

        let replacement = tracker.begin(
            registry.begin(DesktopAsyncOperationKind::SessionSearch),
            "new".to_string(),
        );
        assert_eq!(replacement.dispatch, None);
        assert!(tracker.queued_request_id().is_some());

        let abandoned_completion = tracker
            .finish(abandoned_dispatch.request_id)
            .expect("invalidated worker still settles its slot");
        assert_eq!(abandoned_completion.operation_id, None);
        assert!(!abandoned_completion.is_latest);
        let replacement_dispatch = abandoned_completion
            .next_dispatch
            .expect("replacement starts after old worker settles");
        assert_eq!(replacement_dispatch.target, "new");
        let replacement_completion = tracker
            .finish(replacement_dispatch.request_id)
            .expect("replacement completes");
        assert!(replacement_completion.is_latest);
        assert!(
            registry.finish(
                replacement_completion
                    .operation_id
                    .expect("replacement operation is counted")
            )
        );
    }

    #[test]
    fn latest_request_tracker_rejects_stale_generation_and_wrong_target() {
        let mut tracker = LatestRequestTracker::default();
        assert!(!tracker.is_pending());
        let first = tracker.begin("workspace-a".to_string());
        let latest = tracker.begin("workspace-b".to_string());
        assert!(tracker.is_pending());

        assert!(!tracker.finish_if_current(first, &"workspace-a".to_string()));
        assert!(!tracker.finish_if_current(latest, &"workspace-a".to_string()));
        assert!(tracker.finish_if_current(latest, &"workspace-b".to_string()));
        assert!(!tracker.is_pending());
        assert!(!tracker.finish_if_current(latest, &"workspace-b".to_string()));

        let abandoned = tracker.begin("workspace-a".to_string());
        tracker.clear();
        assert!(!tracker.finish_if_current(abandoned, &"workspace-a".to_string()));
    }
}
