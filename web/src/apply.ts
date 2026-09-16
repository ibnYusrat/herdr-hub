// Pure state application: mirrors the hub's upsert rules exactly (see
// hub/src/state.rs). Unit-tested against the same fixture shapes as the Rust
// tests. Event records arrive nested under a key (`data.workspace`,
// `data.pane`, ...) for lifecycle kinds; the three dotted subscription kinds
// arrive flat. The hub normalizes dotted → underscore before forwarding.

import type {
  AgentStatus,
  PaneInfo,
  PaneLayoutSnapshot,
  ServerState,
  TabInfo,
  WorkspaceInfo,
} from "./types.ts";

export function emptyState(): ServerState {
  return {
    workspaces: {},
    tabs: {},
    panes: {},
    layouts: {},
    agents: [],
    focused: { workspace: null, tab: null, pane: null },
  };
}

function indexBy<T extends Record<string, any>>(list: T[], key: string): Record<string, T> {
  const out: Record<string, T> = {};
  for (const item of list) out[item[key]] = item;
  return out;
}

/** Install a hub snapshot frame's state (bootstrap or resync). */
export function applySnapshot(snapshot: SnapshotStateIn): ServerState {
  const workspaces = indexBy(snapshot.workspaces, "workspace_id");
  const tabs = indexBy(snapshot.tabs, "tab_id");
  // Orphan panes (workspace/tab missing from the same snapshot) are 0.8.2
  // phantoms of closed workspaces — installing them would ghost the tree
  // and poison subscriptions. The hub filters them too.
  const panes = indexBy(
    snapshot.panes.filter((p) => workspaces[p.workspace_id] && tabs[p.tab_id]),
    "pane_id",
  );
  const layouts = indexBy(
    snapshot.layouts.filter((l) => tabs[l.tab_id]),
    "tab_id",
  );
  return {
    workspaces,
    tabs,
    panes,
    layouts,
    agents: snapshot.agents,
    focused: snapshot.focused,
  };
}

export interface SnapshotStateIn {
  workspaces: WorkspaceInfo[];
  tabs: TabInfo[];
  panes: PaneInfo[];
  layouts: PaneLayoutSnapshot[];
  agents: any[];
  focused: ServerState["focused"];
}

export interface ApplyResult {
  state: ServerState;
  /** The pane set changed → the view may need re-sending. */
  paneSetChanged: boolean;
}

/**
 * Apply one lifecycle event (underscore kind) with upsert-by-id semantics.
 * Returns a new state object; untouched sub-objects are reused so React and
 * selectors can cheaply detect what changed.
 */
