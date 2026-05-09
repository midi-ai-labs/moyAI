use super::model::{
    PartialAgentConfig, PartialDoclingConfig, PartialFileGuardConfig, PartialFormatConfig,
    PartialInspectionConfig, PartialInstructionConfig, PartialLoggingConfig, PartialMcpConfig,
    PartialModelConfig, PartialPermissionsConfig, PartialResolvedConfig, PartialSessionConfig,
    PartialShellConfig, PartialToolOutputConfig, PartialWorkspaceConfig, ResolvedConfig,
};

fn apply_model(target: &mut crate::config::ModelConfig, patch: PartialModelConfig) {
    if let Some(value) = patch.base_url {
        target.base_url = value;
    }
    if let Some(value) = patch.model {
        target.model = value;
    }
    if let Some(value) = patch.prompt_profile {
        target.prompt_profile = value;
    }
    if let Some(value) = patch.api_key_env {
        target.api_key_env = value;
    }
    if let Some(value) = patch.extra_headers {
        target.extra_headers = value;
    }
    if let Some(value) = patch.request_timeout_ms {
        target.request_timeout_ms = value;
    }
    if let Some(value) = patch.stream_idle_timeout_ms {
        target.stream_idle_timeout_ms = value;
    }
    if let Some(value) = patch.connect_timeout_ms {
        target.connect_timeout_ms = value;
    }
    if let Some(value) = patch.max_retries {
        target.max_retries = value;
    }
    if let Some(value) = patch.stream_max_retries {
        target.stream_max_retries = value;
    }
    if let Some(value) = patch.context_window {
        target.context_window = value;
    }
    if let Some(value) = patch.max_output_tokens {
        target.max_output_tokens = value;
    }
    if let Some(value) = patch.temperature {
        target.temperature = Some(value);
    }
    if let Some(value) = patch.top_p {
        target.top_p = Some(value);
    }
    if let Some(value) = patch.top_k {
        target.top_k = Some(value);
    }
    if let Some(value) = patch.presence_penalty {
        target.presence_penalty = Some(value);
    }
    if let Some(value) = patch.frequency_penalty {
        target.frequency_penalty = Some(value);
    }
    if let Some(value) = patch.seed {
        target.seed = Some(value);
    }
    if let Some(value) = patch.stop_sequences {
        target.stop_sequences = value;
    }
    if let Some(value) = patch.supports_tools {
        target.supports_tools = value;
    }
    if let Some(value) = patch.supports_reasoning {
        target.supports_reasoning = value;
    }
    if let Some(value) = patch.supports_images {
        target.supports_images = value;
    }
    if let Some(value) = patch.parallel_tool_calls {
        target.parallel_tool_calls = value;
    }
    if let Some(value) = patch.max_parallel_predictions {
        target.max_parallel_predictions = value.max(1);
    }
    if let Some(value) = patch.extra_body_json {
        target.extra_body_json = Some(value);
    }
}

fn apply_session(target: &mut crate::config::SessionConfig, patch: PartialSessionConfig) {
    if let Some(value) = patch.default_title_max_len {
        target.default_title_max_len = value;
    }
    if let Some(value) = patch.transcript_limit_messages {
        target.transcript_limit_messages = value;
    }
    if let Some(value) = patch.auto_resume_last {
        target.auto_resume_last = value;
    }
    if let Some(value) = patch.max_steps_per_turn {
        target.max_steps_per_turn = value;
    }
    if let Some(value) = patch.overflow_margin_tokens {
        target.overflow_margin_tokens = value;
    }
}

fn apply_agent(target: &mut crate::config::AgentConfig, patch: PartialAgentConfig) {
    if let Some(value) = patch.duplicate_success_abort_threshold {
        target.duplicate_success_abort_threshold = value;
    }
    if let Some(value) = patch.repetitive_text_line_threshold {
        target.repetitive_text_line_threshold = value;
    }
    if let Some(value) = patch.readonly_stall_threshold_implementation {
        target.readonly_stall_threshold_implementation = value;
    }
    if let Some(value) = patch.readonly_stall_threshold_general {
        target.readonly_stall_threshold_general = value;
    }
    if let Some(value) = patch.verification_repair_grace_steps {
        target.verification_repair_grace_steps = value;
    }
    if let Some(value) = patch.verification_failure_attempt_limit {
        target.verification_failure_attempt_limit = value;
    }
    if let Some(value) = patch.verification_failure_repair_read_budget {
        target.verification_failure_repair_read_budget = value;
    }
    if let Some(value) = patch.staged_task_documentation_finish_grace_steps {
        target.staged_task_documentation_finish_grace_steps = value;
    }
    if let Some(value) = patch.staged_task_discovery_redirect_repeat_threshold {
        target.staged_task_discovery_redirect_repeat_threshold = value;
    }
    if let Some(value) = patch.staged_task_authoring_read_limit {
        target.staged_task_authoring_read_limit = value;
    }
    if let Some(value) = patch.staged_task_authoring_successful_read_budget_after_progress {
        target.staged_task_authoring_successful_read_budget_after_progress = value;
    }
    if let Some(value) = patch.staged_task_audit_repair_read_budget {
        target.staged_task_audit_repair_read_budget = value;
    }
    if let Some(value) = patch.staged_task_audit_repair_rewrite_escalation_threshold {
        target.staged_task_audit_repair_rewrite_escalation_threshold = value;
    }
    if let Some(value) = patch.staged_task_recovery_stall_threshold {
        target.staged_task_recovery_stall_threshold = value;
    }
}

