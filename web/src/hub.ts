// WebSocket client for the hub protocol: hello auth (token never in the URL),
// automatic reconnect with 0.5–10 s backoff + jitter, re-hello on reconnect,
// ping keepalive, pending-request map for `request` messages, and debounced
// `view` updates (full replace, SPEC §5.1.4).

import type { HubFrame, ResponseFrame } from "./types.ts";

const PROTOCOL = 1;

export interface HubHandlers {
  onFrame: (frame: HubFrame) => void;
  onConnectionChange: (connected: boolean) => void;
}

let nextReqId = 1;

export class HubConnection {
  private ws: WebSocket | null = null;
  private token: string;
  private handlers: HubHandlers;
  private closedByUser = false;
  private backoffMs = 500;
  private reconnectTimer: number | null = null;
  private pingTimer: number | null = null;
  private pendingView: any = null;
  private viewTimer: number | null = null;
  private requests = new Map<string, (r: ResponseFrame) => void>();

  constructor(token: string, handlers: HubHandlers) {
    this.token = token;
    this.handlers = handlers;
  }

  connect(): void {
    this.closedByUser = false;
    this.open();
  }

  private open(): void {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${proto}//${location.host}/ws`);
    this.ws = ws;

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          type: "hello",
          protocol: PROTOCOL,
          token: this.token,
          client: { name: "web", version: "0.1.0", caps: [] },
        }),
      );
    };

    ws.onmessage = (ev) => {
      let frame: HubFrame;
      try {
        frame = JSON.parse(ev.data as string);
      } catch {
        return;
      }
      if (frame.type === "welcome") {
        this.backoffMs = 500;
        this.handlers.onConnectionChange(true);
        this.schedulePing(frame.heartbeat_ms);
        // Re-declare the current view after a reconnect.
        if (this.pendingView) this.sendViewNow(this.pendingView);
        // Fall through: the app layer also consumes the welcome (server list).
      }
      if (frame.type === "response") {
        const done = this.requests.get(frame.id ?? "");
        if (done) {
          this.requests.delete(frame.id ?? "");
          done(frame);
        }
      }
      this.handlers.onFrame(frame);
    };

    ws.onclose = () => {
      this.handlers.onConnectionChange(false);
      this.stopPing();
      if (this.closedByUser) return;
      const jitter = this.backoffMs * (0.75 + Math.random() * 0.5);
      this.reconnectTimer = window.setTimeout(() => this.open(), jitter);
      this.backoffMs = Math.min(this.backoffMs * 2, 10_000);
    };

    ws.onerror = () => {
      // onclose follows; nothing to do here.
    };
  }

  /** Full-replace view declaration, debounced ~100 ms (SPEC §5.1.4). */
  sendView(view: {
    server: string;
    active: { workspace: string | null; tab: string | null };
    panes: { pane: string; focused: boolean }[];
    scrollback: string[];
  }): void {
    this.pendingView = view;
    if (this.viewTimer !== null) return;
    this.viewTimer = window.setTimeout(() => {
      this.viewTimer = null;
      if (this.pendingView && this.ws?.readyState === WebSocket.OPEN) {
        this.sendViewNow(this.pendingView);
      }
    }, 100);
  }

  private sendViewNow(view: any): void {
    this.ws?.send(JSON.stringify({ type: "view", ...view }));
  }

  /** Send an allowlisted action; resolves with the hub response frame. */
  request(server: string, action: string, params: any): Promise<ResponseFrame> {
    return new Promise((resolve) => {
      const id = `web${nextReqId++}`;
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        resolve({
          type: "response",
          id,
          ok: false,
          error: { code: "hub.disconnected", message: "not connected" },
        });
        return;
      }
      this.requests.set(id, resolve);
      this.ws.send(JSON.stringify({ type: "request", id, server, action, params }));
      window.setTimeout(() => {
        if (this.requests.delete(id)) {
          resolve({
            type: "response",
            id,
            ok: false,
            error: { code: "hub.timeout", message: "no response from hub" },
          });
        }
      }, 15_000);
    });
  }

  private schedulePing(heartbeatMs: number): void {
    this.stopPing();
    this.pingTimer = window.setInterval(() => {
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({ type: "ping" }));
      }
    }, Math.max(5000, heartbeatMs / 2));
  }

  private stopPing(): void {
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  close(): void {
    this.closedByUser = true;
    this.stopPing();
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    if (this.viewTimer !== null) clearTimeout(this.viewTimer);
    this.ws?.close();
  }
}
