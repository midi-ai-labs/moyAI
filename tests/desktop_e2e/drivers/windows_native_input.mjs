import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const bridge = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "windows_native_input.ps1");
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
export const TAURI_MAIN_WINDOW_CLASS = "Tauri Window";

function collect(stream) {
  let value = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => { value += chunk; });
  return () => value;
}

function requireHwnd(value, label) {
  if (typeof value !== "string" || !/^0x[0-9a-f]+$/i.test(value)) {
    throw new TypeError(`${label} must be a hexadecimal HWND string`);
  }
  return `0x${value.slice(2).toUpperCase()}`;
}

function requireOwner(owner, label) {
  if (!owner || !Number.isInteger(owner.process_id) || owner.process_id <= 0) {
    throw new TypeError(`${label}.process_id must be a positive integer`);
  }
  if (typeof owner.process_start_time_utc_ticks !== "string" || !/^\d+$/.test(owner.process_start_time_utc_ticks)) {
    throw new TypeError(`${label}.process_start_time_utc_ticks must be a decimal string`);
  }
  if (typeof owner.executable_path !== "string" || owner.executable_path.trim().length === 0) {
    throw new TypeError(`${label}.executable_path must be non-empty`);
  }
  return owner;
}

function executableIdentity(value) {
  return path.win32.normalize(value).toLowerCase();
}

function sameOwnerIdentity(left, right) {
  return left.process_id === right.process_id
    && left.process_start_time_utc_ticks === right.process_start_time_utc_ticks
    && executableIdentity(left.executable_path) === executableIdentity(right.executable_path);
}

function requireSnapshot(snapshot, label, expectedOwner) {
  const owner = requireOwner(snapshot?.owner, `${label}.owner`);
  if (!sameOwnerIdentity(owner, expectedOwner)) {
    throw new NativeInputError("native-owner-identity-drift", `${label} does not belong to the expected process identity`, {
      expected_owner: expectedOwner,
      observed_owner: owner,
    });
  }
  if (!Array.isArray(snapshot.windows)) throw new TypeError(`${label}.windows must be an array`);
  const handles = new Set();
  for (const window of snapshot.windows) {
    const hwnd = requireHwnd(window?.hwnd, `${label}.windows[].hwnd`);
    if (handles.has(hwnd)) throw new TypeError(`${label}.windows contains duplicate HWND ${hwnd}`);
    handles.add(hwnd);
    if (window.process_id !== owner.process_id) {
      throw new NativeInputError("native-window-owner-mismatch", `${label} contains a window outside the exact owner PID`, {
        owner,
        window,
      });
    }
  }
  return snapshot;
}

function requireCandidate(candidate) {
  const hwnd = requireHwnd(candidate?.hwnd, "candidate.hwnd");
  if (!Number.isInteger(candidate?.thread_id) || candidate.thread_id <= 0) {
    throw new TypeError("candidate.thread_id must be a positive integer");
  }
  if (typeof candidate?.class_name !== "string" || candidate.class_name.length === 0) {
    throw new TypeError("candidate.class_name must be non-empty");
  }
  return { hwnd, threadId: candidate.thread_id, className: candidate.class_name };
}

function requireInteractiveCandidate(candidate) {
  const fingerprint = requireCandidate(candidate);
  if (candidate?.visible !== true || candidate?.enabled !== true || candidate?.is_root !== true) {
    throw new TypeError("candidate must be a visible, enabled root window");
  }
  if (requireHwnd(candidate?.root_hwnd, "candidate.root_hwnd") !== fingerprint.hwnd) {
    throw new TypeError("candidate.root_hwnd must equal candidate.hwnd");
  }
  return fingerprint;
}

export class NativeInputError extends Error {
  constructor(code, message, evidence = null) {
    super(message);
    this.name = "NativeInputError";
    this.code = code;
    this.evidence = evidence === null ? null : structuredClone(evidence);
  }
}

