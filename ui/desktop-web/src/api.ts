import { invoke } from "@tauri-apps/api/core";
import desktopCommandNames from "./desktop_commands.json" with { type: "json" };

const DESKTOP_COMMAND_NAMES = new Set<string>(desktopCommandNames);
export const DESKTOP_COMMAND_OBSERVER_SYMBOL = "moyai.desktop.command-observer.v1";

interface DesktopCommandObservation {
  readonly name: string;
  readonly args: Record<string, unknown>;
}

function observeDesktopCommand(name: string, args: Record<string, unknown>): void {
  const observer = (globalThis as Record<PropertyKey, unknown>)[
    Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL)
  ];
  if (typeof observer !== "function") return;
  try {
    (observer as (observation: DesktopCommandObservation) => void)({
      name,
      args: structuredClone(name === "hub_connect" ? { ...args, token: "[redacted]" }
        : name === "device_network_join" ? { ...args, code: "[redacted]" }
        : name === "mcp_peer_add" ? { ...args, peer: { ...(args.peer as Record<string, unknown>), token: "[redacted]" } } : args),
    });
  } catch {
    // External diagnostics must never change command delivery.
  }
}

export function isDesktopCommandName(name: string): boolean {
  return DESKTOP_COMMAND_NAMES.has(name);
}

export async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (!isDesktopCommandName(name)) {
    throw new Error(`Unknown Desktop command: ${name}`);
  }
  const payload = args ?? {};
  observeDesktopCommand(name, payload);
  return invoke<T>(name, payload);
}
