// App store (zustand): server list + per-server canonical state, connection
// status, and the active selection (which server/workspace/tab is viewed).
// Screen content lives in screens.ts, not here.

import { create } from "zustand";
import { applyEvent, applySnapshot, emptyState } from "./apply.ts";
import type {
  ServerEntryInfo,
  ServerState,
  SnapshotFrame,
} from "./types.ts";

export interface ServerViewState {
  info: ServerEntryInfo;
  state: ServerState;
  /** Bumped on every snapshot install (bootstrap or resync). */
  generation: number;
}

interface HubStore {
  connected: boolean;
  servers: Record<string, ServerViewState>;
  order: string[];
  activeServer: string | null;
  activeTab: string | null;

  setConnected: (v: boolean) => void;
  welcome: (servers: ServerEntryInfo[]) => void;
  serverStatus: (server: string, status: string, detail?: string) => void;
  snapshot: (frame: SnapshotFrame) => void;
  event: (server: string, kind: string, data: any) => void;
  selectTab: (server: string, tab: string | null) => void;
  selectServer: (server: string) => void;

  /** Server to show in the main area: active, else first online, else first. */
  viewServer: () => ServerViewState | null;
}

export const useStore = create<HubStore>((set, get) => ({
  connected: false,
  servers: {},
  order: [],
  activeServer: null,
  activeTab: null,

  setConnected: (v) => set({ connected: v }),

  welcome: (infos) =>
    set((s) => {
      const servers: Record<string, ServerViewState> = {};
      const order: string[] = [];
      for (const info of infos) {
        order.push(info.id);
        servers[info.id] = {
          info,
          state: s.servers[info.id]?.state ?? emptyState(),
          generation: s.servers[info.id]?.generation ?? 0,
        };
      }
      const activeServer =
        s.activeServer && order.includes(s.activeServer) ? s.activeServer : (order[0] ?? null);
      return { servers, order, activeServer };
    }),

  serverStatus: (server, status, detail) =>
    set((s) => {
      const existing = s.servers[server];
      if (!existing) return {};
      return {
        servers: {
          ...s.servers,
          [server]: {
            ...existing,
            info: { ...existing.info, status, detail },
          },
        },
      };
    }),

  snapshot: (frame) =>
    set((s) => {
      const existing = s.servers[frame.server];
      if (!existing) return {};
      return {
        servers: {
          ...s.servers,
          [frame.server]: {
            ...existing,
            state: applySnapshot(frame.state),
            generation: frame.generation,
          },
        },
      };
    }),

  event: (server, kind, data) =>
    set((s) => {
      const existing = s.servers[server];
      if (!existing) return {};
      const result = applyEvent(existing.state, kind, data);
      const patch: any = {
        servers: { ...s.servers, [server]: { ...existing, state: result.state } },
      };
      if (kind === "tab_closed" && s.activeTab === data?.tab_id) {
        patch.activeTab = null;
      }
      return patch;
    }),

  selectTab: (server, tab) =>
    set(() => ({
      activeServer: server,
      activeTab: tab,
    })),

  selectServer: (server) =>
    set(() => ({
      activeServer: server,
      activeTab: null,
    })),

  viewServer: () => {
    const s = get();
    const active = s.activeServer ? s.servers[s.activeServer] : null;
    if (active && active.info.status === "online") return active;
    for (const id of s.order) {
      const sv = s.servers[id];
      if (sv && sv.info.status === "online") return sv;
    }
    return active ?? (s.order.length ? s.servers[s.order[0]] : null);
  },
}));

/** The active tab's layout, resolved from the active server's state. */
export function activeLayout(server: ServerViewState | null, activeTab: string | null) {
  if (!server || !activeTab) return null;
  return server.state.layouts[activeTab] ?? null;
}
