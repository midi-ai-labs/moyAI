pub mod apply_patch;
pub mod context;
pub mod contract;
pub mod current_time;
pub mod docling_convert;
pub mod goal;
pub mod inspect_directory;
pub(crate) mod internal_output;
pub mod mcp_call;
pub mod multi_agent;
pub(crate) mod permission_review;
pub(crate) mod process;
pub mod read;
pub mod registry;
pub mod search;
pub mod shell;
pub mod skill;
pub(crate) mod text_encoding;
pub mod todo_write;
pub mod truncate;
pub mod write;
pub(crate) mod write_support;

pub use contract::{
    PermissionRequest, PermissionRisk, ToolName, ToolResult, ToolSpec, TruncatedToolOutput,
};

pub(crate) fn structured_document_suggested_tools(
    config: &crate::config::ResolvedConfig,
) -> Vec<String> {
    let mut tools = Vec::new();
    if config.docling.enabled {
        tools.push("docling_convert".to_string());
    }
    if config.mcp.enabled {
        tools.push("mcp_call".to_string());
    }
    tools.push("inspect_directory".to_string());
    tools
}

pub(crate) fn structured_document_guidance(config: &crate::config::ResolvedConfig) -> String {
    let mut guidance = Vec::new();
    if config.docling.enabled {
        guidance.push(
            "Use `docling_convert` with the configured Docling Serve backend to extract markdown or text."
                .to_string(),
        );
    }
    if config.mcp.enabled {
        guidance.push(
            "Use `mcp_call` with a configured MCP document server when you need that workflow."
                .to_string(),
        );
    }
    guidance.push(
        "Stay metadata-first with `inspect_directory` until you know which file to process."
            .to_string(),
    );
    guidance.join(" ")
}
