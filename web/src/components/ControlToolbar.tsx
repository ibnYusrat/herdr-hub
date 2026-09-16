// Control toolbar: herdr key combos for the focused pane (SPEC §7) —
// enter/esc/arrows/tab/y/n/ctrl+c/digits via agent|pane.send_keys, plus
// zoom and split. Highlights amber while the pane's agent is blocked.

import { sendKeys, act } from "../actions.ts";
import type { PaneInfo } from "../types.ts";

interface Props {
  server: string;
  pane: PaneInfo | null;
  rawMode: boolean;
  onToggleRaw: () => void;
  transcriptMode: boolean;
  onToggleTranscript: () => void;
  onRenamePane: () => void;
  onClosePane: () => void;
  onScrollback: () => void;
  notifications: NotificationPermission | "unsupported";
  onToggleNotifications: () => void;
}

const KEYS: { label: string; keys: string[]; title: string }[] = [
  { label: "⏎", keys: ["enter"], title: "enter" },
  { label: "esc", keys: ["esc"], title: "escape" },
  { label: "↑", keys: ["up"], title: "up" },
  { label: "↓", keys: ["down"], title: "down" },
  { label: "tab", keys: ["tab"], title: "tab" },
  { label: "⇧tab", keys: ["shift+tab"], title: "shift+tab" },
  { label: "y", keys: ["y"], title: "y" },
  { label: "n", keys: ["n"], title: "n" },
  { label: "^c", keys: ["ctrl+c"], title: "ctrl+c" },
];

export function ControlToolbar({
  server,
  pane,
  rawMode,
  onToggleRaw,
  transcriptMode,
  onToggleTranscript,
  onRenamePane,
  onClosePane,
  onScrollback,
  notifications,
  onToggleNotifications,
}: Props) {
  const isAgent = !!pane?.agent;
  const blocked = pane?.agent_status === "blocked";

  const press = (keys: string[]) => {
    if (pane) void sendKeys(server, pane.pane_id, isAgent, keys);
  };

  return (
    <div className={"control-toolbar" + (blocked ? " blocked" : "")} role="toolbar">
      {pane && KEYS.map((k) => (
        <button key={k.label} title={k.title} onClick={() => press(k.keys)}>
          {k.label}
        </button>
      ))}
      {pane && (
        <span className="toolbar-digits">
          {[1, 2, 3, 4, 5, 6, 7, 8, 9].map((d) => (
            <button key={d} title={`digit ${d}`} onClick={() => press([String(d)])}>
              {d}
            </button>
          ))}
        </span>
      )}
      <span className="toolbar-gap" />
      {pane && (
        <>
          <button
            title="toggle zoom"
            onClick={() => void act(server, "pane.zoom", { pane_id: pane.pane_id, mode: "toggle" })}
          >
            zoom
          </button>
          <button
            title="split right"
            onClick={() =>
              void act(server, "pane.split", { direction: "right", target_pane_id: pane.pane_id, focus: false })
            }
          >
            split │
          </button>
          <button
            title="split down"
            onClick={() =>
              void act(server, "pane.split", { direction: "down", target_pane_id: pane.pane_id, focus: false })
            }
          >
            split ─
          </button>
          <button title="rename pane" onClick={onRenamePane}>✎</button>
          <button title="close pane" className="danger" onClick={onClosePane}>×</button>
          <button title="scrollback" onClick={onScrollback}>log</button>
        </>
      )}
      <button
        className={transcriptMode ? "txt-on" : ""}
        title="transcript view: append-only text of the focused pane, independent scroll (vs the live grid)"
        onClick={onToggleTranscript}
      >
        TXT
      </button>
      <button
        className={rawMode ? "raw-on" : ""}
        title="raw mode: keys go straight to the pane (pane.send_input); esc exits"
        onClick={onToggleRaw}
      >
        RAW
      </button>
      {notifications !== "unsupported" && (
        <button
          className={notifications === "granted" ? "notif-on" : ""}
          title={`notifications ${notifications}`}
          onClick={onToggleNotifications}
        >
          🔔
        </button>
      )}
    </div>
  );
}
