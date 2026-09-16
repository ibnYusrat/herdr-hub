// Hub protocol v1 frame types (see protocol/hub-protocol.md) and herdr record
// shapes (mirrored verbatim — SPEC §5.3: mirror, don't reinvent).

export type AgentStatus = "idle" | "working" | "blocked" | "done" | "unknown";

export interface WorkspaceInfo {
  workspace_id: string;
  number: number;
  label: string;
  focused: boolean;
  pane_count: number;
  tab_count: number;
  active_tab_id: string | null;
  agent_status: AgentStatus;
  tokens?: Record<string, string>;
  worktree?: WorktreeInfo | null;
}

export interface WorktreeInfo {
  repo_key: string;
  repo_name: string;
  repo_root: string;
  checkout_path: string;
  is_linked_worktree: boolean;
}

export interface TabInfo {
  tab_id: string;
  workspace_id: string;
  number: number;
  label: string;
  focused: boolean;
  pane_count: number;
  agent_status: AgentStatus;
}

export interface PaneScrollInfo {
  offset_from_bottom: number;
  max_offset_from_bottom: number;
  viewport_rows: number;
}

export interface PaneInfo {
  pane_id: string;
  terminal_id: string;
  workspace_id: string;
  tab_id: string;
  focused: boolean;
  agent_status: AgentStatus;
  revision: number;
  label?: string | null;
  title?: string | null;
  cwd?: string | null;
  foreground_cwd?: string | null;
  agent?: string | null;
  display_agent?: string | null;
  terminal_title?: string | null;
  terminal_title_stripped?: string | null;
  scroll?: PaneScrollInfo | null;
  state_labels?: Record<string, string>;
  tokens?: Record<string, string>;
}

export interface LayoutRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface PaneLayoutSnapshot {
  workspace_id: string;
  tab_id: string;
  zoomed: boolean;
  area: LayoutRect;
  focused_pane_id: string;
  panes: { pane_id: string; focused: boolean; rect: LayoutRect }[];
  splits: { id: string; direction: "right" | "down"; ratio: number; rect: LayoutRect }[];
}

export interface AgentInfo {
  terminal_id: string;
  agent_status: AgentStatus;
  workspace_id: string;
  tab_id: string;
  pane_id: string;
  focused: boolean;
  revision: number;
  name?: string | null;
  agent?: string | null;
  cwd?: string | null;
  title?: string | null;
}

export interface FocusIds {
  workspace: string | null;
  tab: string | null;
  pane: string | null;
}

export interface ServerState {
  workspaces: Record<string, WorkspaceInfo>;
  tabs: Record<string, TabInfo>;
  panes: Record<string, PaneInfo>;
  layouts: Record<string, PaneLayoutSnapshot>;
  agents: AgentInfo[];
  focused: FocusIds;
}

// ---------------------------------------------------------------------------
// Hub frames
// ---------------------------------------------------------------------------

export interface WelcomeFrame {
  type: "welcome";
  protocol: number;
  hub: { name: string; version: string };
  servers: ServerEntryInfo[];
  heartbeat_ms: number;
}

export interface ServerEntryInfo {
  id: string;
  label: string;
  kind: string;
  status: string;
  detail?: string;
  herdr_version?: string;
  protocol?: number;
}

export interface SnapshotFrame {
  type: "snapshot";
  server: string;
  generation: number;
  state: {
    workspaces: WorkspaceInfo[];
    tabs: TabInfo[];
    panes: PaneInfo[];
    layouts: PaneLayoutSnapshot[];
    agents: AgentInfo[];
    focused: FocusIds;
  };
}

export interface EventFrame {
  type: "event";
  server: string;
  event: string; // underscore-normalized kind
  data: any;
}

export interface ScreenFrame {
  type: "screen";
  server: string;
  pane: string;
  revision: number;
  rows: number;
  cols: number;
  mode: "full" | "rows";
  lines?: string[];
  changes?: { row: number; text: string }[];
  source: "visible" | "recent";
  truncated?: boolean;
}

export interface ResponseFrame {
  type: "response";
  id?: string;
  server?: string;
  ok: boolean;
  result?: any;
  error?: { code: string; message: string };
}

export interface ServerStatusFrame {
  type: "server_status";
  server: string;
  status: string;
  detail?: string;
}

export type HubFrame =
  | WelcomeFrame
  | SnapshotFrame
  | EventFrame
  | ScreenFrame
  | ResponseFrame
  | ServerStatusFrame
  | { type: "pong" };
