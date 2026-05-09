pub mod catalog;
pub mod contract;
pub mod dto;
pub mod model_probe;
pub mod openai_compat;

pub use catalog::{ConfigModelCatalog, ModelCatalog};
pub use contract::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelCapabilities,
    ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ToolSchema,
};
pub use model_probe::{
    ModelAvailabilityReport, ModelAvailabilityStatus, ProviderModelInfo,
    apply_provider_model_info_to_config, check_model_availability, ensure_openai_model_available,
    fetch_openai_models, fetch_provider_model_infos, normalize_provider_base_url,
};
pub use openai_compat::OpenAiCompatClient;
