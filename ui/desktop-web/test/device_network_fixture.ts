import { acceptDeviceNetworkProjection, createDeviceNetworkUiState, type DeviceNetworkProjection } from "../src/device_network_state.ts";

export function deviceProjection(overrides: Partial<DeviceNetworkProjection> = {}): DeviceNetworkProjection {
  return {
    revision: "3", generation: "7", hub_url: "https://192.168.1.10:9471", device_id: "device-00",
    display_name: "Win00", local_hostname: "DESKTOP-00", enrollment: "active",
    receiver: { profile_id: "receiver-00", enabled: false, status: "stopped", target: { kind: "temp" },
      access_mode: "default", model_mode: "hub", start_on_launch: false, keep_when_hidden: false,
      endpoint: null, can_change: true, confirmed: false, reason: null },
    targets: [{ target: { kind: "temp" }, label: "Temp（一時作業用）" },
      { target: { kind: "project", project_id: "project-a", workspace_root: "C:/work/project-a" }, label: "プロジェクトA" }],
    peers: [{ device_id: "device-19", profile_id: "receiver-19", display_name: "Win19", name: "Temp受付",
      selected: false, online: true, receiving: true, can_use: true, reason: null },
    { device_id: "device-20", profile_id: "receiver-20", display_name: "Win20", name: "開発受付",
      selected: false, online: true, receiving: true, can_use: true, reason: null }],
    can_join: false, can_leave: true, error: null, ...overrides,
  };
}
export function deviceUiFixture() {
  const local = createDeviceNetworkUiState();
  acceptDeviceNetworkProjection(local, deviceProjection());
  local.jobs.outgoing = [{ reference_id: "reference-a", root_task_id: "task-00", device_id: "device-20",
    profile_id: "receiver-20", device_path: ["device-00", "device-19", "device-20"], job_id: "job-20",
    state: "running", stop_status: "none", can_stop: true, result: null }];
  local.jobs.incoming = [{ job_id: "job-00", profile_id: "receiver-00", parent: { peer_id: "device-19", task_id: "task-19", turn_id: "turn-19" },
    prompt_preview: "このPCの作業を確認", session_id: "session-00", state: "running", model: "model-a",
    result: null, result_truncated: false, can_stop: true,
    network: { origin_device_id: "device-19", actor_device_id: "device-19", root_task_id: "task-19", device_path: ["device-19", "device-00"] } }];
  return local;
}