export async function invokeWindowsNativeInput(action, parameters = {}, { timeoutMs = 30_000 } = {}) {
  if (process.platform !== "win32") throw new Error("Windows native input adapter is only available on win32");
  const args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-File", bridge, "-Action", action];
  for (const [name, value] of Object.entries(parameters)) {
    if (value === null || value === undefined || value === "") continue;
    args.push(`-${name}`, String(value));
  }
  const child = spawn("pwsh.exe", args, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  const stdout = collect(child.stdout);
  const stderr = collect(child.stderr);
  const result = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`Windows native input adapter ${action} timed out`));
    }, timeoutMs);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout: stdout(), stderr: stderr() });
    });
  });
  if (result.code !== 0) {
    throw new Error(`Windows native input adapter ${action} failed: ${result.stderr.trim() || result.stdout.trim()}`);
  }
  const text = result.stdout.trim();
  return text.length === 0 ? null : JSON.parse(text);
}

export function selectFreshForegroundWindow(before, after, expectedOwner = before?.owner) {
  expectedOwner = requireOwner(expectedOwner, "expectedOwner");
  requireSnapshot(before, "before", expectedOwner);
  requireSnapshot(after, "after", expectedOwner);
  const baselineHandles = new Set(before.windows.map((window) => requireHwnd(window.hwnd, "before.windows[].hwnd")));
  const fresh = after.windows.filter((window) => {
    const hwnd = requireHwnd(window.hwnd, "after.windows[].hwnd");
    const root = requireHwnd(window.root_hwnd, "after.windows[].root_hwnd");
    return !baselineHandles.has(hwnd)
      && window.visible === true
      && window.enabled === true
      && window.is_root === true
      && hwnd === root;
  });
  const foregroundRoot = after.foreground_root_hwnd === null
    ? null
    : requireHwnd(after.foreground_root_hwnd, "after.foreground_root_hwnd");
  const foregroundCandidates = fresh.filter(
    (window) => requireHwnd(window.hwnd, "fresh.windows[].hwnd") === foregroundRoot,
  );
  if (foregroundCandidates.length === 1 && after.foreground_process_id === expectedOwner.process_id) {
    const candidate = foregroundCandidates[0];
    requireInteractiveCandidate(candidate);
    return structuredClone(candidate);
  }
  if (fresh.length !== 1) {
    throw new NativeInputError(
      "native-window-cardinality",
      `expected one fresh visible root matching the exact foreground, found ${foregroundCandidates.length} among ${fresh.length} fresh roots`,
      {
        baseline_handles: [...baselineHandles],
        fresh_windows: fresh,
        foreground_root_hwnd: after.foreground_root_hwnd,
        foreground_process_id: after.foreground_process_id,
      },
    );
  }
  const candidate = fresh[0];
  throw new NativeInputError("native-window-not-foreground", "the fresh exact-owner window is not the foreground root", {
    candidate,
    foreground_root_hwnd: after.foreground_root_hwnd,
    foreground_process_id: after.foreground_process_id,
  });
}

export function selectFreshOwnedRootWindow(
  before,
  after,
  expectedOwner = before?.owner,
  { expectedClassName = null } = {},
) {
  expectedOwner = requireOwner(expectedOwner, "expectedOwner");
  if (expectedClassName !== null && (typeof expectedClassName !== "string" || expectedClassName.length === 0)) {
    throw new TypeError("expectedClassName must be null or a non-empty string");
  }
  requireSnapshot(before, "before", expectedOwner);
  requireSnapshot(after, "after", expectedOwner);
  const baselineHandles = new Set(before.windows.map((window) => requireHwnd(window.hwnd, "before.windows[].hwnd")));
  const allFreshRoots = after.windows.filter((window) => {
    const hwnd = requireHwnd(window.hwnd, "after.windows[].hwnd");
    const root = requireHwnd(window.root_hwnd, "after.windows[].root_hwnd");
    return !baselineHandles.has(hwnd)
      && window.visible === true
      && window.enabled === true
      && window.is_root === true
      && hwnd === root;
  });
  const candidates = expectedClassName === null
    ? allFreshRoots
    : allFreshRoots.filter((window) => window.class_name === expectedClassName);
  if (candidates.length !== 1) {
    throw new NativeInputError(
      "native-window-cardinality",
      `expected exactly one fresh exact-owner root candidate, found ${candidates.length}`,
      {
        baseline_handles: [...baselineHandles],
        expected_class_name: expectedClassName,
        fresh_windows: candidates,
        auxiliary_fresh_windows: allFreshRoots.filter((window) => !candidates.includes(window)),
        foreground_root_hwnd: after.foreground_root_hwnd,
        foreground_process_id: after.foreground_process_id,
      },
    );
  }
  const candidate = candidates[0];
  requireInteractiveCandidate(candidate);
  return structuredClone(candidate);
}

