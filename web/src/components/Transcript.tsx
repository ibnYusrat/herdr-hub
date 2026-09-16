// Transcript view: the focused pane's output as colored (ANSI-rendered),
// wrapped, selectable text in its own scroller — appended, never
// overwritten, scroll position fully client-owned (sticky only while the
// user sits at the bottom). The parent declares the pane in
// `view.scrollback`; the hub then polls it (agent panes via `visible`
// ANSI) and this component accumulates via transcript.ts.

import { useEffect, useRef, useState } from "react";
import { attachRecentStream, transcript, type TranscriptState } from "../transcript.ts";
import { AnsiLines } from "./AnsiText.tsx";

interface Props {
  server: string;
  pane: string;
  dim: boolean;
}

const PIN_THRESHOLD_PX = 48;
/** DOM cap: lines beyond this are kept in memory but not rendered. */
const RENDER_CAP = 2000;

export function Transcript({ server, pane, dim }: Props) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const pinnedRef = useRef(true);
  const [st, setSt] = useState<TranscriptState | null>(null);
  const [showJump, setShowJump] = useState(false);

  useEffect(() => {
    setSt(transcript.get(server, pane));
    const unsubFeed = attachRecentStream(server, pane);
    const unsubStore = transcript.subscribe(server, pane, setSt);
    return () => {
      unsubFeed();
      unsubStore();
    };
  }, [server, pane]);

  // Keep pinned to the bottom while following; otherwise leave the user's
  // scroll position strictly alone.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
    setShowJump(!pinnedRef.current);
  }, [st]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < PIN_THRESHOLD_PX;
    setShowJump(!pinnedRef.current);
  };

  const jumpToLatest = () => {
    const el = scrollRef.current;
    if (!el) return;
    pinnedRef.current = true;
    el.scrollTop = el.scrollHeight;
    setShowJump(false);
  };

  const total = st ? st.lines.length : 0;
  const start = st ? Math.max(0, total - RENDER_CAP) : 0;

  return (
    <div className={"transcript-wrap" + (dim ? " dim" : "")}>
      <div ref={scrollRef} className="transcript selectable" onScroll={onScroll}>
        {st && st.headTruncated && <div className="transcript-note">⋯ oldest lines dropped</div>}
        {start > 0 && <div className="transcript-note">⋯ {start} older lines not rendered</div>}
        {st ? <AnsiLines lines={st.lines.slice(start)} from={start} /> : "waiting for output…"}
      </div>
      {showJump && (
        <button className="transcript-jump" onClick={jumpToLatest}>
          ↓ latest
        </button>
      )}
    </div>
  );
}
