// Renders ANSI-styled pane text as colored spans (diff red/green, dim
// hints, ...). Per-line memoization keeps re-parsing proportional to NEW
// output, not to transcript size; text content is plain data, so React's
// escaping makes this injection-safe.

import { memo, type CSSProperties, type ReactNode } from "react";
import { parseAnsi, type SegStyle } from "../ansi.ts";

function css(s: SegStyle): CSSProperties {
  const st: CSSProperties = {};
  if (s.fg) st.color = s.fg;
  if (s.bg) st.backgroundColor = s.bg;
  if (s.bold) st.fontWeight = 700;
  if (s.dim) st.opacity = 0.55;
  if (s.italic) st.fontStyle = "italic";
  if (s.underline) st.textDecoration = "underline";
  return st;
}

export const AnsiLine = memo(function AnsiLine({ text }: { text: string }) {
  const segs = parseAnsi(text);
  if (!segs.length) return null;
  return (
    <>
      {segs.map((s, i) => (
        <span key={i} style={css(s.style)}>
          {s.text}
        </span>
      ))}
    </>
  );
});

/**
 * Render lines as one pre-wrapped flow (the parent supplies
 * `white-space: pre-wrap`), newline-separated, `from` = absolute index of
 * `lines[0]` for stable React keys in append-only arrays.
 */
export function AnsiLines({ lines, from = 0 }: { lines: string[]; from?: number }) {
  const out: ReactNode[] = [];
  for (let i = 0; i < lines.length; i++) {
    if (i) out.push("\n");
    out.push(<AnsiLine key={from + i} text={lines[i]} />);
  }
  return <>{out}</>;
}
