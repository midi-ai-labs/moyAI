use camino::Utf8PathBuf;

use crate::session::SessionId;

#[derive(Debug, Clone)]
pub struct DesktopArgs {
    pub directory: Option<Utf8PathBuf>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
}