export function selectSingleOwnedRootWindow(
  observed,
  expectedOwner = observed?.owner,
  { expectedClassName } = {},
) {
  expectedOwner = requireOwner(expectedOwner, "expectedOwner");
  if (typeof expectedClassName !== "string" || expectedClassName.length === 0) {
    throw new TypeError("expectedClassName must be a non-empty current native class fingerprint");
  }
  requireSnapshot(observed, "observed", expectedOwner);
  const allCurrentRoots = observed.windows.filter((window) => {
    const hwnd = requireHwnd(window.hwnd, "observed.windows[].hwnd");
    const root = requireHwnd(window.root_hwnd, "observed.windows[].root_hwnd");
    return window.visible === true
      && window.enabled === true
      && window.is_root === true
      && hwnd === root;
  });
  const roots = allCurrentRoots.filter((window) => window.class_name === expectedClassName);
  if (roots.length !== 1) {
    throw new NativeInputError(
      "native-window-cardinality",
      `expected exactly one current exact-owner root candidate, found ${roots.length}`,
      {
        expected_class_name: expectedClassName,
        candidate_windows: roots,
        auxiliary_current_windows: allCurrentRoots.filter((window) => !roots.includes(window)),
        observed_windows: observed.windows,
      },
    );
  }
  requireInteractiveCandidate(roots[0]);
  return structuredClone(roots[0]);
}

export async function snapshotOwnedTopLevelWindows(
  { executionRoot, ownerPath, expectedOwner = null },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const snapshot = await invoke("Snapshot", { ExecutionRoot: executionRoot, OwnerPath: ownerPath });
  if (expectedOwner !== null) requireSnapshot(snapshot, "snapshot", requireOwner(expectedOwner, "expectedOwner"));
  else requireSnapshot(snapshot, "snapshot", requireOwner(snapshot?.owner, "snapshot.owner"));
  return snapshot;
}

export async function probeExactOwnedWindow(
  { executionRoot, ownerPath, candidate },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireCandidate(candidate);
  const result = await invoke("ProbeWindow", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
  });
  const expectedMatches = typeof result?.expected_hwnd === "string"
    && /^0x[0-9a-f]+$/i.test(result.expected_hwnd)
    && result.expected_hwnd.toLowerCase() === fingerprint.hwnd.toLowerCase();
  const commonValid = result?.exact_identity === true
    && expectedMatches
    && result?.cleanup_only === false
    && result?.representative_input === false;
  if (result?.live === false && commonValid && result?.identity_state === "destroyed" && result?.window === null) {
    return result;
  }
  if (
    result?.live === true
    && commonValid
    && result?.identity_state === "live-exact-owner"
    && typeof result?.window === "object"
    && result.window !== null
    && requireHwnd(result.window.hwnd, "result.window.hwnd") === fingerprint.hwnd
    && result.window.thread_id === fingerprint.threadId
    && result.window.class_name === fingerprint.className
    && result.window.is_root === true
  ) {
    return result;
  }
  throw new NativeInputError("native-window-liveness-invalid", "exact HWND liveness probe did not satisfy its identity contract", result);
}

export async function sendEscapeToOwnedForegroundWindow(
  { executionRoot, ownerPath, candidate },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  const result = await invoke("SendEscape", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
  });
  if (
    result?.foreground_activation_verified !== true
    || result?.foreground_pre_input_verified !== true
    || result?.foreground_post_input_verified !== true
    || result?.foreground_verified !== true
    || result?.delivery_verified !== true
    || result?.input_count !== 2
    || result?.cleanup_only !== false
    || result?.representative_input !== true
  ) {
    throw new NativeInputError("native-escape-delivery-invalid", "SendInput Escape did not satisfy the exact delivery contract", result);
  }
  return result;
}

function requireDragInteger(value, label, { minimum = null, nonZero = false } = {}) {
  if (!Number.isInteger(value) || (minimum !== null && value < minimum) || (nonZero && value === 0)) {
    throw new TypeError(`${label} is invalid`);
  }
  return value;
}

