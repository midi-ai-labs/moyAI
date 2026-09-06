//! Receiver-owned remote jobs. Execution and history remain in ordinary local sessions.

pub mod runtime;
pub mod store;

pub use runtime::{RemoteJobRow, RemoteJobService, RemoteJobState};
pub use store::{RemoteJobParent, RemoteTaskRequest};
