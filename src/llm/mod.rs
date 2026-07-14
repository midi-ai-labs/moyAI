pub mod catalog;
pub mod contract;
pub mod dto;
pub mod model_probe;
pub mod openai_compat;

pub use catalog::{ConfigModelCatalog, ModelCatalog};
pub use contract::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelCapabilities,
    ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ProviderToolChoice, ToolSchema,
    control_plane_parallel_tool_calls_projection, effective_parallel_tool_calls,
    tool_surface_scoped_parallel_tool_calls_projection, validate_toolless_text_response,
};
pub use model_probe::{
    ModelAvailabilityReport, ModelAvailabilityStatus, ProviderModelInfo, ToolCallProbeReport,
    apply_model_availability_report_to_config, apply_provider_model_info_to_config,
    check_model_availability, extra_body_with_num_ctx, fetch_openai_models,
    fetch_provider_model_infos, normalize_provider_base_url,
};
pub use openai_compat::OpenAiCompatClient;
