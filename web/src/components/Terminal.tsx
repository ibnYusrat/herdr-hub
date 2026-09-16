// Read-only terminal renderer. xterm is used write-only as a grid of cells:
// every screen frame rewrites the full grid with ESC[H ESC[2J (no per-frame
// reset), the grid is resized only when the server's cell geometry changes,
// the cursor is hidden, and the rendered screen is scaled with a CSS
// transform to fit the pane rect (server grid is authoritative, SPEC §6).

import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { screens, type ScreenSnapshot } from "../screens.ts";
import { splitFooter, paneModes } from "../footer.ts";

interface Props {
  server: string;
  pane: string;
}

export function Terminal({ server, pane }: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new XTerm({
      scrollback: 0,
      cursorBlink: false,
      cursorStyle: "bar",
      convertEol: false,
      allowProposedApi: false,
      // Literal family (not a CSS var): xterm measures cell geometry with
      // this string and the canvas renderer can't resolve vars.
      fontFamily:
        '"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace',
      fontSize: 13,
      theme: {
        background: "#0b0e14",
        foreground: "#c8ccd4",
      },
    });
    term.open(host);

    let grid: { cols: number; rows: number } | null = null;

    const scale = () => {
      const screenEl = host.querySelector<HTMLElement>(".xterm-screen");
      if (!screenEl) return;
      const sw = screenEl.offsetWidth;
      const sh = screenEl.offsetHeight;
      if (!sw || !sh) return;
      const s = Math.min(host.clientWidth / sw, host.clientHeight / sh);
      screenEl.style.transformOrigin = "top left";
      screenEl.style.transform = `scale(${s})`;
    };
    const ro = new ResizeObserver(scale);
    ro.observe(host);

    const render = (snap: ScreenSnapshot | null) => {
      if (!snap) return;
      if (!grid || grid.cols !== snap.cols || grid.rows !== snap.rows) {
        term.resize(snap.cols, snap.rows);
        grid = { cols: snap.cols, rows: snap.rows };
        // Give xterm a frame to lay out the new grid before scaling.
        requestAnimationFrame(scale);
      }
      // Full rewrite; hidden cursor. Lines may contain ANSI colors. The
      // agent console's input box + hint line are blanked rather than
      // mirrored — the web client has its own composer (footer.ts); the
      // parsed mode surfaces as the composer's pill.
      const { body, mode } = splitFooter(snap.lines);
      paneModes.note(server, pane, mode);
      term.write(`\x1b[?25l\x1b[H\x1b[2J${body.join("\r\n")}`);
    };

    const unsub = screens.subscribe(server, pane, "visible", render);

    return () => {
      ro.disconnect();
      unsub();
      term.dispose();
    };
  }, [server, pane]);

  return <div ref={hostRef} className="term-host" />;
}
