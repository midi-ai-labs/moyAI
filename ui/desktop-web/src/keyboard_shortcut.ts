export interface KeyboardShortcutSample {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  repeat: boolean;
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
