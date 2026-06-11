use super::async_ops::DesktopAsyncOperationRegistry;
use super::state::{DEFAULT_WINDOW_OPACITY_PERCENT, DesktopOverlay};

#[derive(Debug, Clone)]
pub struct DesktopViewState {
    pub overlay: DesktopOverlay,
    pub window_opacity_percent: i32,
    pub artifact_selected_index: usize,
    pub local_search_text: String,
    pub session_search_text: String,
    pub session_search_include_archived: bool,
    pub async_operations: DesktopAsyncOperationRegistry,
}

impl Default for DesktopViewState {
    fn default() -> Self {
        Self {
            overlay: DesktopOverlay::None,
            window_opacity_percent: DEFAULT_WINDOW_OPACITY_PERCENT,
            artifact_selected_index: 0,
            local_search_text: String::new(),
            session_search_text: String::new(),
            session_search_include_archived: false,
            async_operations: DesktopAsyncOperationRegistry::default(),
        }
    }
}
