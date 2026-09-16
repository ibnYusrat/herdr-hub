// Action helper: a single connection-bound `request` facade so components
// don't prop-drill the HubConnection. App registers the connection.

import type { HubConnection } from "./hub.ts";
import type { ResponseFrame } from "./types.ts";

let conn: HubConnection | null = null;

export function setActionConnection(c: HubConnection | null): void {
  conn = c;
}

export function act(server: string, action: string, params: any): Promise<ResponseFrame> {
  if (!conn) {
    return Promise.resolve({
      type: "response",
      id: "offline",
      ok: false,
      error: { code: "hub.disconnected", message: "not connected" },
    });
  }
  return conn.request(server, action, params);
}

/** Send a prompt to an agent pane (agent.prompt) — never auto-retried. */
export function promptAgent(server: string, target: string, text: string): Promise<ResponseFrame> {
  return act(server, "agent.prompt", { target, text });
}

/** Send a line to a plain pane: text + enter. */
export async function sendLine(server: string, paneId: string, text: string): Promise<void> {
  await act(server, "pane.send_text", { pane_id: paneId, text });
  await act(server, "pane.send_keys", { pane_id: paneId, keys: ["enter"] });
}

/** Key-combo press (toolbar / raw specials): agent panes take agent.send_keys. */
export function sendKeys(server: string, paneId: string, isAgent: boolean, keys: string[]): Promise<ResponseFrame> {
  return isAgent
    ? act(server, "agent.send_keys", { target: paneId, keys })
    : act(server, "pane.send_keys", { pane_id: paneId, keys });
}
