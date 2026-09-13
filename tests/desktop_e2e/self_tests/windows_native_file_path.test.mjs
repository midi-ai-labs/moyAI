import assert from "node:assert/strict";
import test from "node:test";
import * as nativeInput from "../drivers/windows_native_input.mjs";

const candidate = {
  hwnd: "0x100", root_hwnd: "0x100", process_id: 4100, thread_id: 812,
  class_name: "#32770", visible: true, enabled: true, is_root: true,
};
const args = {
  executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate,
  selectedPath: "C:\\fixtures\\日本語 フォルダ\\hub-config.toml",
};
function delivered() {
  const control = (hwnd, parent, className, id) => ({
    hwnd, parent_hwnd: parent, root_hwnd: candidate.hwnd, process_id: candidate.process_id,
    thread_id: candidate.thread_id, class_name: className, control_id: id,
    visible: true, enabled: true,
  });
  return {
    window: { ...candidate }, selected_path: args.selectedPath, delivery_verified: true,
    filename_set_count: 1, filename_readback_verified: true, open_click_count: 1,
    open_call_returned: true, foreground_required: false,
    cleanup_only: false, representative_input: true, retry_count: 0,
    input: "native-control: WM_SETTEXT -> WM_GETTEXT exact readback -> BM_CLICK",
    os_keyboard_ime_evidence: false,
    controls: {
      combo_ex: control("0x110", "0x100", "ComboBoxEx32", 1148),
      combo: control("0x120", "0x110", "ComboBox", 1148),
      edit: control("0x130", "0x120", "Edit", 1148),
      button: control("0x140", "0x100", "Button", 1),
    },
  };
}

test("native absolute-path input preserves exact owner/control identity and one delivery per operation", async () => {
  const calls = [];
  const evidence = delivered();
  const result = await nativeInput.openFilePathInOwnedNativeDialog(args, { invoke: async (action, parameters) => {
    calls.push({ action, parameters }); return evidence;
  } });
  assert.equal(result, evidence);
  assert.deepEqual(calls, [{ action: "OpenFilePath", parameters: {
    ExecutionRoot: args.executionRoot, OwnerPath: args.ownerPath,
    WindowHandle: candidate.hwnd, ExpectedThreadId: candidate.thread_id,
    ExpectedClassName: "#32770", SelectedPath: args.selectedPath,
  } }]);
  assert.equal(result.os_keyboard_ime_evidence, false);
});

test("native absolute-path acceptance rejects misdelivery, stale identity and broadened claims without retry", async () => {
  const invalid = [
    result => { result.filename_set_count = 2; },
    result => { result.filename_readback_verified = false; },
    result => { result.open_click_count = 0; },
    result => { result.open_call_returned = false; },
    result => { result.retry_count = 1; },
    result => { result.foreground_required = true; },
    result => { result.selected_path = "C:\\fixtures\\wrong.toml"; },
    result => { result.window.thread_id += 1; },
    result => { result.window.enabled = false; },
    result => { result.window.root_hwnd = "0x999"; },
    result => { result.controls.edit.process_id += 1; },
    result => { result.controls.edit.parent_hwnd = candidate.hwnd; },
    result => { result.controls.combo_ex.control_id = 41477; },
    result => { result.controls.combo_ex.hwnd = candidate.hwnd; },
    result => { result.controls.button.hwnd = result.controls.edit.hwnd; },
    result => { result.controls.button.control_id = 2; },
    result => { result.controls.button.class_name = "Other"; },
    result => { result.controls.button.visible = false; },
    result => { result.controls.button.root_hwnd = "0x999"; },
    result => { result.controls.edit.enabled = false; },
    result => { result.os_keyboard_ime_evidence = true; },
    result => { result.input = "fallback input"; },
  ];
  for (const mutate of invalid) {
    const result = delivered(); mutate(result);
    let calls = 0;
    await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog(args, { invoke: async () => {
      calls += 1; return result;
    } }), error => error.code === "native-dialog-path-open-invalid");
    assert.equal(calls, 1);
  }
  const ambiguous = new Error("BM_CLICK delivery is ambiguous");
  let calls = 0;
  await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog(args, { invoke: async () => {
    calls += 1; throw ambiguous;
  } }), error => error === ambiguous);
  assert.equal(calls, 1);
});

test("native absolute-path input rejects an invalid path or dialog before dispatch", async () => {
  let calls = 0;
  const invoke = async () => { calls += 1; return delivered(); };
  for (const selectedPath of ["relative.toml", "C:\\fixture\0.toml", `C:\\${"x".repeat(32768)}`]) {
    await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog({ ...args, selectedPath }, { invoke }), TypeError);
  }
  await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog({
    ...args, candidate: { ...candidate, class_name: "Tauri Window" },
  }, { invoke }), TypeError);
  assert.equal(calls, 0);
});

test("native save and directory choices require their explicit intent and cannot accept open-mode evidence", async () => {
  for (const intent of ["save_new", "directory"]) {
  const selectedPath = "C:\\execution\\output\\new.bin";
  const parameters = { ...args, selectedPath, intent };
  const evidence = { ...delivered(), selected_path: selectedPath, selected_path_intent: intent };
  if (intent === "save_new") {
    const control = (hwnd, parent_hwnd, class_name, control_id) => ({ ...evidence.controls.edit, hwnd, parent_hwnd, class_name, control_id });
    evidence.controls = { view: control("0x110", "0x100", "DUIViewWndClassName", 0), direct: control("0x120", "0x110", "DirectUIHWND", 0), sink: control("0x130", "0x120", "FloatNotifySink", 0), combo: control("0x140", "0x130", "ComboBox", 0), edit: control("0x150", "0x140", "Edit", 1001), button: control("0x160", "0x100", "Button", 1) };
    const wrongParent = structuredClone(evidence); wrongParent.controls.sink.parent_hwnd = candidate.hwnd;
    await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog(parameters, { invoke: async () => wrongParent }), error => error.code === "native-dialog-path-open-invalid");
  }
  if (intent === "directory") {
    evidence.controls = { edit: { ...evidence.controls.edit, parent_hwnd: candidate.hwnd, control_id: 1152 }, button: evidence.controls.button };
    await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog(parameters, { invoke: async () => ({ ...delivered(), selected_path: selectedPath, selected_path_intent: intent }) }), error => error.code === "native-dialog-path-open-invalid");
  }
  const result = await nativeInput.openFilePathInOwnedNativeDialog(parameters, { invoke: async (action, values) => {
    assert.equal(action, "OpenFilePath"); assert.equal(values.SelectedPathIntent, intent);
    return evidence;
  } });
  assert.equal(result, evidence);
  await assert.rejects(() => nativeInput.openFilePathInOwnedNativeDialog(parameters, { invoke: async () => ({ ...evidence, selected_path_intent: "open" }) }), error => error.code === "native-dialog-path-open-invalid");
  }
});
