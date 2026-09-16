// Scrollback overlay: the pane's scrollback stream (`recent` source for
// plain panes, `visible` ANSI for agent panes — SPEC §3.5). Rendered as
// colored selectable text — browser selection IS copy mode — with the
// agent console's input box stripped (footer.ts). The parent declares
// scrollback interest in its `view`; this component only reads the screen
// bus.

import { useEffect, useState } from "react";
import { screens, type ScreenSnapshot } from "../screens.ts";
import { splitFooter, paneModes } from "../footer.ts";
import { AnsiLines } from "./AnsiText.tsx";

interface Props {
  server: string;
  pane: string;
  title: string;
  onClose: () => void;
}

export function ScrollbackOverlay({ server, pane, title, onClose }: Props) {
  const [snap, setSnap] = useState<ScreenSnapshot | null>(null);

  useEffect(() => {
    return screens.subscribe(server, pane, "recent", setSnap);
  }, [server, pane]);

  // Closing on Escape is safe here — raw mode is a separate surface.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const split = snap ? splitFooter(snap.lines) : null;
  useEffect(() => {
    if (snap) paneModes.note(server, pane, splitFooter(snap.lines).mode);
  }, [snap, server, pane]);

  return (
    <div className="scrollback-overlay">
      <div className="scrollback-head">
        <span>
          scrollback — {title} {snap?.truncated ? "(truncated)" : ""}
        </span>
        <button onClick={onClose}>close (esc)</button>
      </div>
      <pre className="scrollback-body selectable" tabIndex={0}>
        {split ? <AnsiLines lines={split.body} /> : "loading…"}
      </pre>
    </div>
  );
}
