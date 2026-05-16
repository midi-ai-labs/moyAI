use camino::Utf8PathBuf;

use crate::session::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavigationRequestId(u64);

impl NavigationRequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationTarget {
    Workspace {
        path: Utf8PathBuf,
        selected_session_id: Option<SessionId>,
        starts_new_project_session: bool,
    },
    Session {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationRequest {
    pub id: NavigationRequestId,
    pub target: NavigationTarget,
}

#[derive(Debug, Clone)]
pub struct DesktopNavigationState {
    next_id: u64,
    active: Option<NavigationRequest>,
}

impl Default for DesktopNavigationState {
    fn default() -> Self {
        Self {
            next_id: 1,
            active: None,
        }
    }
}

impl DesktopNavigationState {
    pub fn begin_workspace(
        &mut self,
        path: Utf8PathBuf,
        selected_session_id: Option<SessionId>,
        starts_new_project_session: bool,
    ) -> NavigationRequestId {
        self.begin(NavigationTarget::Workspace {
            path,
            selected_session_id,
            starts_new_project_session,
        })
    }

    pub fn begin_session(&mut self, session_id: SessionId) -> NavigationRequestId {
        self.begin(NavigationTarget::Session { session_id })
    }

    pub fn active(&self) -> Option<&NavigationRequest> {
        self.active.as_ref()
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn is_current(&self, id: NavigationRequestId) -> bool {
        self.active.as_ref().is_some_and(|request| request.id == id)
    }

    pub fn is_current_session(&self, id: NavigationRequestId, session_id: SessionId) -> bool {
        self.active.as_ref().is_some_and(|request| {
            request.id == id
                && matches!(
                    request.target,
                    NavigationTarget::Session { session_id: active } if active == session_id
                )
        })
    }

    pub fn finish(&mut self, id: NavigationRequestId) -> bool {
        if self.is_current(id) {
            self.active = None;
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.active = None;
    }

    fn begin(&mut self, target: NavigationTarget) -> NavigationRequestId {
        let id = NavigationRequestId(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.active = Some(NavigationRequest { id, target });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_navigation_supersedes_older_request() {
        let mut state = DesktopNavigationState::default();
        let first = state.begin_workspace(Utf8PathBuf::from("C:/one"), None, false);
        let session = SessionId::new();
        let second = state.begin_session(session);

        assert!(!state.is_current(first));
        assert!(state.is_current(second));
        assert!(state.is_current_session(second, session));
        assert!(!state.finish(first));
        assert!(state.is_active());
        assert!(state.finish(second));
        assert!(!state.is_active());
    }
}
