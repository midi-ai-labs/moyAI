//! Receiver-owned remote jobs. Execution and history remain in ordinary local sessions.

mod activity;
pub(crate) mod approval;
pub mod artifacts;
pub mod history;
pub mod runtime;
pub mod store;

pub use activity::RemoteActivityProjection;
pub use history::{McpHistoryDetail, McpHistoryDirection, McpHistoryPage, McpHistoryRow};
pub use runtime::{RemoteJobRow, RemoteJobService, RemoteJobState};
pub use store::{RemoteJobParent, RemoteTaskRequest};
