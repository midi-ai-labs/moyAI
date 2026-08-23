export interface KeyboardShortcutSample {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  repeat: boolean;
}

export interface ModalKeyboardShortcutSample extends KeyboardShortcutSample {
  altKey: boolean;
}

const NATIVE_TEXT_EDITING_KEYS = new Set([
  "a",
  "c",
  "v",
  "x",
  "y",
  "z",
  "backspace",
  "delete",
  "arrowleft",
  "arrowright",
  "home",
  "end",
]);

export function modalShortcutShouldPreventDefault(
  sample: ModalKeyboardShortcutSample,
  textEditingTarget: boolean,
): boolean {
  const commandKey = sample.ctrlKey || sample.metaKey;
  if (
    textEditingTarget
    && commandKey
    && !sample.altKey
    && NATIVE_TEXT_EDITING_KEYS.has(sample.key.toLowerCase())
  ) {
    return false;
  }
  return commandKey || sample.altKey || /^F\d+$/.test(sample.key);
}

export function globalShortcutAction(sample: KeyboardShortcutSample): string | null {
  if (sample.repeat) return null;
  const commandKey = sample.ctrlKey || sample.metaKey;
  const key = sample.key.toLowerCase();
  if (commandKey && key === "k") return "show-command-palette";
  if (commandKey && key === "n") return "new-chat";
  if (commandKey && sample.key === "Enter") return "send";
  if (sample.key === "F8") return "toggle-access";
  if (sample.key === "F9") return "export-transcript";
  if (commandKey && key === "i") return "toggle-session-archived-search";
  return null;
}
