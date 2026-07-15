use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL_BASE_URL: &str = "http://127.0.0.1:1234";
pub const DEFAULT_MODEL_NAME: &str = "qwen/qwen3.6-35b-a3b";
pub const DEFAULT_MODEL_CONTEXT_WINDOW: u32 = 131_072;
pub const DEFAULT_MODEL_MAX_OUTPUT_TOKENS: u32 = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Default,
    AutoReview,
    FullAccess,
}

impl AccessMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "default" => Some(Self::Default),
            "auto_review" => Some(Self::AutoReview),
            "full_access" => Some(Self::FullAccess),
            _ => None,
        }
    }

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMetadataMode {
    #[default]
    LmStudioNativeRequired,
    #[serde(rename = "openai_compatible_only")]
    OpenAiCompatibleOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderApiMode {
    #[default]
    Auto,
    ChatCompletions,
    Responses,
}

impl ProviderApiMode {
    pub const fn resolved_for_provider_metadata_mode(
        self,
        provider_metadata_mode: ProviderMetadataMode,
    ) -> Self {
        match self {
            Self::Auto => match provider_metadata_mode {
                ProviderMetadataMode::LmStudioNativeRequired => Self::Responses,
                ProviderMetadataMode::OpenAiCompatibleOnly => Self::ChatCompletions,
            },
            explicit => explicit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatCompletionsReasoningParameters {
    EffortOnly,
    EffortAndSummary,
}

impl ChatCompletionsReasoningParameters {
    pub const fn supports_summary(self) -> bool {
        matches!(self, Self::EffortAndSummary)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderReasoningCapability {
    #[default]
    Unsupported,
    ChatCompletions {
        parameters: ChatCompletionsReasoningParameters,
    },
    Responses {
        supports_summary: bool,
        supports_previous_response_id: bool,
    },
}

impl ProviderReasoningCapability {
    pub const fn api_mode(self) -> Option<ProviderApiMode> {
        match self {
            Self::Unsupported => None,
            Self::ChatCompletions { .. } => Some(ProviderApiMode::ChatCompletions),
            Self::Responses { .. } => Some(ProviderApiMode::Responses),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
    Custom(String),
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
            Self::Custom(value) => value,
        }
    }
}

impl Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            "" => Err("reasoning effort must not be empty".to_string()),
            custom => Ok(Self::Custom(custom.to_string())),
        }
    }
}

impl Serialize for ReasoningEffort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    #[default]
    None,
    Auto,
    Concise,
    Detailed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiAgentMode {
    #[default]
    ExplicitRequestOnly,
    Proactive,
}

impl MultiAgentMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "explicit_request_only" | "explicit" => Some(Self::ExplicitRequestOnly),
            "proactive" => Some(Self::Proactive),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitRequestOnly => "explicit_request_only",
            Self::Proactive => "proactive",
        }
    }
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
    pub provider_metadata_mode: ProviderMetadataMode,
    pub provider_api_mode: ProviderApiMode,
    pub chat_completions_reasoning_parameters: Option<ChatCompletionsReasoningParameters>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub api_key_env: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u8,
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
    pub overflow_margin_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAgentConfig {
    pub enabled: bool,
    pub mode: MultiAgentMode,
    pub max_concurrent_agents: usize,
    pub max_concurrent_model_requests: usize,
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
    pub hide_windows: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolRouteConfig {
    pub name: String,
    pub effect: crate::tool::ToolEffectClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub enabled: bool,
    pub transport: McpTransportKind,
    pub base_url: String,
    pub timeout_ms: u64,
    #[serde(default)]
    pub tool_routes: Vec<McpToolRouteConfig>,
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
    pub multi_agent: MultiAgentConfig,
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
                base_url: DEFAULT_MODEL_BASE_URL.to_string(),
                model: DEFAULT_MODEL_NAME.to_string(),
                provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
                provider_api_mode: ProviderApiMode::Auto,
                chat_completions_reasoning_parameters: None,
                reasoning_effort: None,
                reasoning_summary: ReasoningSummary::None,
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                extra_headers: BTreeMap::new(),
                request_timeout_ms: 300_000,
                stream_idle_timeout_ms: 300_000,
                connect_timeout_ms: 10_000,
                max_retries: 2,
                context_window: DEFAULT_MODEL_CONTEXT_WINDOW,
                max_output_tokens: DEFAULT_MODEL_MAX_OUTPUT_TOKENS,
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
                extra_body_json: Some(
                    serde_json::json!({ "num_ctx": DEFAULT_MODEL_CONTEXT_WINDOW }),
                ),
            },
            session: SessionConfig {
                overflow_margin_tokens: 1_024,
            },
            multi_agent: MultiAgentConfig {
                enabled: false,
                mode: MultiAgentMode::ExplicitRequestOnly,
                max_concurrent_agents: 4,
                max_concurrent_model_requests: 1,
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
                hide_windows: cfg!(windows),
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
                    tool_routes: Vec::new(),
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

pub fn config_default_provider_profile_lm_studio_fixture_passes() -> bool {
    let config = ResolvedConfig::default();
    config.model.base_url == "http://127.0.0.1:1234"
        && config.model.model == "qwen/qwen3.6-35b-a3b"
        && config.model.provider_metadata_mode == ProviderMetadataMode::LmStudioNativeRequired
        && config.model.context_window == 131_072
        && config.model.max_output_tokens == 8_192
        && config.model.extra_body_json
            == Some(serde_json::json!({
                "num_ctx": 131072
            }))
}

pub fn provider_metadata_mode_default_lm_studio_fixture_passes() -> bool {
    ProviderMetadataMode::default() == ProviderMetadataMode::LmStudioNativeRequired
        && ResolvedConfig::default().model.provider_metadata_mode == ProviderMetadataMode::default()
}

pub fn full_effective_override(config: &ResolvedConfig) -> PartialResolvedConfig {
    PartialResolvedConfig {
        model: Some(PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            provider_metadata_mode: Some(config.model.provider_metadata_mode),
            provider_api_mode: Some(config.model.provider_api_mode),
            chat_completions_reasoning_parameters: config
                .model
                .chat_completions_reasoning_parameters,
            reasoning_effort: config.model.reasoning_effort.clone(),
            reasoning_summary: Some(config.model.reasoning_summary),
            api_key_env: None,
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            stream_idle_timeout_ms: Some(config.model.stream_idle_timeout_ms),
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            context_window: Some(config.model.context_window),
            max_output_tokens: Some(config.model.max_output_tokens),
            temperature: config.model.temperature,
            top_p: config.model.top_p,
            top_k: config.model.top_k,
            presence_penalty: config.model.presence_penalty,
            frequency_penalty: config.model.frequency_penalty,
            seed: config.model.seed,
            stop_sequences: Some(config.model.stop_sequences.clone()),
            supports_tools: Some(config.model.supports_tools),
            supports_reasoning: Some(config.model.supports_reasoning),
            supports_images: Some(config.model.supports_images),
            parallel_tool_calls: Some(config.model.parallel_tool_calls),
            max_parallel_predictions: Some(config.model.max_parallel_predictions),
            extra_body_json: config.model.extra_body_json.clone(),
        }),
        session: None,
        multi_agent: Some(PartialMultiAgentConfig {
            enabled: Some(config.multi_agent.enabled),
            mode: Some(config.multi_agent.mode),
            max_concurrent_agents: Some(config.multi_agent.max_concurrent_agents),
            max_concurrent_model_requests: Some(config.multi_agent.max_concurrent_model_requests),
        }),
        inspection: Some(PartialInspectionConfig {
            default_max_depth: Some(config.inspection.default_max_depth),
            default_max_entries_per_dir: Some(config.inspection.default_max_entries_per_dir),
            max_extensions_reported: Some(config.inspection.max_extensions_reported),
            include_hidden_by_default: Some(config.inspection.include_hidden_by_default),
        }),
        file_guard: Some(PartialFileGuardConfig {
            max_inline_read_bytes: Some(config.file_guard.max_inline_read_bytes),
            large_file_warning_bytes: Some(config.file_guard.large_file_warning_bytes),
            blocked_read_extensions: Some(config.file_guard.blocked_read_extensions.clone()),
            structured_document_extensions: Some(
                config.file_guard.structured_document_extensions.clone(),
            ),
        }),
        docling: Some(PartialDoclingConfig {
            enabled: Some(config.docling.enabled),
            base_url: Some(config.docling.base_url.clone()),
            timeout_ms: Some(config.docling.timeout_ms),
            api_key_env: Some(config.docling.api_key_env.clone()),
            headers: Some(config.docling.headers.clone()),
        }),
        mcp: Some(PartialMcpConfig {
            enabled: Some(config.mcp.enabled),
            servers: Some(config.mcp.servers.clone()),
        }),
        permissions: Some(PartialPermissionsConfig {
            access_mode: Some(config.permissions.access_mode),
            additional_read_roots: Some(config.permissions.additional_read_roots.clone()),
            additional_write_roots: Some(config.permissions.additional_write_roots.clone()),
        }),
        shell: Some(PartialShellConfig {
            program: config.shell.program.clone().map(Some),
            family: config.shell.family.map(Some),
            default_timeout_ms: Some(config.shell.default_timeout_ms),
            max_timeout_ms: Some(config.shell.max_timeout_ms),
            env_allowlist: Some(config.shell.env_allowlist.clone()),
            hide_windows: Some(config.shell.hide_windows),
        }),
        format: None,
        instructions: None,
        workspace: None,
        tool_output: None,
        logging: None,
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialResolvedConfig {
    pub model: Option<PartialModelConfig>,
    pub session: Option<PartialSessionConfig>,
    pub multi_agent: Option<PartialMultiAgentConfig>,
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
#[serde(deny_unknown_fields)]
pub struct PartialModelConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub provider_metadata_mode: Option<ProviderMetadataMode>,
    pub provider_api_mode: Option<ProviderApiMode>,
    pub chat_completions_reasoning_parameters: Option<ChatCompletionsReasoningParameters>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: Option<ReasoningSummary>,
    pub api_key_env: Option<Option<String>>,
    pub extra_headers: Option<BTreeMap<String, String>>,
    pub request_timeout_ms: Option<u64>,
    pub stream_idle_timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    pub max_retries: Option<u8>,
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
#[serde(deny_unknown_fields)]
pub struct PartialSessionConfig {
    pub overflow_margin_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialMultiAgentConfig {
    pub enabled: Option<bool>,
    pub mode: Option<MultiAgentMode>,
    pub max_concurrent_agents: Option<usize>,
    pub max_concurrent_model_requests: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialPermissionsConfig {
    pub access_mode: Option<AccessMode>,
    pub additional_read_roots: Option<Vec<Utf8PathBuf>>,
    pub additional_write_roots: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialShellConfig {
    pub program: Option<Option<Utf8PathBuf>>,
    pub family: Option<Option<ShellFamily>>,
    pub default_timeout_ms: Option<u64>,
    pub max_timeout_ms: Option<u64>,
    pub env_allowlist: Option<Vec<String>>,
    pub hide_windows: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialFormatConfig {
    pub default_newline: Option<NewlineStyle>,
    pub ensure_trailing_newline: Option<bool>,
    pub commands: Option<Vec<FormatterRule>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialInstructionConfig {
    pub additional_files: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialWorkspaceConfig {
    pub extra_ignore_globs: Option<Vec<String>>,
    pub protected_paths: Option<Vec<Utf8PathBuf>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialInspectionConfig {
    pub default_max_depth: Option<usize>,
    pub default_max_entries_per_dir: Option<usize>,
    pub max_extensions_reported: Option<usize>,
    pub include_hidden_by_default: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialFileGuardConfig {
    pub max_inline_read_bytes: Option<u64>,
    pub large_file_warning_bytes: Option<u64>,
    pub blocked_read_extensions: Option<Vec<String>>,
    pub structured_document_extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialDoclingConfig {
    pub enabled: Option<bool>,
    pub base_url: Option<String>,
    pub timeout_ms: Option<u64>,
    pub api_key_env: Option<Option<String>>,
    pub headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialMcpConfig {
    pub enabled: Option<bool>,
    pub servers: Option<Vec<McpServerConfig>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialToolOutputConfig {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialLoggingConfig {
    pub verbosity: Option<LogVerbosity>,
    pub json_logs: Option<bool>,
}

#[cfg(test)]
mod config_contract_tests {
    use super::{McpServerConfig, PartialResolvedConfig, ResolvedConfig, full_effective_override};
    use crate::tool::ToolEffectClass;

    #[test]
    fn effective_override_has_no_empty_session_patch() {
        let patch = full_effective_override(&ResolvedConfig::default());

        assert!(patch.session.is_none());
    }

    #[test]
    fn current_effective_and_session_overrides_round_trip_strictly() {
        let patch = full_effective_override(&ResolvedConfig::default());
        let encoded = toml::to_string(&patch).expect("serialize current effective override");
        let decoded = toml::from_str::<PartialResolvedConfig>(&encoded)
            .expect("current effective override is accepted");
        assert_eq!(
            decoded
                .model
                .as_ref()
                .and_then(|model| model.model.as_deref()),
            Some("qwen/qwen3.6-35b-a3b")
        );

        let session =
            toml::from_str::<PartialResolvedConfig>("[session]\noverflow_margin_tokens = 2048\n")
                .expect("current session override is accepted");
        assert_eq!(
            session
                .session
                .and_then(|session| session.overflow_margin_tokens),
            Some(2048)
        );
    }

    #[test]
    fn every_partial_config_boundary_rejects_unknown_or_retired_keys() {
        for (input, field) in [
            ("[agent]\nretired = true\n", "agent"),
            ("[model]\nprompt_profile = \"auto\"\n", "prompt_profile"),
            ("[session]\nmax_steps_per_turn = 8\n", "max_steps_per_turn"),
            ("[multi_agent]\nmax_depth = 2\n", "max_depth"),
            ("[permissions]\nallow_all = true\n", "allow_all"),
            ("[shell]\ntimeout = 1\n", "timeout"),
            ("[format]\nformatter = \"rustfmt\"\n", "formatter"),
            ("[instructions]\nfiles = []\n", "files"),
            ("[workspace]\nignore = []\n", "ignore"),
            ("[inspection]\nmax_depth = 2\n", "max_depth"),
            ("[file_guard]\ninline_limit = 1\n", "inline_limit"),
            ("[docling]\nurl = \"http://invalid\"\n", "url"),
            ("[mcp]\nroute_allowlist = []\n", "route_allowlist"),
            ("[tool_output]\nmax_chars = 1\n", "max_chars"),
            ("[logging]\nlevel = \"info\"\n", "level"),
        ] {
            let error = toml::from_str::<PartialResolvedConfig>(input)
                .expect_err("unknown config key must fail closed");
            assert!(
                error.to_string().contains(field),
                "error `{error}` did not identify `{field}`"
            );
        }
    }

    #[test]
    fn mcp_routes_require_typed_effects_and_reject_retired_allowlists() {
        let server = toml::from_str::<McpServerConfig>(
            r#"
id = "fixture"
enabled = true
transport = "http"
base_url = "http://127.0.0.1:8123/mcp"
timeout_ms = 1000
headers = {}

[[tool_routes]]
name = "inspect"
effect = "read"
"#,
        )
        .expect("typed MCP route");
        assert_eq!(server.tool_routes[0].effect, ToolEffectClass::Read);

        let retired = toml::from_str::<McpServerConfig>(
            r#"
id = "fixture"
enabled = true
transport = "http"
base_url = "http://127.0.0.1:8123/mcp"
timeout_ms = 1000
tool_allowlist = ["inspect"]
headers = {}
"#,
        );
        assert!(retired.is_err());
    }
}

#[cfg(test)]
mod reasoning_contract_tests {
    use super::{
        ChatCompletionsReasoningParameters, ProviderApiMode, ProviderReasoningCapability,
        ReasoningEffort, ReasoningSummary,
    };

    #[test]
    fn reasoning_effort_uses_provider_wire_strings_and_preserves_future_values() {
        for (wire, expected) in [
            ("none", ReasoningEffort::None),
            ("minimal", ReasoningEffort::Minimal),
            ("low", ReasoningEffort::Low),
            ("medium", ReasoningEffort::Medium),
            ("high", ReasoningEffort::High),
            ("xhigh", ReasoningEffort::XHigh),
            ("max", ReasoningEffort::Max),
            ("ultra", ReasoningEffort::Ultra),
            (
                "provider_future_effort",
                ReasoningEffort::Custom("provider_future_effort".to_string()),
            ),
        ] {
            let parsed = serde_json::from_str::<ReasoningEffort>(&format!("\"{wire}\""))
                .expect("reasoning effort");
            assert_eq!(parsed, expected);
            assert_eq!(
                serde_json::to_string(&parsed).expect("wire effort"),
                format!("\"{wire}\"")
            );
        }
        assert!(serde_json::from_str::<ReasoningEffort>("\"\"").is_err());
    }

    #[test]
    fn provider_api_mode_auto_resolves_from_the_typed_provider_contract() {
        assert_eq!(ProviderApiMode::default(), ProviderApiMode::Auto);
        assert_eq!(
            ProviderApiMode::Auto.resolved_for_provider_metadata_mode(
                super::ProviderMetadataMode::LmStudioNativeRequired,
            ),
            ProviderApiMode::Responses
        );
        assert_eq!(
            ProviderApiMode::Auto.resolved_for_provider_metadata_mode(
                super::ProviderMetadataMode::OpenAiCompatibleOnly,
            ),
            ProviderApiMode::ChatCompletions
        );
        assert_eq!(
            ProviderApiMode::ChatCompletions.resolved_for_provider_metadata_mode(
                super::ProviderMetadataMode::LmStudioNativeRequired,
            ),
            ProviderApiMode::ChatCompletions
        );
        assert_eq!(
            ProviderApiMode::Responses.resolved_for_provider_metadata_mode(
                super::ProviderMetadataMode::OpenAiCompatibleOnly,
            ),
            ProviderApiMode::Responses
        );
    }

    #[test]
    fn provider_reasoning_capability_describes_responses_state_support() {
        let effort_only = ProviderReasoningCapability::ChatCompletions {
            parameters: ChatCompletionsReasoningParameters::EffortOnly,
        };
        let effort_and_summary = ProviderReasoningCapability::ChatCompletions {
            parameters: ChatCompletionsReasoningParameters::EffortAndSummary,
        };
        assert_eq!(
            effort_only.api_mode(),
            Some(ProviderApiMode::ChatCompletions)
        );
        assert_eq!(
            effort_and_summary.api_mode(),
            Some(ProviderApiMode::ChatCompletions)
        );
        assert!(!ChatCompletionsReasoningParameters::EffortOnly.supports_summary());
        assert!(ChatCompletionsReasoningParameters::EffortAndSummary.supports_summary());
        let responses = ProviderReasoningCapability::Responses {
            supports_summary: true,
            supports_previous_response_id: true,
        };
        assert_eq!(responses.api_mode(), Some(ProviderApiMode::Responses));
        assert!(matches!(
            responses,
            ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            }
        ));
        assert_eq!(ProviderReasoningCapability::Unsupported.api_mode(), None);
    }

    #[test]
    fn reasoning_summary_defaults_to_no_wire_parameter() {
        assert_eq!(ReasoningSummary::default(), ReasoningSummary::None);
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Concise).expect("summary"),
            "\"concise\""
        );
    }

    #[test]
    fn model_reasoning_defaults_preserve_provider_defaults_and_output_capability() {
        let model = super::ResolvedConfig::default().model;

        assert_eq!(model.provider_api_mode, ProviderApiMode::Auto);
        assert_eq!(model.chat_completions_reasoning_parameters, None);
        assert_eq!(model.reasoning_effort, None);
        assert_eq!(model.reasoning_summary, ReasoningSummary::None);
        assert!(!model.supports_reasoning);
    }
}