function sameExactWindow(left, right, candidate) {
  return requireHwnd(left?.hwnd, "drag.window_before.hwnd") === requireHwnd(candidate.hwnd, "candidate.hwnd")
    && requireHwnd(right?.hwnd, "drag.window_after.hwnd") === requireHwnd(candidate.hwnd, "candidate.hwnd")
    && left?.process_id === candidate.process_id
    && right?.process_id === candidate.process_id
    && left?.thread_id === candidate.thread_id
    && right?.thread_id === candidate.thread_id
    && left?.class_name === candidate.class_name
    && right?.class_name === candidate.class_name
    && left?.is_root === true
    && right?.is_root === true
    && left?.visible === true
    && right?.visible === true
    && left?.enabled === true
    && right?.enabled === true;
}

export function exactOwnedWindowDragObserved(result, { minimumDistance = 8 } = {}) {
  requireDragInteger(minimumDistance, "minimumDistance", { minimum: 1 });
  const before = result?.window_before?.rect;
  const after = result?.window_after?.rect;
  const delta = result?.position_delta;
  if (![before?.left, before?.top, before?.width, before?.height, after?.left, after?.top, after?.width, after?.height, delta?.x, delta?.y]
    .every(Number.isInteger)) return false;
  const measuredX = after.left - before.left;
  const measuredY = after.top - before.top;
  return delta.x === measuredX
    && delta.y === measuredY
    && Math.max(Math.abs(measuredX), Math.abs(measuredY)) >= minimumDistance
    && before.width === after.width
    && before.height === after.height
    && result?.size_unchanged === true;
}

export async function dragExactOwnedWindow(
  {
    executionRoot,
    ownerPath,
    candidate,
    clientOffsetX,
    clientOffsetY,
    deltaX,
    deltaY,
  },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  requireDragInteger(clientOffsetX, "clientOffsetX", { minimum: 0 });
  requireDragInteger(clientOffsetY, "clientOffsetY", { minimum: 0 });
  requireDragInteger(deltaX, "deltaX");
  requireDragInteger(deltaY, "deltaY");
  if (deltaX === 0 && deltaY === 0) throw new TypeError("window drag delta must be non-zero");
  const result = await invoke("DragWindow", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
    ClientOffsetX: clientOffsetX,
    ClientOffsetY: clientOffsetY,
    DragDeltaX: deltaX,
    DragDeltaY: deltaY,
  });
  const requested = result?.requested_css;
  const pathRows = result?.delivered_device?.path;
  const clientRect = result?.client_rect;
  const delivered = result?.delivered_device;
  if (
    result?.foreground_activation_verified !== true
    || result?.foreground_pre_input_verified !== true
    || result?.foreground_post_input_verified !== true
    || result?.delivery_verified !== true
    || result?.button_initially_up !== true
    || result?.mouse_down_count !== 1
    || result?.mouse_up_count !== 1
    || result?.mouse_up_attempted !== true
    || result?.button_release_verified !== true
    || result?.cursor_moved_by_driver !== true
    || result?.cursor_restore_attempted !== true
    || result?.cursor_restore_succeeded !== true
    || result?.cleanup_only !== false
    || result?.representative_input !== true
    || !Number.isInteger(result?.dpi)
    || result.dpi <= 0
    || !Number.isFinite(result?.css_to_device_scale)
    || result.css_to_device_scale <= 0
    || !Number.isInteger(clientRect?.width)
    || !Number.isInteger(clientRect?.height)
    || clientRect.width <= 0
    || clientRect.height <= 0
    || !Number.isInteger(delivered?.start_x)
    || !Number.isInteger(delivered?.start_y)
    || !Number.isInteger(delivered?.delta_x)
    || !Number.isInteger(delivered?.delta_y)
    || !Array.isArray(pathRows)
    || pathRows.length < 2
    || pathRows.some((row) => row?.succeeded !== true || !Number.isInteger(row?.x) || !Number.isInteger(row?.y))
    || requested?.client_offset_x !== clientOffsetX
    || requested?.client_offset_y !== clientOffsetY
    || requested?.delta_x !== deltaX
    || requested?.delta_y !== deltaY
    || !sameExactWindow(result?.window_before, result?.window_after, candidate)
  ) {
    throw new NativeInputError(
      "native-window-drag-delivery-invalid",
      "native titlebar pointer drag did not satisfy the exact delivery contract",
      result,
    );
  }
  return result;
}