export function applyEvent(state: ServerState, kind: string, data: any): ApplyResult {
  const next: ServerState = {
    workspaces: state.workspaces,
    tabs: state.tabs,
    panes: state.panes,
    layouts: state.layouts,
    agents: state.agents,
    focused: state.focused,
  };
  let changed = false;
  let paneSetChanged = false;

  const str = (k: string): string | undefined => (typeof data[k] === "string" ? data[k] : undefined);
  const nested = (k: string): any | undefined =>
    data[k] !== undefined ? data[k] : undefined;

  switch (kind) {
    case "workspace_created":
    case "workspace_updated":
    case "workspace_metadata_updated": {
      const w = nested("workspace");
      if (w && w.workspace_id) {
        next.workspaces = { ...next.workspaces, [w.workspace_id]: w };
        changed = true;
      }
      break;
    }
    case "workspace_renamed": {
      const id = str("workspace_id");
      const label = str("label");
      if (id && label !== undefined && next.workspaces[id]) {
        next.workspaces = { ...next.workspaces, [id]: { ...next.workspaces[id], label } };
        changed = true;
      }
      break;
    }
    case "workspace_moved":
    case "workspace_reordered": {
      const list = data.workspaces as WorkspaceInfo[] | undefined;
      if (list && list.length) {
        next.workspaces = indexBy(list, "workspace_id");
        changed = true;
      }
      break;
    }
    case "workspace_closed": {
      const id = str("workspace_id");
      if (id && next.workspaces[id]) {
        const { [id]: _drop, ...rest } = next.workspaces;
        next.workspaces = rest;
        // herdr emits only workspace_closed — no per-tab/per-pane events —
        // so the removal cascades to everything under the workspace.
        const before = Object.keys(next.panes).length;
        ({ panes: next.panes, tabs: next.tabs, layouts: next.layouts } =
          cascadeWorkspace(next.panes, next.tabs, next.layouts, id));
        if (Object.keys(next.panes).length !== before) paneSetChanged = true;
        changed = true;
      }
      break;
    }
    case "workspace_focused": {
      const id = str("workspace_id");
      if (id) {
        next.focused = { ...next.focused, workspace: id };
        next.workspaces = mapValues(next.workspaces, (w) => ({ ...w, focused: w.workspace_id === id }));
        changed = true;
      }
      break;
    }
    case "tab_created": {
      const t = nested("tab");
      if (t && t.tab_id) {
        next.tabs = { ...next.tabs, [t.tab_id]: t };
        changed = true;
      }
      break;
    }
    case "tab_renamed": {
      const id = str("tab_id");
      const label = str("label");
      if (id && label !== undefined && next.tabs[id]) {
        next.tabs = { ...next.tabs, [id]: { ...next.tabs[id], label } };
        changed = true;
      }
      break;
    }
    case "tab_moved": {
      const list = data.tabs as TabInfo[] | undefined;
      if (list && list.length) {
        next.tabs = indexBy(list, "tab_id");
        changed = true;
      }
      break;
    }
    case "tab_closed": {
      const id = str("tab_id");
      if (id && next.tabs[id]) {
        const { [id]: _drop, ...restTabs } = next.tabs;
        next.tabs = restTabs;
        const { [id]: _dropL, ...restLayouts } = next.layouts;
        next.layouts = restLayouts;
        // The tab's panes cascade (herdr emits no per-pane events here).
        const panes: typeof next.panes = {};
        for (const [pid, p] of Object.entries(next.panes)) {
          if (p.tab_id !== id) panes[pid] = p;
          else paneSetChanged = true;
        }
        next.panes = panes;
        changed = true;
      }
      break;
    }
    case "tab_focused": {
      const id = str("tab_id");
      const ws = str("workspace_id");
      if (id && ws) {
        next.focused = { ...next.focused, tab: id, workspace: ws };
        next.tabs = mapValues(next.tabs, (t) => ({ ...t, focused: t.tab_id === id }));
        const w = next.workspaces[ws];
        if (w) next.workspaces = { ...next.workspaces, [ws]: { ...w, active_tab_id: id } };
        changed = true;
      }
      break;
    }
    case "pane_created": {
      const p = nested("pane");
      // A pane whose workspace/tab is absent is a phantom of a closed
      // workspace (0.8.2 flicker) — herdr emits real creates strictly
      // workspace → tab → pane, so this drops nothing legitimate.
      if (p && p.pane_id && next.workspaces[p.workspace_id] && next.tabs[p.tab_id]) {
        next.panes = { ...next.panes, [p.pane_id]: p };
        if (!state.panes[p.pane_id]) paneSetChanged = true;
        changed = true;
      }
      break;
    }
    case "pane_updated": {
      // Update-only: herdr keeps pushing pane_updated for panes of closed
      // workspaces after forgetting them (verified live on 0.8.2);
      // resurrecting zombies would ghost them in the tree forever.
      const p = nested("pane");
      if (p && p.pane_id && next.panes[p.pane_id]) {
        next.panes = { ...next.panes, [p.pane_id]: p };
        changed = true;
      }
      break;
    }
    case "pane_focused": {
      const id = str("pane_id");
      if (id) {
        next.focused = { ...next.focused, pane: id };
        next.panes = mapValues(next.panes, (p) => ({ ...p, focused: p.pane_id === id }));
        changed = true;
      }
      break;
    }
    case "pane_moved": {
      const prevId = str("previous_pane_id");
      const panes = { ...next.panes };
      if (prevId && panes[prevId]) {
        delete panes[prevId];
        paneSetChanged = true;
      }
      const cw = nested("created_workspace");
      if (cw && cw.workspace_id) next.workspaces = { ...next.workspaces, [cw.workspace_id]: cw };
      const ct = nested("created_tab");
      if (ct && ct.tab_id) next.tabs = { ...next.tabs, [ct.tab_id]: ct };
      const p = nested("pane");
      if (p && p.pane_id) panes[p.pane_id] = p;
      next.panes = panes;
      changed = true;
      break;
    }
    case "pane_closed":
    case "pane_exited": {
      const id = str("pane_id");
      if (id && next.panes[id]) {
        const { [id]: _drop, ...rest } = next.panes;
        next.panes = rest;
        changed = true;
        paneSetChanged = true;
      }
      break;
    }
    case "pane_agent_detected": {
      const id = str("pane_id");
      const agent = str("agent");
      if (id && agent !== undefined && next.panes[id]) {
        next.panes = { ...next.panes, [id]: { ...next.panes[id], agent } };
        changed = true;
      }
      break;
    }
    case "pane_agent_status_changed": {
      const id = str("pane_id");
      if (id && next.panes[id]) {
        let pane = next.panes[id];
        if (typeof data.agent_status === "string") pane = { ...pane, agent_status: data.agent_status };
        for (const key of ["agent", "display_agent", "title"] as const) {
          const v = str(key);
          if (v !== undefined) pane = { ...pane, [key]: v };
        }
        if (data.state_labels) pane = { ...pane, state_labels: data.state_labels };
        next.panes = { ...next.panes, [id]: pane };
        const tabId = pane.tab_id;
        const tab = next.tabs[tabId];
        if (tab && typeof data.agent_status === "string") {
          next.tabs = { ...next.tabs, [tabId]: { ...tab, agent_status: data.agent_status } };
        }
        changed = true;
      }
      break;
    }
    case "pane_scroll_changed": {
      const id = str("pane_id");
      if (id && data.scroll && next.panes[id]) {
        next.panes = { ...next.panes, [id]: { ...next.panes[id], scroll: data.scroll } };
        changed = true;
      }
      break;
    }
    case "layout_updated": {
      const l = nested("layout");
      if (l && l.tab_id) {
        next.layouts = { ...next.layouts, [l.tab_id]: l };
        changed = true;
      }
      break;
    }
    case "worktree_created":
    case "worktree_opened": {
      const w = nested("workspace");
      if (w && w.workspace_id) {
        next.workspaces = { ...next.workspaces, [w.workspace_id]: w };
        changed = true;
      }
      break;
    }
    case "worktree_removed": {
      const id = str("workspace_id");
      if (id && next.workspaces[id]) {
        const { [id]: _drop, ...rest } = next.workspaces;
        next.workspaces = rest;
        const before = Object.keys(next.panes).length;
        ({ panes: next.panes, tabs: next.tabs, layouts: next.layouts } =
          cascadeWorkspace(next.panes, next.tabs, next.layouts, id));
        if (Object.keys(next.panes).length !== before) paneSetChanged = true;
        changed = true;
      }
      break;
    }
    default:
      // Unknown kind: nothing to apply locally (the hub forwards new kinds).
      break;
  }

  return { state: changed || paneSetChanged ? next : state, paneSetChanged };
}

