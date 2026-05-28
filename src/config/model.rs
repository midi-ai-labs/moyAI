use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Default,
    AutoReview,
    FullAccess,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoReview => "auto_review",
            Self::FullAccess => "full_access",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::AutoReview => "Auto Review",
            Self::FullAccess => "Full Access",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::AutoReview,
            Self::AutoReview => Self::FullAccess,
            Self::FullAccess => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellFamily {
    Bash,
    PowerShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewlineStyle {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogVerbosity {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptProfile {
    Auto,
    Default,
    QwenCoder,
}

impl PromptProfile {
    pub fn resolved_for_model(self, model_name: &str) -> Self {
        match self {
            PromptProfile::Auto => {
                if model_name.to_ascii_lowercase().contains("qwen") {
                    PromptProfile::QwenCoder
                } else {
                    PromptProfile::Default
                }
            }
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMetadataMode {
    #[default]
    LmStudioNativeRequired,
    #[serde(rename = "openai_compatible_only")]
    OpenAiCompatibleOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatterRule {
    pub glob: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub base_url: String,
    pub model: String,
    pub prompt_profile: PromptProfile,
    pub provider_metadata_mode: ProviderMetadataMode,
    pub api_key_env: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u8,
    pub stream_max_retries: u8,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<u64>,
    pub stop_sequences: Vec<String>,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_images: bool,
    pub parallel_tool_calls: bool,
    pub max_parallel_predictions: u32,
    pub extra_body_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub default_title_max_len: usize,
    pub transcript_limit_messages: usize,
    pub auto_resume_last: bool,
    pub max_steps_per_turn: usize,
    pub overflow_margin_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub duplicate_success_abort_threshold: usize,
    pub repetitive_text_line_threshold: usize,
    pub readonly_stall_threshold_implementation: usize,
    pub readonly_stall_threshold_general: usize,
    pub verification_repair_grace_steps: usize,
    pub verification_failure_attempt_limit: usize,
    pub verification_failure_repair_read_budget: usize,
    pub staged_task_documentation_finish_grace_steps: usize,
    pub staged_task_discovery_redirect_repeat_threshold: usize,
    pub staged_task_authoring_read_limit: u64,
    pub staged_task_authoring_successful_read_budget_after_progress: usize,
    pub staged_task_audit_repair_read_budget: usize,
    pub staged_task_audit_repair_rewrite_escalation_threshold: usize,
    pub staged_task_recovery_stall_threshold: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    pub access_mode: AccessMode,
    pub additional_read_roots: Vec<Utf8PathBuf>,
    pub additional_write_roots: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    pub program: Option<Utf8PathBuf>,
    pub family: Option<ShellFamily>,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub env_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatConfig {
    pub default_newline: NewlineStyle,
    pub ensure_trailing_newline: bool,
    pub commands: Vec<FormatterRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionConfig {
    pub additional_files: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub extra_ignore_globs: Vec<String>,
    pub protected_paths: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionConfig {
    pub default_max_depth: usize,
    pub default_max_entries_per_dir: usize,
    pub max_extensions_reported: usize,
    pub include_hidden_by_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGuardConfig {
    pub max_inline_read_bytes: u64,
    pub large_file_warning_bytes: u64,
    pub blocked_read_extensions: Vec<String>,
    pub structured_document_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoclingConfig {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_ms: u64,
    pub api_key_env: Option<String>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub enabled: bool,
    pub transport: McpTransportKind,
    pub base_url: String,
    pub timeout_ms: u64,
    pub route_allowlist: Vec<String>,
    pub tool_allowlist: Vec<String>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub enabled: bool,
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutputConfig {
    pub max_lines: usize,
    pub max_bytes: usize,
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub verbosity: LogVerbosity,
    pub json_logs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub model: ModelConfig,
    pub session: SessionConfig,
    pub agent: AgentConfig,
    pub permissions: PermissionsConfig,
    pub shell: ShellConfig,
    pub format: FormatConfig,
    pub instructions: InstructionConfig,
    pub workspace: WorkspaceConfig,
    pub inspection: InspectionConfig,
    pub file_guard: FileGuardConfig,
    pub docling: DoclingConfig,
    pub mcp: McpConfig,
    pub tool_output: ToolOutputConfig,
    pub logging: LoggingConfig,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        let default_shell_family = if cfg!(windows) {
            ShellFamily::PowerShell
        } else {
            ShellFamily::Bash
        };
        let default_newline = if cfg!(windows) {
            NewlineStyle::Crlf
        } else {
            NewlineStyle::Lf
        };
        let default_shell_env_allowlist = if cfg!(windows) {
            vec![
                "PATH".to_string(),
                "PATHEXT".to_string(),
                "SystemRoot".to_string(),
                "ComSpec".to_string(),
                "USERPROFILE".to_string(),
                "USERNAME".to_string(),
                "LOCALAPPDATA".to_string(),
                "APPDATA".to_string(),
                "HOMEDRIVE".to_string(),
                "HOMEPATH".to_string(),
                "CARGO_HOME".to_string(),
                "RUSTUP_HOME".to_string(),
                "RUSTUP_TOOLCHAIN".to_string(),
                "TMP".to_string(),
                "TEMP".to_string(),
            ]
        } else {
            vec![
                "PATH".to_string(),
                "HOME".to_string(),
                "USER".to_string(),
                "SHELL".to_string(),
                "LANG".to_string(),
                "LC_ALL".to_string(),
                "TMPDIR".to_string(),
                "TMP".to_string(),
                "TEMP".to_string(),
            ]
        };

        Self {
            model: ModelConfig {
                base_url: "http://127.0.0.1:1234".to_string(),
                model: "qwen/qwen3.6-35b-a3b".to_string(),
                prompt_profile: PromptProfile::Auto,
                provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                extra_headers: BTreeMap::new(),
                request_timeout_ms: 600_000,
                stream_idle_timeout_ms: 300_000,
                connect_timeout_ms: 10_000,
                max_retries: 2,
                stream_max_retries: 2,
                context_window: 131_072,
                max_output_tokens: 8_192,
                temperature: None,
                top_p: None,
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                seed: None,
                stop_sequences: Vec::new(),
                supports_tools: true,
                supports_reasoning: false,
                supports_images: true,
                parallel_tool_calls: true,
                max_parallel_predictions: 1,
                extra_body_json: Some(serde_json::json!({ "num_ctx": 131072 })),
            },
            session: SessionConfig {
                default_title_max_len: 80,
                transcript_limit_messages: 200,
                auto_resume_last: false,
                max_steps_per_turn: 128,
                overflow_margin_tokens: 1_024,
            },
            agent: AgentConfig {
                duplicate_success_abort_threshold: 6,
                repetitive_text_line_threshold: 6,
                readonly_stall_threshold_implementation: 3,
                readonly_stall_threshold_general: 4,
                verification_repair_grace_steps: 4,
                verification_failure_attempt_limit: 4,
                verification_failure_repair_read_budget: 2,
                staged_task_documentation_finish_grace_steps: 128,
                staged_task_discovery_redirect_repeat_threshold: 2,
                staged_task_authoring_read_limit: 160,
                staged_task_authoring_successful_read_budget_after_progress: 3,
                staged_task_audit_repair_read_budget: 3,
                staged_task_audit_repair_rewrite_escalation_threshold: 2,
                staged_task_recovery_stall_threshold: 3,
            },
            permissions: PermissionsConfig {
                access_mode: AccessMode::Default,
                additional_read_roots: Vec::new(),
                additional_write_roots: Vec::new(),
            },
            shell: ShellConfig {
                program: None,
                family: Some(default_shell_family),
                default_timeout_ms: 120_000,
                max_timeout_ms: 600_000,
                env_allowlist: default_shell_env_allowlist,
            },
            format: FormatConfig {
                default_newline,
                ensure_trailing_newline: true,
                commands: Vec::new(),
            },
            instructions: InstructionConfig {
                additional_files: Vec::new(),
            },
            workspace: WorkspaceConfig {
                extra_ignore_globs: Vec::new(),
                protected_paths: Vec::new(),
            },
            inspection: InspectionConfig {
                default_max_depth: 4,
                default_max_entries_per_dir: 64,
                max_extensions_reported: 32,
                include_hidden_by_default: false,
            },
            file_guard: FileGuardConfig {
                max_inline_read_bytes: 256 * 1024,
                large_file_warning_bytes: 5 * 1024 * 1024,
                blocked_read_extensions: vec![
                    "arrow".to_string(),
                    "bin".to_string(),
                    "ckpt".to_string(),
                    "feather".to_string(),
                    "joblib".to_string(),
                    "npy".to_string(),
                    "npz".to_string(),
                    "onnx".to_string(),
                    "parquet".to_string(),
                    "pkl".to_string(),
                    "pickle".to_string(),
                    "pt".to_string(),
                    "pth".to_string(),
                    "safetensors".to_string(),
                ],
                structured_document_extensions: vec![
                    "docx".to_string(),
                    "pdf".to_string(),
                    "pptx".to_string(),
                    "xlsx".to_string(),
                ],
            },
            docling: DoclingConfig {
                enabled: false,
                base_url: "http://127.0.0.1:8123".to_string(),
                timeout_ms: 120_000,
                api_key_env: Some("DOCLING_API_KEY".to_string()),
                headers: BTreeMap::new(),
            },
            mcp: McpConfig {
                enabled: false,
                servers: vec![McpServerConfig {
                    id: "docling".to_string(),
                    enabled: false,
                    transport: McpTransportKind::Http,
                    base_url: "http://127.0.0.1:8123/mcp".to_string(),
                    timeout_ms: 120_000,
                    route_allowlist: vec![
                        "ask".to_string(),
                        "docs".to_string(),
                        "review".to_string(),
                    ],
                    tool_allowlist: Vec::new(),
                    headers: BTreeMap::new(),
                }],
            },
            tool_output: ToolOutputConfig {
                max_lines: 2_000,
                max_bytes: 50 * 1024,
                max_results: 100,
            },
            logging: LoggingConfig {
                verbosity: LogVerbosity::Info,
                json_logs: false,
            },
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialResolvedConfig {
    pub model: Option<PartialModelConfig>,
    pub session: Option<PartialSessionConfig>,
    pub agent: Option<PartialAgentConfig>,
    pub permissions: Option<PartialPermissionsConfig>,
    pub shell: Option<PartialShellConfig>,
    pub format: Option<PartialFormatConfig>,
    pub instructions: Option<PartialInstructionConfig>,
    pub workspace: Option<PartialWorkspaceConfig>,
    pub inspection: Option<PartialInspectionConfig>,
    pub file_guard: Option<PartialFileGuardConfig>,
    pub docling: Option<PartialDoclingConfig>,
    pub mcp: Option<PartialMcpConfig>,
    pub tool_output: Option<PartialToolOutputConfig>,
    pub logging: Option<PartialLoggingConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialModelConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub prompt_profile: Option<PromptProfile>,
    pub provider_metadata_mode: Option<ProviderMetadataMode>,
    pub api_key_env: Option<Option<String>>,
    pub extra_headers: Option<BTreeMap<String, String>>,
    pub request_timeout_ms: Option<u64>,
    pub stream_idle_timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    pub max_retries: Option<u8>,
    pub stream_max_retries: Option<u8>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<u64>,
    pub stop_sequences: Option<Vec<String>>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub supports_images: Option<bool>,
    pub parallel_tool_calls: Option<bool>,
    pub max_parallel_predictions: Option<u32>,
    pub extra_body_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialSessionConfig {
    pub default_title_max_len: Option<usize>,
    pub transcript_limit_messages: Option<usize>,
    pub auto_resume_last: Option<bool>,
    pub max_steps_per_turn: Option<usize>,
    pub overflow_margin_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialAgentConfig {
    pub duplicate_success_abort_threshold: Option<usize>,
    pub repetitive_text_line_threshold: Option<usize>,
    pub readonly_stall_threshold_implementation: Option<usize>,
    pub readonly_stall_threshold_general: Option<usize>,
    pub verification_repair_grace_steps: Option<usize>,
    pub verification_failure_attempt_limit: Option<usize>,
    pub verification_failure_repair_read_budget: Option<usize>,
    pub staged_task_documentation_finish_grace_steps: Option<usize>,
    pub staged_task_discovery_redirect_repeat_threshold: Option<usize>,
    pub staged_task_authoring_read_limit: Option<u64>,
    pub staged_task_authoring_successful_read_budget_after_progress: Option<usize>,
    pub staged_task_audit_repair_read_budget: Option<usize>,
    pub staged_task_audit_repair_rewrite_escalation_threshold: Option<usize>,
    pub staged_task_recovery_stall_threshold: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialPermissionsConfig {
    pub access_mode: Option<AccessMode>,
    pub additional_read_roots: Option<Vec<Utf8PathBuf>>,
    pub additional_write_roots: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialShellConfig {
    pub program: Option<Option<Utf8PathBuf>>,
    pub family: Option<Option<ShellFamily>>,
    pub default_timeout_ms: Option<u64>,
    pub max_timeout_ms: Option<u64>,
    pub env_allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialFormatConfig {
    pub default_newline: Option<NewlineStyle>,
    pub ensure_trailing_newline: Option<bool>,
    pub commands: Option<Vec<FormatterRule>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialInstructionConfig {
    pub additional_files: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialWorkspaceConfig {
    pub extra_ignore_globs: Option<Vec<String>>,
    pub protected_paths: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialInspectionConfig {
    pub default_max_depth: Option<usize>,
    pub default_max_entries_per_dir: Option<usize>,
    pub max_extensions_reported: Option<usize>,
    pub include_hidden_by_default: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialFileGuardConfig {
    pub max_inline_read_bytes: Option<u64>,
    pub large_file_warning_bytes: Option<u64>,
    pub blocked_read_extensions: Option<Vec<String>>,
    pub structured_document_extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialDoclingConfig {
    pub enabled: Option<bool>,
    pub base_url: Option<String>,
    pub timeout_ms: Option<u64>,
    pub api_key_env: Option<Option<String>>,
    pub headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialMcpConfig {
    pub enabled: Option<bool>,
    pub servers: Option<Vec<McpServerConfig>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialToolOutputConfig {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PartialLoggingConfig {
    pub verbosity: Option<LogVerbosity>,
    pub json_logs: Option<bool>,
}
