// Sidebar: servers (status dot), workspaces with agent rows (status icon,
// agent name, cwd basename, worktree badge). Mirrors herdr's TUI information
// architecture (SPEC §8): the workspace list IS the session tree.

import type { WorkspaceInfo } from "../types.ts";
import type { ServerViewState } from "../store.ts";

interface Props {
  servers: Record<string, ServerViewState>;
  order: string[];
  activeServer: string | null;
  onSelectServer: (id: string) => void;
  onSelectWorkspace: (server: string, workspaceId: string) => void;
  onCreateWorkspace: (server: string) => void;
  onRenameWorkspace: (server: string, ws: WorkspaceInfo) => void;
  onCloseWorkspace: (server: string, ws: WorkspaceInfo) => void;
  onWorktree: (server: string, mode: "create" | "open") => void;
  onWorktreeRemove: (server: string, ws: WorkspaceInfo) => void;
}

const statusIcon: Record<string, string> = {
  idle: "○",
  working: "◐",
  blocked: "✖",
  done: "✔",
  unknown: "·",
};

function baseName(p: string | null | undefined): string {
  if (!p) return "";
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

export function Sidebar({
  servers,
  order,
  activeServer,
  onSelectServer,
  onSelectWorkspace,
  onCreateWorkspace,
  onRenameWorkspace,
  onCloseWorkspace,
  onWorktree,
  onWorktreeRemove,
}: Props) {
  const sorted = order
    .map((id) => servers[id])
    .filter((s): s is ServerViewState => !!s);

  return (
    <aside className="sidebar">
      {sorted.map((sv) => (
        <section key={sv.info.id} className={`server ${sv.info.status}`}>
          <header
            className={`server-head ${sv.info.id === activeServer ? "active" : ""}`}
            onClick={() => onSelectServer(sv.info.id)}
            title={`${sv.info.label} — ${sv.info.status}${sv.info.detail ? ` (${sv.info.detail})` : ""}`}
          >
            <span className={`dot st-${sv.info.status}`} />
            <span className="server-label">{sv.info.label}</span>
            <span
              className="head-actions"
              onClick={(e) => {
                e.stopPropagation();
              }}
            >
              <button title="new workspace" onClick={() => onCreateWorkspace(sv.info.id)}>＋</button>
              <button title="add worktree" onClick={() => onWorktree(sv.info.id, "create")}>⎇+</button>
              <button title="open worktree" onClick={() => onWorktree(sv.info.id, "open")}>⎇↗</button>
            </span>
          </header>
          <div className={`server-body ${sv.info.status !== "online" ? "dim" : ""}`}>
            {Object.values(sv.state.workspaces).map((w) => (
              <div key={w.workspace_id} className="workspace">
                <div
                  className="ws-row"
                  onClick={() => onSelectWorkspace(sv.info.id, w.workspace_id)}
                >
                  <span className={`dot st-${w.agent_status}`} />
                  <span className="ws-label">{w.label || String(w.number)}</span>
                  {w.worktree && (
                    <span className="badge" title={w.worktree.checkout_path}>
                      ⎇ {baseName(w.worktree.checkout_path)}
                    </span>
                  )}
                  <span className="ws-actions">
                    <button title="rename workspace" onClick={(e) => { e.stopPropagation(); onRenameWorkspace(sv.info.id, w); }}>✎</button>
                    {w.worktree ? (
                      <button title="remove worktree workspace" onClick={(e) => { e.stopPropagation(); onWorktreeRemove(sv.info.id, w); }}>×</button>
                    ) : (
                      <button title="close workspace" onClick={(e) => { e.stopPropagation(); onCloseWorkspace(sv.info.id, w); }}>×</button>
                    )}
                  </span>
                </div>
                {Object.values(sv.state.panes)
                  .filter((p) => p.workspace_id === w.workspace_id && p.agent)
                  .map((p) => (
                    <button
                      key={p.pane_id}
                      className={`agent-row st-${p.agent_status}`}
                      onClick={() => onSelectWorkspace(sv.info.id, w.workspace_id)}
                      title={p.cwd ?? ""}
                    >
                      <span className="status-icon">{statusIcon[p.agent_status] ?? "·"}</span>
                      <span className="agent-name">{p.display_agent ?? p.agent}</span>
                      <span className="agent-cwd">{baseName(p.cwd)}</span>
                    </button>
                  ))}
              </div>
            ))}
            {Object.keys(sv.state.workspaces).length === 0 && (
              <div className="empty">no workspaces</div>
            )}
          </div>
        </section>
      ))}
    </aside>
  );
}