/** Drop a workspace's tabs, panes, and layouts (closure cascade). */
function cascadeWorkspace(
  panes: ServerState["panes"],
  tabs: ServerState["tabs"],
  layouts: ServerState["layouts"],
  ws: string,
): Pick<ServerState, "panes" | "tabs" | "layouts"> {
  const nextTabs = filterEntries(tabs, (t) => t.workspace_id !== ws);
  const nextLayouts = filterEntries(layouts, (_l, tid) => nextTabs[tid] !== undefined);
  const nextPanes = filterEntries(panes, (p) => p.workspace_id !== ws);
  return { panes: nextPanes, tabs: nextTabs, layouts: nextLayouts };
}

function filterEntries<T>(obj: Record<string, T>, keep: (v: T, key: string) => boolean): Record<string, T> {
  const out: Record<string, T> = {};
  for (const [k, v] of Object.entries(obj)) if (keep(v, k)) out[k] = v;
  return out;
}

function mapValues<T>(obj: Record<string, T>, fn: (v: T) => T): Record<string, T> {
  const out: Record<string, T> = {};
  for (const [k, v] of Object.entries(obj)) out[k] = fn(v);
  return out;
}

export function agentStatusRank(s: AgentStatus): number {
  return { blocked: 0, working: 1, done: 2, idle: 3, unknown: 4 }[s] ?? 4;
}
