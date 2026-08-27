export type RowId = string;

export interface TranscriptRow {
  row_kind:
    | "empty_placeholder"
    | "user"
    | "assistant"
    | "reasoning_summary"
    | "editing"
    | "tool"
    | "diff"
    | "sub_agent_started"
    | "sub_agent_updated"
    | "sub_agent_interrupted"
    | "system"
    | "error"
    | "work_summary_running"
    | "work_summary_incomplete"
    | "work_summary_completed"
    | "work_summary_failed"
    | "work_summary_cancelled"
    | "file_changes";
  stable_history_identity?: string | null;
  step: string;
  title: string;
  body: string;
  file_changes: FileChangeRow[];
}

export interface PendingTurnInput {
  id: RowId;
  turn_id: RowId;
  text: string;
  image_count: number;
  accepted_at_ms: number;
}

export interface ProjectRow {
  project_id: RowId;
  label: string;
  path: string;
}

export interface SessionRow {
  session_id: RowId;
  title: string;
  status: "idle" | "running" | "completed" | "cancelled" | "failed";
  loaded_status: "not_loaded" | "idle" | "active" | "system_error";
  archived: boolean;
  active_turn_id?: RowId | null;
  active_turn_sequence_no?: number | null;
  admission_revision: string;
  interrupt_target?: StopMutationTarget | null;
  pending_permission_requests: number;
  pending_user_input_requests: number;
  short_id: string;
  label: string;
}

export interface ArtifactRow {
  label: string;
  path: string;
  kind: string;
  action: string;
}

export interface FileChangeRow {
  label: string;
  path: string;
  action: string;
  summary: string;
}

export type AgentStatus =
  | "pending_init"
  | "running"
  | "awaiting_descendants"
  | "interrupted"
  | "completed"
  | "errored"
  | "shutdown";

export interface AgentActivityRow {
  agent_path: string;
  session_id: RowId;
  task_name: string;
  task_preview: string;
  status: AgentStatus;
  current_activity: string;
  result_preview: string;
  started_order: number;
  updated: boolean;
  active_turn_id: RowId | null;
  interrupt_target: AgentInterruptExpectedTarget | null;
}

export interface AgentExecutionExpectedTarget {
  workspacePath: string;
  rootSessionId: RowId;
  agentPath: string;
  childSessionId: RowId;
}

export interface AgentExecutionProjection {
  workspace_path: string;
  root_session_id: RowId;
  agent_path: string;
  session_id: RowId;
  task_name: string;
  transcript_rows: TranscriptRow[];
  turn_page_offset: number;
  turn_page_end: number;
  turn_page_total: number;
  turn_page_has_previous: boolean;
}

export type RunStatusKey = "idle" | "running" | "completed" | "cancelled" | "failed";

export interface PermissionProjection {
  summary: string;
  details: string[];
  targets: string[];
  outside_workspace: boolean;
  risks: string[];
  agent_path?: string | null;
  agent_task_name?: string | null;
}

export interface StartupCheckProjection {
  key: string;
  label: string;
  status: "pass" | "warning" | "fail";
  message: string;
}

export interface StartupProjection {
  status: "ready" | "requires_config" | "requires_provider";
  title: string;
  message: string;
  detail: string;
  action_overlay: string;
  initial_setup_required: boolean;
  initial_setup_reason: "config_missing" | "provider_invalid" | "optional_tool_invalid" | null;
  global_config_path: string | null;
  setup_target: InitialSetupMutationTarget | null;
  checks: StartupCheckProjection[];
}

export interface InitialSetupMutationTarget {
  workspacePath: string;
  globalConfigPath: string;
  setupGeneration: string;
}

export interface ConfigFieldProjection {
  key: string;
  value: string;
  env_override: string | null;
  value_type: "string" | "boolean" | "integer" | "number" | "json" | "enum" | string;
  required: boolean;
  min_value: number | null;
  max_value: number | null;
  options: string[];
}

export interface ConfigMutationTarget {
  workspacePath: string;
  sessionId: string | null;
  configGeneration: string;
}

export interface AccessModeMutationTarget extends ConfigMutationTarget {
  accessMode: "default" | "auto_review" | "full_access";
  runtimeOwnerToken: string;
}

export interface SessionSettingsMutationTarget {
  workspacePath: string;
  rootSessionId: string;
  settingsRevision: string;
  configGeneration: string;
  runtimeOwnerToken: string;
}

