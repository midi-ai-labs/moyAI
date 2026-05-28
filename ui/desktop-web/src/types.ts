export type RowId = string;

export interface TranscriptRow {
  kind: string;
  step: string;
  title: string;
  body: string;
  file_changes: FileChangeRow[];
}

export interface ProjectRow {
  project_id: RowId;
  label: string;
  path: string;
}

export interface SessionRow {
  session_id: RowId;
  title: string;
  status: "running" | "completed" | "awaiting_user" | "cancelled" | "failed";
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

export type RunStatusKey = "idle" | "running" | "confirming" | "completed" | "awaiting_user" | "cancelled" | "failed";

export interface PermissionProjection {
  summary: string;
  details: string[];
  targets: string[];
  outside_workspace: boolean;
  risks: string[];
}

export interface StartupCheckProjection {
  key: string;
  label: string;
  status: "pending" | "pass" | "warning" | "fail";
  message: string;
}

export interface StartupProjection {
  status: "loading" | "ready" | "requires_config" | "requires_provider";
  title: string;
  message: string;
  detail: string;
  action_overlay: string;
  checks: StartupCheckProjection[];
}

export interface DesktopWebState {
  workspace_path: string;
  provider_label: string;
  model_label: string;
  access_label: string;
  current_session_label: string;
  selected_session_title: string;
  status_message: string;
  status_detail: string;
  run_status_key: RunStatusKey;
  run_status_text: string;
  run_phase: string;
  run_active_step: string;
  latest_tool_summary: string;
  progress_text: string;
  tool_status_text: string;
  confirmation_visible: boolean;
  confirmation_text: string;
  confirmation: PermissionProjection | null;
  startup: StartupProjection;
  draft_prompt: string;
  image_input: string;
  attached_images: string[];
  can_submit: boolean;
  busy: boolean;
  async_polling_required: boolean;
  pending_async_operations: string[];
  navigation_loading: boolean;
  post_run_refresh_pending: boolean;
  background_mutation_pending: boolean;
  overlay: string;
  project_rows: ProjectRow[];
  selected_project_index: number;
  session_rows: SessionRow[];
  chat_session_rows: SessionRow[];
  selected_session_index: number;
  transcript_rows: TranscriptRow[];
  artifact_rows: ArtifactRow[];
  selected_artifact_index: number;
  artifact_preview_text: string;
  file_change_rows: FileChangeRow[];
  file_change_summary_text: string;
  local_search_text: string;
  local_search_results_text: string;
  command_rows: Array<{ name: string; label: string; path: string }>;
  provider_base_url: string;
  provider_metadata_mode: "lm_studio_native_required" | "openai_compatible_only";
  provider_context_window: string;
  provider_max_output_tokens: string;
  provider_models: string[];
  provider_selected_index: number;
  provider_status_text: string;
  provider_selected_model_summary: string[];
  provider_loading: boolean;
  provider_apply_enabled: boolean;
  config_items: string[];
  selected_config_index: number;
  config_field_title: string;
  config_value_text: string;
  config_feedback_text: string;
  workspace_input: string;
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
