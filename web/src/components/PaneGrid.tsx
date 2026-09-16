// Pane grid for one tab: panes positioned absolutely from the flat layout
// rects (`pane.rect / tab.area` in cells), split dividers drawn from
// `splits[].rect`. The server grid is authoritative; the browser viewport
// only scales it (SPEC §5.3).

import type { PaneInfo, PaneLayoutSnapshot } from "../types.ts";
import { Terminal } from "./Terminal.tsx";

interface Props {
  server: string;
  layout: PaneLayoutSnapshot;
  panes: Record<string, PaneInfo>;
  dim: boolean;
  onFocusPane: (paneId: string) => void;
}

export function PaneGrid({ server, layout, panes, dim, onFocusPane }: Props) {
  const area = layout.area;
  const pct = (r: { x: number; y: number; width: number; height: number }) => ({
    left: `${(100 * r.x) / area.width}%`,
    top: `${(100 * r.y) / area.height}%`,
    width: `${(100 * r.width) / area.width}%`,
    height: `${(100 * r.height) / area.height}%`,
  });

  // Zoomed: only the focused pane, filling the area.
  const entries = layout.zoomed
    ? layout.panes.filter((e) => e.pane_id === layout.focused_pane_id)
    : layout.panes;

  return (
    <div
      className={`pane-grid ${dim ? "dim" : ""}`}
      style={{ "--grid-ratio": `${area.width} / ${area.height}` } as React.CSSProperties}
    >
      {!layout.zoomed &&
        layout.splits.map((s) => (
          <div key={s.id} className="split" style={pct(s.rect) as any} />
        ))}
      {entries.map((entry) => {
        const pane = panes[entry.pane_id];
        return (
          <div
            key={entry.pane_id}
            className={`pane ${entry.focused ? "focused" : ""} ${
              pane?.agent_status ? `st-${pane.agent_status}` : ""
            }`}
            style={pct(entry.rect) as any}
            onMouseDown={() => onFocusPane(entry.pane_id)}
          >
            <Terminal server={server} pane={entry.pane_id} />
          </div>
        );
      })}
    </div>
  );
}
