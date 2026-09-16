// Agent-console footer detection. The agent console TUI pins an input box to
// the bottom of its viewport — a rule line carrying the workspace title, a
// dead `❯` input row, another rule, and a mode/hint line
// (`⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · …`). In the
// web client that whole block is noise: we have our own composer, so the
// console's own input area is redundant and misleading. Every rendering
// surface (grid terminal, TXT transcript, scrollback overlay) cuts the
// footer with `splitFooter()` before display, and surfaces call
// `paneModes.note()` so the composer can show the parsed mode as an HTML
// pill instead of raw console text. Pure — unit-tested against fixtures
// captured from a live session.

import { stripAnsi } from "./ansi.ts";

export interface PaneMode {
  /** Display label, e.g. `⏵⏵ auto mode on`. */
  label: string;
  kind: "auto" | "bypass" | "plan" | "other";
}

export interface FooterSplit {
  /** Lines with the console footer removed. */
  body: string[];
  /** The removed footer lines (bottom last); null when none detected. */
  footer: string[] | null;
  /** Mode parsed from the hint line, when a footer was detected. */
  mode: PaneMode | null;
}

/** Box rule: predominantly `─` (may carry a workspace title mid-line). */
function isRule(line: string): boolean {
  const t = stripAnsi(line).trim();
  const dashes = (t.match(/─/g) ?? []).length;
  if (dashes < 10) return false;
  return dashes / t.replace(/\s/g, "").length >= 0.6;
}

function isInputRow(line: string): boolean {
  return stripAnsi(line).trimStart().startsWith("❯");
}

/**
 * The hint line is the anchor: the block is only stripped when the LAST
 * line looks like the mode/hint line. Anchors are stable across idle and
 * working states and survive narrow-pane truncation (the `⏵⏵` glyph leads
 * the label in every variant observed).
 */
function isModeHint(line: string): boolean {
  const t = stripAnsi(line);
  return /[⏵⏸]/.test(t) || t.includes("(shift+tab to cycle") || t.includes("for agents");
}

function parseMode(hint: string): PaneMode {
  const label = stripAnsi(hint)
    .trim()
    .split(/\s*[(·]/)[0]
    .trim();
  const kind = /bypass/.test(label)
    ? "bypass"
    : /auto/.test(label)
      ? "auto"
      : /plan/.test(label)
        ? "plan"
        : "other";
  return { label, kind };
}

/**
 * Split a screen/window into body + console footer. The footer is the
 * maximal bottom suffix matching [rule?, input-row?, rule?, hint] — the
 * hint is required, so plain-pane content (even content that happens to
 * end with a `───` rule) is never touched.
 */
export function splitFooter(lines: string[]): FooterSplit {
  const n = lines.length;
  if (!n || !isModeHint(lines[n - 1])) {
    return { body: lines, footer: null, mode: null };
  }
  let k = 1;
  if (n >= 2 && isRule(lines[n - 2])) k = 2;
  if (k === 2 && n >= 3 && isInputRow(lines[n - 3])) k = 3;
  if (k === 3 && n >= 4 && isRule(lines[n - 4])) k = 4;
  return {
    body: lines.slice(0, n - k),
    footer: lines.slice(n - k),
    mode: parseMode(lines[n - 1]),
  };
}

type ModeListener = (m: PaneMode | null) => void;

/** Latest parsed mode per pane, fed by whichever surface sees its frames. */
class ModeStore {
  private latest = new Map<string, PaneMode | null>();
  private listeners = new Map<string, Set<ModeListener>>();

  note(server: string, pane: string, mode: PaneMode | null): void {
    const k = `${server}#${pane}`;
    const prev = this.latest.get(k) ?? null;
    if (prev?.label === mode?.label && prev?.kind === mode?.kind) return;
    this.latest.set(k, mode);
    const set = this.listeners.get(k);
    if (set) for (const cb of set) cb(mode);
  }

  subscribe(server: string, pane: string, cb: ModeListener): () => void {
    const k = `${server}#${pane}`;
    let set = this.listeners.get(k);
    if (!set) {
      set = new Set();
      this.listeners.set(k, set);
    }
    set.add(cb);
    cb(this.latest.get(k) ?? null);
    return () => {
      set!.delete(cb);
      if (!set!.size) this.listeners.delete(k);
    };
  }
}

export const paneModes = new ModeStore();
