use camino::Utf8PathBuf;
use directories_next::UserDirs;

use crate::session::SessionId;

#[derive(Debug, Clone)]
pub struct DesktopArgs {
    pub directory: Option<Utf8PathBuf>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
}

pub fn default_workspace_directory() -> Option<Utf8PathBuf> {
    UserDirs::new()
        .and_then(|dirs| dirs.desktop_dir().map(|path| path.to_path_buf()))
        .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
}
