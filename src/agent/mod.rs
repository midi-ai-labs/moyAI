pub mod compaction;
pub(crate) mod completion_guard;
pub(crate) mod content_shape_contract;
pub(crate) mod contract_reconciliation;
pub mod event;
pub mod loop_impl;
pub mod prompt;
pub(crate) mod prompt_assets;
pub(crate) mod repair_lane;
pub(crate) mod state;
pub(crate) mod tool_orchestrator;
pub(crate) mod tool_result_classification;
pub(crate) mod turn_decision;
pub(crate) mod verification;

pub use loop_impl::AgentLoop;
pub use prompt::{AgentRunRequest, PromptBuilder, PromptBundle, RuntimeInputView};
