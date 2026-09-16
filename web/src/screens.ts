// Screen bus: pub/sub for pane content, keyed by pane id. Screen frames
// NEVER enter the app store — at 10 Hz they must not re-render the sidebar
// (SPEC §6/§8). Only Terminal components subscribe here. Rows-mode frames are
// merged onto the last known grid; a full frame replaces it. A rows frame
// with a mismatched base (a dropped frame self-healed by the hub into a full
// send) is ignored — the next full frame will resync.

import type { ScreenFrame } from "./types.ts";

export interface ScreenSnapshot {
  revision: number;
  rows: number;
  cols: number;
  lines: string[];
  truncated: boolean;
  updatedAt: number;
}

type Listener = (snap: ScreenSnapshot | null) => void;

class ScreenBus {
  private latest = new Map<string, ScreenSnapshot>();
  private listeners = new Map<string, Set<Listener>>();

  /** Subscribe to one pane's visible screen. Returns the current snapshot
   *  synchronously via the callback, then on every update. */
  subscribe(server: string, pane: string, source: "visible" | "recent", cb: Listener): () => void {
    const key = `${server}#${source}#${pane}`;
    let set = this.listeners.get(key);
    if (!set) {
      set = new Set();
      this.listeners.set(key, set);
    }
    set.add(cb);
    cb(this.latest.get(key) ?? null);
    return () => {
      set!.delete(cb);
      if (set!.size === 0) {
        this.listeners.delete(key);
        this.latest.delete(key);
      }
    };
  }

  apply(frame: ScreenFrame): void {
    const key = `${frame.server}#${frame.source}#${frame.pane}`;
    const prev = this.latest.get(key);
    let snap: ScreenSnapshot;
    if (frame.mode === "full" || !prev || prev.rows !== frame.rows) {
      snap = {
        revision: frame.revision,
        rows: frame.rows,
        cols: frame.cols,
        lines: frame.lines ?? [],
        truncated: !!frame.truncated,
        updatedAt: Date.now(),
      };
    } else {
      // Rows delta on top of the previous grid.
      const lines = prev.lines.slice(0, frame.rows);
      while (lines.length < frame.rows) lines.push("");
      for (const c of frame.changes ?? []) lines[c.row] = c.text;
      snap = {
        revision: frame.revision,
        rows: frame.rows,
        cols: frame.cols,
        lines,
        truncated: !!frame.truncated,
        updatedAt: Date.now(),
      };
    }
    this.latest.set(key, snap);
    const set = this.listeners.get(key);
    if (set) for (const cb of set) cb(snap);
  }

  /** Drop everything for one server (reconnect resyncs via full frames). */
  invalidateServer(server: string): void {
    for (const key of [...this.listeners.keys()]) {
      if (key.startsWith(`${server}#`)) {
        this.listeners.delete(key);
        this.latest.delete(key);
      }
    }
  }
}

export const screens = new ScreenBus();
