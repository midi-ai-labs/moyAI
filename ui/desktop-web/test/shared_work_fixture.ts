import { createSharedWorkUiState, type SharedWorkProjection } from "../src/shared_work_state.ts";
export function sharedProjection(overrides: Partial<SharedWorkProjection> = {}): SharedWorkProjection {
  return { revision: "1", generation: "1", connected: true, hub_url: "https://hub.test:8443", enrollment: "active", enrollment_error: null,
    principal: { user_id: "user-a", display_name: "利用者 A", administrator: false }, expires_at_ms: 9_999_999_999_999,
    projects: [{ id: "project-a", label: "解析 A", can_submit: true }], selected_project_id: "project-a", selected_job_id: null,
    status: { project_id: "project-a", jobs: [{ id: "job-a", root_id: "job-a", parent_id: null, title: "試験の仕事", state: "queued", environment_id: "env-a", environment_label: "解析用 PC", requestor: { user_id: "user-a", display_name: "利用者 A" }, assignee: { user_id: "user-a", display_name: "利用者 A" }, wait_reason: "実行枠を待っています。", uncertainty_reason: null, can_cancel: true, revision: 1, created_at_ms: 1, updated_at_ms: 1 }], environments: [{ id: "env-a", label: "解析用 PC", resource_id: "solver", runner_id: "runner-a", enabled: true, capacity: 2, occupied: 1, occupants: [], other_occupants: 1, additional_visible_occupants: 0 }], next_before: "job-a", next_environment_before: "env-a" },
    detail: null, approval: null, observed_at_ms: 1, error: null, submission_uncertain: false, submission_storage_error: null,
    inputs: [], assets: [], transcript: null, handover: null, inbox: null, feedback: null, provider: null, provider_draft: null, provider_error: null, provider_scope: { kind: "device" }, ...overrides };
}
export function sharedUiFixture() {
  const local = createSharedWorkUiState(); local.projection = sharedProjection(); return local;
}
