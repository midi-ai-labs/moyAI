pub mod catalog;
pub mod contract;
pub mod dto;
pub mod image_validation;
pub mod model_policy;
pub mod model_probe;
pub mod openai_compat;
pub mod provider;
pub mod responses;
pub mod turn_session;

pub use catalog::{ConfigModelCatalog, ModelCatalog};
pub use contract::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelCapabilities,
    ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ProviderToolChoice,
    ReasoningRequest, ResponsesContinuation, ToolSchema,
    control_plane_parallel_tool_calls_projection, effective_parallel_tool_calls,
    tool_surface_scoped_parallel_tool_calls_projection, validate_toolless_text_response,
};
pub use image_validation::{
    ImageValidationError, ValidatedImageMetadata, validate_image_bytes, validate_image_payload,
};
pub use model_probe::{
    ModelAvailabilityReport, ModelAvailabilityStatus, ProviderModelInfo, ProviderModelLoadState,
    apply_provider_model_info_to_config, check_model_availability, extra_body_with_num_ctx,
    fetch_openai_models, fetch_provider_model_infos, normalize_provider_base_url,
    validate_model_availability_report,
};
pub use openai_compat::OpenAiCompatClient;
pub use provider::{
    ProviderFailure, ProviderFailureKind, ProviderPhase, ProviderPhaseEvent, ProviderRequestId,
    ProviderTerminalStatus, resolve_api_key_from_env,
};
