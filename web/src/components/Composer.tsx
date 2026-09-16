// Composer: input line under the pane grid. Agent panes send `agent.prompt`
// (one in flight, agent_blocked is surfaced and NEVER auto-retried); plain
// panes send the text plus an enter key. For agent panes the console's
// mode/hint line (stripped everywhere else, footer.ts) resurfaces here as
// an HTML pill — e.g. `⏵⏵ auto mode on`.

import { useEffect, useState } from "react";
import { promptAgent, sendLine } from "../actions.ts";
import { paneModes, type PaneMode } from "../footer.ts";
import type { PaneInfo } from "../types.ts";

interface Props {
  server: string;
  pane: PaneInfo | null;
  /** Reported once via the banner until cleared. */
  onNotice: (msg: string | null) => void;
}

/** Latest permission mode parsed from the pane's console footer. */
function usePaneMode(server: string, paneId: string | null): PaneMode | null {
  const [mode, setMode] = useState<PaneMode | null>(null);
  useEffect(() => {
    setMode(null);
    if (!paneId) return;
    return paneModes.subscribe(server, paneId, (m) => setMode(m));
  }, [server, paneId]);
  return mode;
}

export function Composer({ server, pane, onNotice }: Props) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const isAgent = !!pane?.agent;
  const blocked = pane?.agent_status === "blocked";
  const mode = usePaneMode(server, pane?.pane_id ?? null);

  async function submit() {
    if (!pane || !text.trim() || busy) return;
    setBusy(true);
    try {
      if (isAgent) {
        const resp = await promptAgent(server, pane.pane_id, text);
        if (resp.ok) {
          setText("");
          onNotice(null);
        } else if (resp.error?.code === "agent_blocked") {
          onNotice(`${pane.agent} is blocked — prompt not sent. Answer it from the toolbar.`);
        } else if (resp.error?.code === "hub.prompt_in_flight") {
          onNotice("a prompt is already in flight for this agent");
        } else {
          onNotice(resp.error?.code ?? "prompt failed");
        }
      } else {
        await sendLine(server, pane.pane_id, text);
        setText("");
        onNotice(null);
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      className={"composer" + (blocked ? " blocked" : "")}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {isAgent && <span className="composer-agent">{pane?.agent}</span>}
      {isAgent && mode && (
        <span
          className={`composer-mode mode-${mode.kind}`}
          title="agent permission mode (parsed from the console footer)"
        >
          {mode.label}
        </span>
      )}
      <input
        value={text}
        placeholder={
          !pane
            ? "no pane focused"
            : isAgent
              ? blocked
                ? `blocked — ${pane.agent} will not take a prompt`
                : `prompt ${pane.agent}…`
              : "send a line to the pane…"
        }
        disabled={!pane || busy}
        onChange={(e) => setText(e.target.value)}
      />
      <button type="submit" disabled={!pane || !text.trim() || busy}>
        send
      </button>
    </form>
  );
}