export interface SessionSettingsProjection {
  available: boolean;
  base_url: string;
  model: string;
  provider_profile: ProviderProfile;
  api_key_env: string;
  access_mode: "default" | "auto_review" | "full_access";
  context_window: string;
  context_window_inherited: boolean;
  provider_mutation_enabled: boolean;
  access_mutation_enabled: boolean;
  unavailable_reason: string;
  target: SessionSettingsMutationTarget | null;
}

export interface ConfigDraftCapabilityProjection {
  dirty: boolean;
  edit_enabled: boolean;
  discard_enabled: boolean;
  commit_enabled: boolean;
  external_owner_mutation_open: boolean;
  access_mode_mutation_enabled: boolean;
}

export interface ConfigDraftCapabilitiesProjection {
  clean: ConfigDraftCapabilityProjection;
  dirty: ConfigDraftCapabilityProjection;
}

export interface DraftActionTarget {
  workspacePath: string;
  sessionId: string | null;
  ownerGeneration: string;
}

export type RunExpectedState =
  | { kind: "idle"; latestTurnId: string | null; admissionRevision: string }
  | { kind: "turn"; turnId: string; admissionRevision: string };

export interface PromptReviewMutationTarget extends DraftActionTarget {
  requestId: string;
  expectedState: RunExpectedState;
}

export interface CommandPaletteInsertionResult {
  state: DesktopWebState;
  insertionText: string;
}

export interface RunMutationTarget {
  workspacePath: string;
  sessionId: string | null;
  runtimeOwnerToken: string;
  permissionConfirmationId: string | null;
  expectedState: RunExpectedState;
}

export type StopMutationTarget =
  | {
    kind: "root";
    workspacePath: string;
    sessionId: string | null;
    rootGeneration: string;
    latestTurnId: string | null;
    admissionRevision: string;
    permissionConfirmationId: string | null;
  }
  | {
    kind: "turn";
    workspacePath: string;
    sessionId: string;
    turnId: string;
    admissionRevision: string;
    rootEpoch: string;
  };

export interface AgentInterruptExpectedTarget extends AgentExecutionExpectedTarget {
  expectedTurnId: RowId;
  admissionRevision: string;
}

export interface SessionSearchTarget {
  workspacePath: string;
  projectId: string | null;
}

export interface ProviderStatusProjection {
  kind: "idle" | "loading" | "success" | "warning" | "error";
  title: string;
  hint: string;
  details: string;
}

export type DoclingReadinessStatus = "idle" | "checking" | "ready" | "unavailable";

export interface DoclingReadinessProjection {
  status: DoclingReadinessStatus;
  endpoint: string;
  httpStatus: number | null;
  message: string;
}

export type DesktopStatusCode =
  | "plain"
  | "provider_transport"
  | "model_unavailable"
  | "image_unsupported"
  | "image_attachment_invalid"
  | "permission_policy_denied"
  | "config_import_failed"
  | "approval_aborted"
  | "user_stopped"
  | "agent_interrupted"
  | "tree_stopped";

export interface RowMutationTarget {
  workspacePath: string;
  ownerProjectId: string | null;
  ownerSessionId: string | null;
  rowId: string;
}

export type PlanStepStatus = "pending" | "in_progress" | "completed";

export interface PlanStepProjection {
  step: string;
  status: PlanStepStatus;
}

export interface PlanProjection {
  explanation: string | null;
  steps: PlanStepProjection[];
}

export interface DesktopAboutProjection {
  product_name: string;
  version: string;
  license_identifier: string;
  copyright_notice: string;
}

export type SideChatStatus = "idle" | "running" | "completed" | "failed" | "cancelled";

export interface SideChatMessageProjection {
  id: string;
  sequence_no: number;
  role: "user" | "assistant" | "error";
  content: string;
}

export type ProviderProfile =
  | "lm_studio"
  | "openai_compatible"
  | "openai_responses"
  | "lm_studio_chat_completions";

export interface SideChatCatalogModel {
  id: string;
  label: string;
  loadState: "loaded" | "not_loaded" | "unknown";
}

export interface SideChatCatalogResult {
  ownerSessionId: string;
  baseUrl: string;
  providerProfile: ProviderProfile;
  configGeneration: string;
  models: SideChatCatalogModel[];
}

export interface SideChatProjection {
  configured: boolean;
  deleting: boolean;
  chat_id: string | null;
  owner_session_id: string | null;
  model: string;
  base_url: string;
  provider_profile: ProviderProfile | "";
  status: SideChatStatus;
  phase: string;
  last_error: string;
  generation: string;
  draft_text: string;
  draft_revision: string;
  messages: SideChatMessageProjection[];
  can_send: boolean;
  can_cancel: boolean;
}