export async function closeOwnedNativeDialog(
  { executionRoot, ownerPath, candidate },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  const result = await invoke("CloseDialog", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
  });
  if (
    result?.requested !== true
    || result?.request_count !== 1
    || result?.attempted !== true
    || result?.attempt_count !== 1
    || result?.may_have_dispatched !== true
    || result?.confirmed !== true
    || result?.call_returned !== true
    || result?.window_pattern_verified !== true
    || result?.foreground_required !== false
    || result?.cleanup_only !== false
    || result?.representative_input !== true
    || result?.ui_automation_window?.process_id !== candidate.process_id
    || typeof result?.ui_automation_window?.native_hwnd !== "string"
    || !/^0x[0-9a-f]+$/i.test(result.ui_automation_window.native_hwnd)
    || result.ui_automation_window.native_hwnd.toLowerCase() !== fingerprint.hwnd.toLowerCase()
  ) {
    throw new NativeInputError(
      "native-dialog-close-invalid",
      "Windows UI Automation did not satisfy the exact native dialog close contract",
      result,
    );
  }
  return result;
}

export async function selectFileInOwnedNativeDialog(
  { executionRoot, ownerPath, candidate, selectedPath },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  if (typeof selectedPath !== "string" || !path.win32.isAbsolute(selectedPath) || selectedPath.includes("\0")) {
    throw new TypeError("selectedPath must be an absolute Windows path without NUL bytes");
  }
  const result = await invoke("SelectFile", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
    SelectedPath: selectedPath,
  });
  const selectedIdentity = path.win32.normalize(String(result?.selected_path ?? "")).toLowerCase();
  const expectedIdentity = path.win32.normalize(selectedPath).toLowerCase();
  const expectedFileName = path.win32.basename(selectedPath);
  const exactDefaultAction = result?.default_action_pattern === "InvokePattern.Invoke()"
    && result?.file_item?.invoke_pattern === true;
  if (
    result?.attempted !== true
    || result?.attempt_count !== 1
    || result?.request_count !== 1
    || result?.delivery_verified !== true
    || result?.file_item_selection_verified !== true
    || result?.file_item_default_action_verified !== true
    || exactDefaultAction !== true
    || result?.file_item?.name !== expectedFileName
    || !["ControlType.DataItem", "ControlType.ListItem"].includes(result?.file_item?.control_type)
    || result?.file_item?.process_id !== candidate.process_id
    || result?.file_item?.enabled !== true
    || result?.file_item?.offscreen !== false
    || result?.file_item?.selection_item_pattern !== true
    || result?.foreground_required !== false
    || result?.cleanup_only !== false
    || result?.representative_input !== true
    || selectedIdentity !== expectedIdentity
    || requireHwnd(result?.window?.hwnd, "select-file.window.hwnd") !== fingerprint.hwnd
    || result?.window?.process_id !== candidate.process_id
    || result?.window?.thread_id !== candidate.thread_id
    || result?.window?.class_name !== candidate.class_name
  ) {
    throw new NativeInputError(
      "native-dialog-file-selection-invalid",
      "Windows UI Automation did not satisfy the exact native file selection contract",
      result,
    );
  }
  return result;
}

