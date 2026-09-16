// Append-only transcript per pane. The hub polls a pane's `recent`
// scrollback (500-line sliding window) and pushes full-text snapshots; this
// store diffs consecutive windows and appends only the new lines, so the
// transcript grows without bound (memory-capped) and is never overwritten
// by TUI redraws. The agent console's bottom input box is stripped before
// diffing (footer.ts). It deliberately shares nothing with the app store —
// it is a log the client owns, decoupled from herdr's live screen.

import { screens } from "./screens.ts";
import { stripAnsi } from "./ansi.ts";
import { splitFooter, paneModes } from "./footer.ts";

/** Hard cap on accumulated lines per pane (drop from the head). */
export const MAX_LINES = 20_000;

export interface TranscriptState {
  lines: string[];
  /** The last `recent` window fed in — the diff baseline. */
  lastWindow: string[];
  /** True once older lines were dropped to respect MAX_LINES. */
  headTruncated: boolean;
}

/**
 * Advance a transcript by one window. Pure — unit-tested.
 *
 * Lines may carry ANSI styling (agent panes are fed `visible` ANSI so the
 * transcript can render colors); alignment compares VISIBLE text only, so
 * a line that was merely restyled (spinner frames, color shifts) is not
 * duplicated.
 *
 * The window slides: once output exceeds the hub's cap, its first lines
 * fall off. We therefore align by the largest suffix of `lastWindow` that
 * equals a prefix of `win` and append the remainder. No overlap at all
 * means the pane's history was reset (clear / alternate-screen app):
 * keep what we have, add a separator, continue from the new window.
 */
export function advance(
  prev: TranscriptState | null,
  win: string[],
): TranscriptState {
  if (!win.length) return prev ?? { lines: [], lastWindow: [], headTruncated: false };
  if (!prev) return { lines: win.slice(), lastWindow: win.slice(), headTruncated: false };

  const { lines, lastWindow } = prev;
  const maxM = Math.min(lastWindow.length, win.length);
  for (let m = maxM; m > 0; m--) {
    let ok = true;
    for (let i = 0; i < m; i++) {
      if (stripAnsi(lastWindow[lastWindow.length - m + i]) !== stripAnsi(win[i])) {
        ok = false;
        break;
      }
    }
    if (ok) {
      const fresh = win.slice(m);
      if (!fresh.length) return prev; // no new output
      return cap({ ...prev, lines: lines.concat(fresh), lastWindow: win.slice() });
    }
  }

  // Complete rewrite: no line of continuity. Append with a separator so
  // nothing already shown is lost or duplicated mid-stream.
  const sep = lines.length ? ["", "⋯ history reset ⋯", ""] : [];
  return cap({ ...prev, lines: lines.concat(sep, win), lastWindow: win.slice() });
}

function cap(st: TranscriptState): TranscriptState {
  if (st.lines.length <= MAX_LINES) return st;
  return {
    ...st,
    lines: st.lines.slice(st.lines.length - MAX_LINES),
    headTruncated: true,
  };
}

type Listener = (st: TranscriptState) => void;

class TranscriptStore {
  private entries = new Map<string, TranscriptState>();
  private listeners = new Map<string, Set<Listener>>();

  private key(server: string, pane: string) {
    return `${server}#${pane}`;
  }

  get(server: string, pane: string): TranscriptState | null {
    return this.entries.get(this.key(server, pane)) ?? null;
  }

  feed(server: string, pane: string, win: string[]): void {
    const k = this.key(server, pane);
    const next = advance(this.entries.get(k) ?? null, win);
    if (next === this.entries.get(k)) return; // unchanged (advance may return prev)
    this.entries.set(k, next);
    const set = this.listeners.get(k);
    if (set) for (const cb of set) cb(next);
  }

  subscribe(server: string, pane: string, cb: Listener): () => void {
    const k = this.key(server, pane);
    let set = this.listeners.get(k);
    if (!set) {
      set = new Set();
      this.listeners.set(k, set);
    }
    set.add(cb);
    const cur = this.entries.get(k);
    if (cur) cb(cur);
    return () => {
      set!.delete(cb);
      if (set!.size === 0) this.listeners.delete(k);
    };
  }
}

export const transcript = new TranscriptStore();

/**
 * Wire one pane to the screen bus: every `recent` snapshot feeds the
 * transcript. Returns an unsubscribe. (Kept here so components stay dumb.)
 */
export function attachRecentStream(server: string, pane: string): () => void {
  return screens.subscribe(server, pane, "recent", (snap) => {
    if (!snap) return;
    // The agent console's input box / hint line never enters the log —
    // the web client has its own composer. The parsed mode feeds the
    // composer's pill (footer.ts).
    const { body, mode } = splitFooter(snap.lines);
    paneModes.note(server, pane, mode);
    transcript.feed(server, pane, body);
  });
}
