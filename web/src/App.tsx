// App shell: token gate, hub connection lifecycle, store wiring, view
// declaration (debounced, full-replace), main layout. M3 adds the composer,
// control toolbar, overlays, scrollback, raw mode, and notifications.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { HubConnection } from "./hub.ts";
import { screens } from "./screens.ts";
import { useStore } from "./store.ts";
import type { HubFrame, ScreenFrame } from "./types.ts";
import { setActionConnection } from "./actions.ts";
import { requestNotificationPermission, trackAgentStatus } from "./notify.ts";
import { Sidebar } from "./components/Sidebar.tsx";
import { TabBar } from "./components/TabBar.tsx";
import { PaneGrid } from "./components/PaneGrid.tsx";
import { Transcript } from "./components/Transcript.tsx";
import { ControlToolbar } from "./components/ControlToolbar.tsx";
import { Composer } from "./components/Composer.tsx";
import { OverlayHost, type Overlay } from "./components/Overlays.tsx";
import { ScrollbackOverlay } from "./components/ScrollbackOverlay.tsx";

const TOKEN_KEY = "herdr-hub-token";

export function App() {
  const [token, setToken] = useState<string | null>(() => localStorage.getItem(TOKEN_KEY));
  if (token === null) {
    return <TokenPrompt onSave={setToken} />;
  }
  return <Connected token={token} onBadToken={() => setToken(null)} />;
}

function TokenPrompt({ onSave }: { onSave: (t: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <div className="token-gate">
      <h1>herdr</h1>
      <p>Paste the hub token — printed by the hub on first run, or run:</p>
      <pre>herdr-hub token</pre>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          const t = value.trim();
          if (t) {
            localStorage.setItem(TOKEN_KEY, t);
            onSave(t);
          }
        }}
      >
        <input
          type="password"
          autoFocus
          placeholder="token"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
        <button type="submit">connect</button>
      </form>
    </div>
  );
}