fn apply_permissions(
    target: &mut crate::config::PermissionsConfig,
    patch: PartialPermissionsConfig,
) {
    if let Some(value) = patch.access_mode {
        target.access_mode = value;
    }
    if let Some(value) = patch.additional_read_roots {
        target.additional_read_roots = value;
    }
    if let Some(value) = patch.additional_write_roots {
        target.additional_write_roots = value;
    }
}

fn apply_shell(target: &mut crate::config::ShellConfig, patch: PartialShellConfig) {
    if let Some(value) = patch.program {
        target.program = value;
    }
    if let Some(value) = patch.family {
        target.family = value;
    }
    if let Some(value) = patch.default_timeout_ms {
        target.default_timeout_ms = value;
    }
    if let Some(value) = patch.max_timeout_ms {
        target.max_timeout_ms = value;
    }
    if let Some(value) = patch.env_allowlist {
        target.env_allowlist = value;
    }
}

fn apply_format(target: &mut crate::config::FormatConfig, patch: PartialFormatConfig) {
    if let Some(value) = patch.default_newline {
        target.default_newline = value;
    }
    if let Some(value) = patch.ensure_trailing_newline {
        target.ensure_trailing_newline = value;
    }
    if let Some(value) = patch.commands {
        target.commands = value;
    }
}

fn apply_instructions(
    target: &mut crate::config::InstructionConfig,
    patch: PartialInstructionConfig,
) {
    if let Some(value) = patch.additional_files {
        target.additional_files = value;
    }
}

fn apply_workspace(target: &mut crate::config::WorkspaceConfig, patch: PartialWorkspaceConfig) {
    if let Some(value) = patch.extra_ignore_globs {
        target.extra_ignore_globs = value;
    }
    if let Some(value) = patch.protected_paths {
        target.protected_paths = value;
    }
}

fn apply_inspection(
    target: &mut crate::config::model::InspectionConfig,
    patch: PartialInspectionConfig,
) {
    if let Some(value) = patch.default_max_depth {
        target.default_max_depth = value;
    }
    if let Some(value) = patch.default_max_entries_per_dir {
        target.default_max_entries_per_dir = value;
    }
    if let Some(value) = patch.max_extensions_reported {
        target.max_extensions_reported = value;
    }
    if let Some(value) = patch.include_hidden_by_default {
        target.include_hidden_by_default = value;
    }
}

fn apply_file_guard(
    target: &mut crate::config::model::FileGuardConfig,
    patch: PartialFileGuardConfig,
) {
    if let Some(value) = patch.max_inline_read_bytes {
        target.max_inline_read_bytes = value;
    }
    if let Some(value) = patch.large_file_warning_bytes {
        target.large_file_warning_bytes = value;
    }
    if let Some(value) = patch.blocked_read_extensions {
        target.blocked_read_extensions = value;
    }
    if let Some(value) = patch.structured_document_extensions {
        target.structured_document_extensions = value;
    }
}

fn apply_docling(target: &mut crate::config::model::DoclingConfig, patch: PartialDoclingConfig) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.base_url {
        target.base_url = value;
    }
    if let Some(value) = patch.timeout_ms {
        target.timeout_ms = value;
    }
    if let Some(value) = patch.api_key_env {
        target.api_key_env = value;
    }
    if let Some(value) = patch.headers {
        target.headers = value;
    }
}

fn apply_mcp(target: &mut crate::config::model::McpConfig, patch: PartialMcpConfig) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.servers {
        target.servers = value;
    }
}

fn apply_tool_output(target: &mut crate::config::ToolOutputConfig, patch: PartialToolOutputConfig) {
    if let Some(value) = patch.max_lines {
        target.max_lines = value;
    }
    if let Some(value) = patch.max_bytes {
        target.max_bytes = value;
    }
    if let Some(value) = patch.max_results {
        target.max_results = value;
    }
}

fn apply_logging(target: &mut crate::config::LoggingConfig, patch: PartialLoggingConfig) {
    if let Some(value) = patch.verbosity {
        target.verbosity = value;
    }
    if let Some(value) = patch.json_logs {
        target.json_logs = value;
    }
}

pub fn apply_patch(mut target: ResolvedConfig, patch: PartialResolvedConfig) -> ResolvedConfig {
    if let Some(value) = patch.model {
        apply_model(&mut target.model, value);
    }
    if let Some(value) = patch.session {
        apply_session(&mut target.session, value);
    }
    if let Some(value) = patch.agent {
        apply_agent(&mut target.agent, value);
    }
    if let Some(value) = patch.permissions {
        apply_permissions(&mut target.permissions, value);
    }
    if let Some(value) = patch.shell {
        apply_shell(&mut target.shell, value);
    }
    if let Some(value) = patch.format {
        apply_format(&mut target.format, value);
    }
    if let Some(value) = patch.instructions {
        apply_instructions(&mut target.instructions, value);
    }
    if let Some(value) = patch.workspace {
        apply_workspace(&mut target.workspace, value);
    }
    if let Some(value) = patch.inspection {
        apply_inspection(&mut target.inspection, value);
    }
    if let Some(value) = patch.file_guard {
        apply_file_guard(&mut target.file_guard, value);
    }
    if let Some(value) = patch.docling {
        apply_docling(&mut target.docling, value);
    }
    if let Some(value) = patch.mcp {
        apply_mcp(&mut target.mcp, value);
    }
    if let Some(value) = patch.tool_output {
        apply_tool_output(&mut target.tool_output, value);
    }
    if let Some(value) = patch.logging {
        apply_logging(&mut target.logging, value);
    }
    target
}
