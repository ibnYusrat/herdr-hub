// Minimal ANSI-to-styles parser for rendering pane text as colored HTML.
// Handles the SGR subset terminals actually emit for text styling —
// fg/bg (basic, bright, 256-color, RGB), bold, dim, italic, underline,
// reverse — and strips every other escape sequence (cursor movement,
// erase, OSC titles) so it can never leak into the DOM. Text content is
// returned as plain data; React escapes it at render time.

export interface SegStyle {
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

export interface Seg {
  text: string;
  style: SegStyle;
}

const BASE16 = [
  "#000000", "#800000", "#008000", "#808000",
  "#000080", "#800080", "#008080", "#c0c0c0",
  "#808080", "#ff0000", "#00ff00", "#ffff00",
  "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
];

const CUBE = [0, 95, 135, 175, 215, 255];

function color256(n: number): string {
  if (n < 16) return BASE16[n];
  if (n < 232) {
    const v = n - 16;
    return `rgb(${CUBE[(v / 36) | 0]},${CUBE[((v / 6) | 0) % 6]},${CUBE[v % 6]})`;
  }
  const g = 8 + 10 * (n - 232);
  return `rgb(${g},${g},${g})`;
}

/** Parse one chunk (may contain newlines) into styled segments. */
export function parseAnsi(input: string): Seg[] {
  const segs: Seg[] = [];
  let style: SegStyle = {};
  let text = "";
  let i = 0;
  const n = input.length;

  const flush = () => {
    if (text) {
      segs.push({ text, style });
      text = "";
    }
  };

  while (i < n) {
    const c = input[i];
    if (c !== "\x1b") {
      text += c;
      i++;
      continue;
    }
    // CSI: ESC [ <params> <final>
    if (input[i + 1] === "[") {
      let j = i + 2;
      while (j < n && input.charCodeAt(j) >= 0x30 && input.charCodeAt(j) <= 0x3f) j++;
      const params = input.slice(i + 2, j);
      while (j < n && input.charCodeAt(j) >= 0x20 && input.charCodeAt(j) <= 0x2f) j++;
      const final = j < n ? input[j] : "";
      if (final === "m") {
        flush();
        style = applySgr(params, style);
      }
      i = j + 1;
      continue;
    }
    // OSC: ESC ] ... (BEL or ST)
    if (input[i + 1] === "]") {
      let j = i + 2;
      while (j < n) {
        if (input[j] === "\x07") { j++; break; }
        if (input[j] === "\x1b" && input[j + 1] === "\\") { j += 2; break; }
        j++;
      }
      i = j;
      continue;
    }
    // Any other escape: optional intermediates (0x20-0x2F) then one final
    // byte (0x30-0x7E) — covers charset selection `ESC ( B` and friends.
    let j = i + 1;
    while (j < n && input.charCodeAt(j) >= 0x20 && input.charCodeAt(j) <= 0x2f) j++;
    if (j < n && input.charCodeAt(j) >= 0x30 && input.charCodeAt(j) <= 0x7e) j++;
    i = j;
  }
  flush();
  return segs;
}

function applySgr(params: string, prev: SegStyle): SegStyle {
  let s: SegStyle = { ...prev };
  const parts = (params.includes(";") ? params.split(";") : params === "" ? ["0"] : [params]);
  for (let k = 0; k < parts.length; k++) {
    const p = parts[k] === "" ? "0" : parts[k];
    const v = Number(p);
    if (!Number.isInteger(v) || v < 0) continue;
    switch (true) {
      case v === 0: s = {}; break;
      case v === 1: s.bold = true; break;
      case v === 2: s.dim = true; break;
      case v === 3: s.italic = true; break;
      case v === 4: s.underline = true; break;
      case v === 7: {
        const fg = s.fg;
        s.fg = s.bg;
        s.bg = fg;
        break;
      }
      case v === 22: delete s.bold; delete s.dim; break;
      case v === 23: delete s.italic; break;
      case v === 24: delete s.underline; break;
      case v >= 30 && v <= 37: s.fg = BASE16[v - 30]; break;
      case v === 39: delete s.fg; break;
      case v >= 40 && v <= 47: s.bg = BASE16[v - 40]; break;
      case v === 49: delete s.bg; break;
      case v >= 90 && v <= 97: s.fg = BASE16[v - 90 + 8]; break;
      case v >= 100 && v <= 107: s.bg = BASE16[v - 100 + 8]; break;
      case v === 38 || v === 48: {
        const mode = Number(parts[k + 1]);
        if (mode === 5 && parts[k + 2] !== undefined) {
          const c = color256(Number(parts[k + 2]));
          if (v === 38) s.fg = c; else s.bg = c;
          k += 2;
        } else if (mode === 2 && parts[k + 4] !== undefined) {
          const c = `rgb(${parts[k + 2]},${parts[k + 3]},${parts[k + 4]})`;
          if (v === 38) s.fg = c; else s.bg = c;
          k += 4;
        }
        break;
      }
      default:
        break;
    }
  }
  return s;
}

/** Visible text with every escape sequence removed. */
export function stripAnsi(input: string): string {
  let out = "";
  let i = 0;
  const n = input.length;
  while (i < n) {
    const c = input[i];
    if (c !== "\x1b") {
      out += c;
      i++;
      continue;
    }
    if (input[i + 1] === "[") {
      // params (0x30-0x3F), intermediates (0x20-0x2F), one final byte.
      let j = i + 2;
      while (j < n && input.charCodeAt(j) >= 0x30 && input.charCodeAt(j) <= 0x3f) j++;
      while (j < n && input.charCodeAt(j) >= 0x20 && input.charCodeAt(j) <= 0x2f) j++;
      j++; // final byte
      i = j;
      continue;
    }
    if (input[i + 1] === "]") {
      let j = i + 2;
      while (j < n) {
        if (input[j] === "\x07") { j++; break; }
        if (input[j] === "\x1b" && input[j + 1] === "\\") { j += 2; break; }
        j++;
      }
      i = j;
      continue;
    }
    i += 2;
  }
  return out;
}