function Connected({ token, onBadToken }: { token: string; onBadToken: () => void }) {
  const store = useStore;
  const connRef = useRef<HubConnection | null>(null);

  useEffect(() => {
    const conn = new HubConnection(token, {
      onFrame: (frame: HubFrame) => {
        switch (frame.type) {
          case "welcome":
            store.getState().welcome(frame.servers);
            break;
          case "snapshot": {
            // On pre-0.9.0 servers the hub runs in poll mode (no event
            // frames — herdr replays history on every subscription), so
            // agent-status transitions are derived from snapshot diffs to
            // keep notifications working.
            const before = store.getState().servers[frame.server]?.state.panes ?? {};
            for (const p of frame.state.panes ?? []) {
              const prev = before[p.pane_id];
              if (prev && prev.agent_status !== p.agent_status) {
                trackAgentStatus(
                  frame.server,
                  p.pane_id,
                  p.agent_status,
                  p.display_agent ?? p.agent ?? p.pane_id,
                );
              }
            }
            store.getState().snapshot(frame);
            break;
          }
          case "event":
            store.getState().event(frame.server, frame.event, frame.data);
            if (frame.event === "pane_agent_status_changed") {
              const d = frame.data ?? {};
              trackAgentStatus(
                frame.server,
                d.pane_id,
                d.agent_status,
                d.display_agent ?? d.agent ?? d.pane_id,
              );
            }
            break;
          case "server_status":
            store.getState().serverStatus(frame.server, frame.status, frame.detail);
            if (frame.status !== "online") screens.invalidateServer(frame.server);
            break;
          case "screen":
            screens.apply(frame as ScreenFrame);
            break;
          case "response":
            if (frame.error?.code === "hub.auth_failed") {
              localStorage.removeItem(TOKEN_KEY);
              onBadToken();
            }
            break;
          default:
            break;
        }
      },
      onConnectionChange: (connected) => store.getState().setConnected(connected),
    });
    connRef.current = conn;
    setActionConnection(conn);
    conn.connect();
    return () => {
      setActionConnection(null);
      conn.close();
    };
  }, [token, onBadToken, store]);

  const connected = useStore((s) => s.connected);
  const servers = useStore((s) => s.servers);
  const order = useStore((s) => s.order);
  const activeServerId = useStore((s) => s.activeServer);
  const activeTabStored = useStore((s) => s.activeTab);
  const selectTab = useStore((s) => s.selectTab);
  const selectServer = useStore((s) => s.selectServer);

  const viewServer = useStore((s) => s.viewServer)();
  const serverId = viewServer?.info.id ?? null;
  const state = viewServer?.state;
  const dim = !!viewServer && viewServer.info.status !== "online";

  // M3 interaction state.
  const [overlay, setOverlay] = useState<Overlay | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [scrollbackPane, setScrollbackPane] = useState<string | null>(null);
  const [rawMode, setRawMode] = useState(false);
  // Transcript mode: append-only text view of the focused pane instead of
  // the mirrored live grid (persisted; on by default).
  const [transcriptMode, setTranscriptMode] = useState(
    () => localStorage.getItem("herdr-hub-transcript") !== "off",
  );
  const toggleTranscript = useCallback(() => {
    setTranscriptMode((v) => {
      localStorage.setItem("herdr-hub-transcript", v ? "off" : "on");
      return !v;
    });
  }, []);
  const [notifPerm, setNotifPerm] = useState<NotificationPermission | "unsupported">(
    typeof Notification !== "undefined" ? Notification.permission : "unsupported",
  );
  // Mobile (<700px container): the sidebar is an off-canvas drawer.
  const [drawer, setDrawer] = useState(false);

  // Resolve the tab to show: explicit selection, else the server's focus.
  const activeTab = useMemo(() => {
    if (activeTabStored && activeServerId === serverId && state?.tabs[activeTabStored]) {
      return activeTabStored;
    }
    if (!state) return null;
    if (state.focused.tab && state.tabs[state.focused.tab]) return state.focused.tab;
    const ws = state.focused.workspace
      ? state.workspaces[state.focused.workspace]
      : Object.values(state.workspaces)[0];
    const tabId = ws?.active_tab_id ?? null;
    return tabId && state.tabs[tabId] ? tabId : null;
  }, [activeTabStored, activeServerId, serverId, state]);

  const layout = activeTab && state ? (state.layouts[activeTab] ?? null) : null;
  const panes = state?.panes ?? {};

  const focusedPaneId = layout?.focused_pane_id ?? null;
  const focusedPane = focusedPaneId ? (panes[focusedPaneId] ?? null) : null;

  // Declare the view whenever the visible pane set changes (debounced in
  // hub.ts; the hub diffs against its poller interest).
  const paneKey = layout ? layout.panes.map((p) => p.pane_id).join(",") : "";
  useEffect(() => {
    if (!serverId) return;
    const focusedWs = state?.focused.workspace ?? null;
    const panes = (layout?.panes ?? []).map((entry) => ({
      pane: entry.pane_id,
      focused: entry.pane_id === layout?.focused_pane_id,
    }));
    // Scrollback interest: the overlay pane, plus the focused pane while
    // transcript mode is on (the hub then polls `recent` for it).
    const scrollback = [
      ...(scrollbackPane ? [scrollbackPane] : []),
      ...(transcriptMode && layout?.focused_pane_id ? [layout.focused_pane_id] : []),
    ];
    connRef.current?.sendView({
      server: serverId,
      active: { workspace: focusedWs, tab: activeTab },
      panes,
      scrollback,
    });
  }, [serverId, activeTab, paneKey, layout, state, scrollbackPane, transcriptMode]);

  const focusTab = useCallback(
    (tabId: string) => {
      if (serverId) {
        selectTab(serverId, tabId);
        connRef.current?.request(serverId, "tab.focus", { tab_id: tabId });
      }
    },
    [serverId, selectTab],
  );

  const focusPane = useCallback(
    (paneId: string) => {
      if (serverId) connRef.current?.request(serverId, "pane.focus", { pane_id: paneId });
    },
    [serverId],
  );

  const focusWorkspace = useCallback(
    (server: string, workspaceId: string) => {
      selectServer(server);
      setDrawer(false);
      connRef.current?.request(server, "workspace.focus", { workspace_id: workspaceId });
    },
    [selectServer],
  );

  // Raw mode: keystrokes go straight to the focused pane (pane.send_input).
  // Esc always exits raw mode first; everything else is forwarded.
  useEffect(() => {
    if (!rawMode || !serverId || !focusedPaneId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setRawMode(false);
        return;
      }
      if (e.metaKey || e.altKey || e.ctrlKey) return; // browser shortcuts stay
      e.preventDefault();
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
      if (e.key.length === 1) {
        void connRef.current?.request(serverId, "pane.send_input", {
          pane_id: focusedPaneId,
          text: e.key,
        });
      } else {
        const named: Record<string, string> = {
          Enter: "enter",
          Backspace: "backspace",
          Tab: "tab",
          ArrowUp: "up",
          ArrowDown: "down",
          ArrowLeft: "left",
          ArrowRight: "right",
          Delete: "delete",
          Home: "home",
          End: "end",
          PageUp: "pageup",
          PageDown: "pagedown",
        };
        const k = named[e.key];
        if (k) void connRef.current?.request(serverId, "pane.send_input", { pane_id: focusedPaneId, keys: [k] });
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [rawMode, serverId, focusedPaneId]);

  const tabs = useMemo(() => {
    if (!state || !activeTab) return [];
    const wsId = state.tabs[activeTab]?.workspace_id;
    return Object.values(state.tabs).filter((t) => t.workspace_id === wsId);
  }, [state, activeTab]);

  const activeWorkspaceId = activeTab && state ? state.tabs[activeTab]?.workspace_id : null;

  const toggleNotifications = useCallback(() => {
    void requestNotificationPermission().then(setNotifPerm);
  }, []);

  return (
    <div className="app-shell">
      <div className={`app ${drawer ? "drawer-open" : ""}`}>
      {!connected && <div className="banner">reconnecting…</div>}
      {notice && (
        <div className="banner warn" onClick={() => setNotice(null)}>
          {notice} <span className="banner-dismiss">(dismiss)</span>
        </div>
      )}
      {drawer && <div className="drawer-backdrop" onClick={() => setDrawer(false)} />}
      <header className="mobile-bar">
        <button title="servers" onClick={() => setDrawer(true)}>☰</button>
        <span className="mobile-title">{viewServer?.info.label ?? "herdr"}</span>
      </header>
      <Sidebar
        servers={servers}
        order={order}
        activeServer={serverId}
        onSelectServer={(id) => {
          selectServer(id);
          setDrawer(false);
        }}
        onSelectWorkspace={focusWorkspace}
        onCreateWorkspace={(server) => serverId === server && setOverlay({ kind: "new-workspace" })}
        onRenameWorkspace={(server, ws) =>
          serverId === server &&
          setOverlay({ kind: "rename", target: "workspace", id: ws.workspace_id, current: ws.label })
        }
        onCloseWorkspace={(server, ws) =>
          serverId === server &&
          setOverlay({ kind: "close", target: "workspace", id: ws.workspace_id, label: ws.label, workspaceId: ws.workspace_id })
        }
        onWorktree={(server, mode) => serverId === server && setOverlay({ kind: "worktree", mode })}
        onWorktreeRemove={(server, ws) =>
          serverId === server &&
          setOverlay({ kind: "worktree-remove", workspaceId: ws.workspace_id, label: ws.label })
        }
      />
      <main className="main">
        {layout ? (
          <>
            <TabBar
              tabs={tabs}
              activeTab={activeTab}
              dim={dim}
              onSelect={focusTab}
              onNewTab={() =>
                activeWorkspaceId && setOverlay({ kind: "new-tab", workspaceId: activeWorkspaceId })
              }
              onRenameTab={(t) =>
                setOverlay({ kind: "rename", target: "tab", id: t.tab_id, current: t.label })
              }
              onCloseTab={(t) =>
                setOverlay({ kind: "close", target: "tab", id: t.tab_id, label: t.label })
              }
            />
            <div className="screen-area">
              {transcriptMode && focusedPaneId ? (
                <Transcript server={serverId!} pane={focusedPaneId} dim={dim} />
              ) : (
                <PaneGrid
                  server={serverId!}
                  layout={layout}
                  panes={panes}
                  dim={dim}
                  onFocusPane={focusPane}
                />
              )}
              {scrollbackPane && (
                <ScrollbackOverlay
                  server={serverId!}
                  pane={scrollbackPane}
                  title={panes[scrollbackPane]?.title ?? scrollbackPane}
                  onClose={() => setScrollbackPane(null)}
                />
              )}
              {rawMode && <div className="raw-badge">RAW — typing goes to the pane, esc exits</div>}
            </div>
            <ControlToolbar
              server={serverId!}
              pane={focusedPane}
              rawMode={rawMode}
              onToggleRaw={() => setRawMode((v) => !v)}
              transcriptMode={transcriptMode}
              onToggleTranscript={toggleTranscript}
              onRenamePane={() =>
                focusedPane &&
                setOverlay({
                  kind: "rename",
                  target: "pane",
                  id: focusedPane.pane_id,
                  current: focusedPane.title ?? "",
                })
              }
              onClosePane={() =>
                focusedPane &&
                setOverlay({
                  kind: "close",
                  target: "pane",
                  id: focusedPane.pane_id,
                  label: focusedPane.title ?? focusedPane.pane_id,
                })
              }
              onScrollback={() =>
                setScrollbackPane((cur) => (cur ? null : focusedPaneId))
              }
              notifications={notifPerm}
              onToggleNotifications={toggleNotifications}
            />
            <Composer server={serverId!} pane={focusedPane} onNotice={setNotice} />
          </>
        ) : (
          <div className="empty-screen">
            {viewServer ? `${viewServer.info.label}: ${viewServer.info.status}` : "no servers"}
          </div>
        )}
      </main>
      {overlay && serverId && (
        <OverlayHost
          server={serverId}
          overlay={overlay}
          onClose={() => setOverlay(null)}
          onError={(msg) => setNotice(msg)}
        />
      )}
      </div>
    </div>
  );
}
