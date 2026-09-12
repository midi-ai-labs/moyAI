// Read-only legacy projection and shared Hub target/job wire types.
// The manual publishing editor and its command boundary are retired.
export type PublishTool = "list" | "glob" | "grep" | "read" | "inspect_directory" | "current_time";
export type PublishBackground = "stop_when_window_closes" | "keep_while_application_running";
export type PublishMode = { kind: "read_tools" }
  | { kind: "agent"; access_mode: "default" | "auto_review" | "full_access" };
export interface PublishTls { certificate_path: string; private_key_path: string }
export type PublishTarget = { kind: "project"; project_id: string; workspace_root: string }
  | { kind: "temp" }
  | { kind: "legacy_session"; project_id: string; root_session_id: string; workspace_root: string };
export interface PublishProfileDraft {
  label: string;
  bind: string;
  mode: PublishMode;
  tls: PublishTls | null;
  target: PublishTarget | null;
  tools: PublishTool[];
  max_concurrent_calls: number;
  background: PublishBackground;
}
export interface PublishProfile extends Omit<PublishProfileDraft, "target"> {
  target: PublishTarget;
  id: string;
  enabled: boolean;
  transport: "streamable_http";
  authentication: { kind: "unpaired" } | { kind: "local_credential"; credential_id: string };
}
export interface PublishProfileRow {
  profile: PublishProfile;
  status: "stopped" | "starting" | "running" | "stopping" | "error";
  status_message: string | null;
  endpoint: string | null;
  active_calls: number;
  connected_sessions: number;
  recent_calls: { id: string; tool: string; status: string }[];
  credential_configured: boolean;
  can_edit: boolean;
  can_delete: boolean;
  can_start: boolean;
  can_stop: boolean;
  can_issue_token: boolean;
  can_revoke_token: boolean;
}
export interface PublishProjection {
  revision: string;
  generation: string;
  profiles: PublishProfileRow[];
  targets: { target: PublishTarget; label: string }[];
  error: string | null;
}
export interface PublishJob {
  job_id: string; profile_id: string;
  parent: { peer_id: string; task_id: string; turn_id: string };
  prompt_preview: string; session_id: string;
  state: "accepted" | "running" | "awaiting_approval" | "cancelling" | "completed" | "failed" | "interrupted";
  model: string; result: string | null; result_truncated: boolean; can_stop: boolean;
}
