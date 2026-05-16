#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopAsyncOperationKind {
    AgentRun,
    StartupProviderProbe,
    ProviderModelCatalogLoad,
    WorkspaceLoad,
    SessionLoad,
    TerminalRunRefresh,
    CurrentTodoRefresh,
    HistoryExport,
    ProjectDelete,
    SessionDelete,
    PromptEnhance,
}

impl DesktopAsyncOperationKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::AgentRun => "agent_run",
            Self::StartupProviderProbe => "startup_provider_probe",
            Self::ProviderModelCatalogLoad => "provider_model_catalog_load",
            Self::WorkspaceLoad => "workspace_load",
            Self::SessionLoad => "session_load",
            Self::TerminalRunRefresh => "terminal_run_refresh",
            Self::CurrentTodoRefresh => "current_todo_refresh",
            Self::HistoryExport => "history_export",
            Self::ProjectDelete => "project_delete",
            Self::SessionDelete => "session_delete",
            Self::PromptEnhance => "prompt_enhance",
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
    }
}
