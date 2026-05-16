use camino::Utf8PathBuf;

use crate::session::SessionId;
use crate::storage::StoragePaths;

#[derive(Debug, Clone)]
pub struct DesktopArgs {
    pub directory: Option<Utf8PathBuf>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
    pub global_config_existed_at_launch: bool,
}

pub fn quick_chat_workspace_directory() -> Option<Utf8PathBuf> {
    StoragePaths::discover()
        .ok()
        .map(|paths| paths.data_dir.join("quick-chat-workspace"))
}
