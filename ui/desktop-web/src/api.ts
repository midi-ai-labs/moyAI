import { invoke } from "@tauri-apps/api/core";

export async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(name, args ?? {});
}