/** Exact native-control input. This does not establish physical keyboard or IME behavior. */
export async function openFilePathInOwnedNativeDialog(
  { executionRoot, ownerPath, candidate, selectedPath },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  if (fingerprint.className !== "#32770") throw new TypeError("An exact native file dialog is required");
  if (typeof selectedPath !== "string" || !path.win32.isAbsolute(selectedPath)
    || selectedPath.includes("\0") || selectedPath.length >= 32768) {
    throw new TypeError("selectedPath must be a bounded absolute Windows path without NUL bytes");
  }
  const result = await invoke("OpenFilePath", {
    ExecutionRoot: executionRoot, OwnerPath: ownerPath, WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId, ExpectedClassName: fingerprint.className, SelectedPath: selectedPath,
  });
  const hwnd = value => typeof value === "string" && /^0x[0-9a-f]+$/i.test(value) && BigInt(value) !== 0n
    ? BigInt(value).toString(16) : null;
  const dialogHwnd = hwnd(fingerprint.hwnd);
  const identities = [
    ["combo_ex", dialogHwnd, "ComboBoxEx32", 1148],
    ["combo", hwnd(result?.controls?.combo_ex?.hwnd), "ComboBox", 1148],
    ["edit", hwnd(result?.controls?.combo?.hwnd), "Edit", 1148],
    ["button", dialogHwnd, "Button", 1],
  ];
  const exactControls = identities.every(([name, parent, className, id]) => {
    const control = result?.controls?.[name];
    return parent !== null && hwnd(control?.hwnd) !== null && hwnd(control?.hwnd) !== dialogHwnd && hwnd(control?.parent_hwnd) === parent
      && hwnd(control?.root_hwnd) === dialogHwnd && control?.process_id === candidate.process_id
      && control?.thread_id === candidate.thread_id && control?.class_name === className
      && control?.control_id === id && control?.visible === true && control?.enabled === true;
  }) && new Set(identities.map(([name]) => hwnd(result?.controls?.[name]?.hwnd))).size === identities.length;
  if (!exactControls || hwnd(result?.window?.hwnd) !== dialogHwnd
    || hwnd(result?.window?.root_hwnd) !== dialogHwnd || result?.window?.is_root !== true
    || result?.window?.visible !== true || result?.window?.enabled !== true
    || result?.window?.process_id !== candidate.process_id || result?.window?.thread_id !== candidate.thread_id
    || result?.window?.class_name !== fingerprint.className
    || path.win32.normalize(String(result?.selected_path ?? "")).toLowerCase() !== path.win32.normalize(selectedPath).toLowerCase()
    || result?.delivery_verified !== true || result?.filename_set_count !== 1
    || result?.filename_readback_verified !== true || result?.open_click_count !== 1
    || result?.open_call_returned !== true || result?.retry_count !== 0
    || result?.foreground_required !== false
    || result?.cleanup_only !== false || result?.representative_input !== true
    || result?.os_keyboard_ime_evidence !== false
    || result?.input !== "native-control: WM_SETTEXT -> WM_GETTEXT exact readback -> BM_CLICK") {
    throw new NativeInputError("native-dialog-path-open-invalid",
      "Native file-dialog input did not satisfy exact control identity, readback, and single-delivery requirements", result);
  }
  return result;
}

export async function captureOwnedWindowPng(
  { executionRoot, ownerPath, candidate },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireInteractiveCandidate(candidate);
  const result = await invoke("CapturePng", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
  });
  if (result?.available !== true) {
    return { available: false, reason: String(result?.reason ?? "native window capture unavailable"), bytes: null };
  }
  if (typeof result.png_base64 !== "string" || result.png_base64.length === 0) {
    throw new NativeInputError("native-window-png-invalid", "native window capture returned no PNG payload", result);
  }
  const bytes = Buffer.from(result.png_base64, "base64");
  if (bytes.byteLength < PNG_SIGNATURE.byteLength || !bytes.subarray(0, PNG_SIGNATURE.byteLength).equals(PNG_SIGNATURE)) {
    throw new NativeInputError("native-window-png-invalid", "native window capture did not return PNG bytes", {
      window: result.window,
      size_bytes: bytes.byteLength,
    });
  }
  return { available: true, reason: null, bytes, window: result.window };
}

export async function closeOwnedWindowForCleanup(
  { executionRoot, ownerPath, candidate },
  { invoke = invokeWindowsNativeInput } = {},
) {
  const fingerprint = requireCandidate(candidate);
  const result = await invoke("CloseCleanup", {
    ExecutionRoot: executionRoot,
    OwnerPath: ownerPath,
    WindowHandle: fingerprint.hwnd,
    ExpectedThreadId: fingerprint.threadId,
    ExpectedClassName: fingerprint.className,
  });
  if (result?.cleanup_only !== true || result?.representative_input !== false) {
    throw new NativeInputError("native-cleanup-close-invalid", "exact-HWND close fallback was not marked cleanup-only", result);
  }
  return result;
}