export type ComposerSubmitMode = "new_request" | "steer" | "blocked";
export type TaskActivityState = "idle" | "running" | "finalizing" | "attention";

export interface DesktopWebState {
  projection_revision: string;
  workspace_path: string;
  provider_label: string;
  model_label: string;
  access_label: string;
  access_target: AccessModeMutationTarget;
  session_settings: SessionSettingsProjection;
  config_draft_capabilities: ConfigDraftCapabilitiesProjection;
  current_session_label: string;
  selected_session_title: string;
  status_message: string;
  status_detail: string;
  status_code: DesktopStatusCode;
  run_status_key: RunStatusKey;
  run_status_text: string;
  run_phase: string;
  run_active_step: string;
  latest_tool_summary: string;
  plan: PlanProjection | null;
  progress_text: string;
  tool_status_text: string;
  token_meter_label: string;
  token_meter_title: string;
  token_meter_level: "unknown" | "low" | "medium" | "high" | "critical" | string;
  confirmation_visible: boolean;
  confirmation_id: string | null;
  confirmation_text: string;
  confirmation: PermissionProjection | null;
  startup: StartupProjection;
  composer_commit_generation: string;
  draft_target: DraftActionTarget;
  draft_prompt: string;
  image_input: string;
  attached_images: string[];
  composer_submit_mode: ComposerSubmitMode;
  can_submit: boolean;
  can_cancel_run: boolean;
  run_target: RunMutationTarget;
  stop_target: StopMutationTarget | null;
  busy: boolean;
  task_activity_state: TaskActivityState;
  async_polling_required: boolean;
  pending_async_operations: string[];
  navigation_loading: boolean;
  navigation_admission_open: boolean;
  turn_page_admission_open: boolean;
  post_run_refresh_pending: boolean;
  background_mutation_pending: boolean;
  overlay: string;
  about: DesktopAboutProjection;
  side_chat: SideChatProjection;
  project_rows: ProjectRow[];
  selected_project_index: number;
  session_rows: SessionRow[];
  chat_session_rows: SessionRow[];
  selected_session_index: number;
  session_search_text: string;
  session_search_include_archived: boolean;
  thread_empty: boolean;
  transcript_rows: TranscriptRow[];
  pending_turn_inputs: PendingTurnInput[];
  turn_page_offset: number;
  turn_page_limit: number;
  turn_page_total: number;
  turn_page_has_more: boolean;
  artifact_rows: ArtifactRow[];
  selected_artifact_index: number;
  artifact_preview_available: boolean;
  artifact_preview_text: string;
  file_change_rows: FileChangeRow[];
  file_change_summary_text: string;
  agent_activity_rows: AgentActivityRow[];
  current_turn_agent_activity_rows: AgentActivityRow[];
  agent_tree_active: boolean;
  local_search_text: string;
  local_search_results_text: string;
  command_rows: Array<{ name: string; label: string; path: string }>;
  provider_base_url: string;
  provider_profile: ProviderProfile;
  provider_api_key_env: string;
  provider_effective_base_url: string;
  provider_effective_profile: ProviderProfile;
  provider_effective_api_key_env: string;
  provider_effective_context_window: string;
  provider_effective_model_id: string;
  provider_catalog_base_url: string | null;
  provider_catalog_profile: ProviderProfile | null;
  provider_catalog_api_key_env: string | null;
  provider_context_window: string;
  provider_models: string[];
  provider_model_ids: string[];
  provider_selected_index: number;
  provider_status: ProviderStatusProjection;
  provider_selected_model_summary: string[];
  provider_loading: boolean;
  provider_apply_enabled: boolean;
  docling_readiness: DoclingReadinessProjection;
  config_fields: ConfigFieldProjection[];
  config_target: ConfigMutationTarget;
  workspace_input: string;
  review_target: PromptReviewMutationTarget | null;
  review_raw_text: string;
  review_draft_text: string;
  review_status_text: string;
  send_enhanced_enabled: boolean;
  send_raw_enabled: boolean;
  history_export_enabled: boolean;
  enhance_enabled: boolean;
  image_input_enabled: boolean;
  window_opacity_percent: number;
}

export interface DesktopViewState extends DesktopWebState {
  config_draft: ConfigDraftCapabilityProjection;
}
