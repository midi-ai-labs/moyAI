use std::collections::HashMap;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSearchCompletion {
    pub operation_id: DesktopAsyncOperationId,
    pub is_latest: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSearchRequestTracker {
    next_id: u64,
    latest: Option<SessionSearchRequestId>,
    pending: HashMap<SessionSearchRequestId, DesktopAsyncOperationId>,
}

impl Default for SessionSearchRequestTracker {
    fn default() -> Self {
        Self {
            next_id: 1,
            latest: None,
            pending: HashMap::new(),
        }
    }
}

impl SessionSearchRequestTracker {
    pub fn begin(&mut self, operation_id: DesktopAsyncOperationId) -> SessionSearchRequestId {
        let request_id = SessionSearchRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.latest = Some(request_id);
        self.pending.insert(request_id, operation_id);
        request_id
    }

    pub fn finish(
        &mut self,
        request_id: SessionSearchRequestId,
    ) -> Option<SessionSearchCompletion> {
        let operation_id = self.pending.remove(&request_id)?;
        Some(SessionSearchCompletion {
            operation_id,
            is_latest: self.latest == Some(request_id),
        })
    }

    pub fn clear(&mut self) -> Vec<DesktopAsyncOperationId> {
        self.latest = None;
        self.pending
            .drain()
            .map(|(_, operation_id)| operation_id)
            .collect()
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
    fn session_search_tracker_keeps_all_work_counted_and_only_applies_newest() {
        let mut registry = DesktopAsyncOperationRegistry::default();
        let mut tracker = SessionSearchRequestTracker::default();
        let first = tracker.begin(registry.begin(DesktopAsyncOperationKind::SessionSearch));
        let latest = tracker.begin(registry.begin(DesktopAsyncOperationKind::SessionSearch));

        let latest_completion = tracker.finish(latest).expect("latest request is pending");
        assert!(latest_completion.is_latest);
        assert!(registry.finish(latest_completion.operation_id));
        assert!(registry.is_pending(DesktopAsyncOperationKind::SessionSearch));

        let stale_completion = tracker.finish(first).expect("first request is pending");
        assert!(!stale_completion.is_latest);
        assert!(registry.finish(stale_completion.operation_id));
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionSearch));

        let abandoned = tracker.begin(registry.begin(DesktopAsyncOperationKind::SessionSearch));
        for operation_id in tracker.clear() {
            assert!(registry.finish(operation_id));
        }
        assert_eq!(tracker.finish(abandoned), None);
        assert!(!registry.is_pending(DesktopAsyncOperationKind::SessionSearch));
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
