// Modal overlays: rename (workspace/tab/pane), confirm-close (with the
// worktree-group second confirm herdr requires), new workspace/tab, and
// worktree create/open/remove. All send allowlisted hub actions verbatim.

import { useState } from "react";
import { act } from "../actions.ts";

export type Overlay =
  | { kind: "rename"; target: "workspace" | "tab" | "pane"; id: string; current: string }
  | { kind: "close"; target: "workspace" | "tab" | "pane"; id: string; label: string; workspaceId?: string }
  | { kind: "new-workspace" }
  | { kind: "new-tab"; workspaceId: string }
  | { kind: "worktree"; mode: "create" | "open" }
  | { kind: "worktree-remove"; workspaceId: string; label: string };

interface Props {
  server: string;
  overlay: Overlay;
  onClose: () => void;
  onError: (msg: string) => void;
}

export function OverlayHost({ server, overlay, onClose, onError }: Props) {
  return (
    <div className="overlay-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="overlay">
        {overlay.kind === "rename" && (
          <RenameForm server={server} overlay={overlay} onClose={onClose} />
        )}
        {overlay.kind === "close" && (
          <CloseForm server={server} overlay={overlay} onClose={onClose} onError={onError} />
        )}
        {overlay.kind === "new-workspace" && (
          <NewForm server={server} what="workspace" onClose={onClose} />
        )}
        {overlay.kind === "new-tab" && (
          <NewForm server={server} what="tab" workspaceId={overlay.workspaceId} onClose={onClose} />
        )}
        {overlay.kind === "worktree" && (
          <WorktreeForm server={server} mode={overlay.mode} onClose={onClose} onError={onError} />
        )}
        {overlay.kind === "worktree-remove" && (
          <Confirm
            title={`remove worktree workspace ${overlay.label}?`}
            body="worktree.remove — the workspace closes; the files stay on disk unless you remove them yourself."
            confirmLabel="remove"
            onConfirm={async () => {
              const r = await act(server, "worktree.remove", { workspace_id: overlay.workspaceId });
              if (!r.ok) onError(r.error?.message ?? "worktree.remove failed");
            }}
            onClose={onClose}
          />
        )}
      </div>
    </div>
  );
}

function RenameForm({ server, overlay, onClose }: { server: string; overlay: Extract<Overlay, { kind: "rename" }>; onClose: () => void }) {
  const [label, setLabel] = useState(overlay.current);
  const action =
    overlay.target === "workspace" ? "workspace.rename" : overlay.target === "tab" ? "tab.rename" : "pane.rename";
  const idKey = overlay.target === "workspace" ? "workspace_id" : overlay.target === "tab" ? "tab_id" : "pane_id";
  return (
    <form
      onSubmit={async (e) => {
        e.preventDefault();
        await act(server, action, { [idKey]: overlay.id, label });
        onClose();
      }}
    >
      <h3>rename {overlay.target}</h3>
      <input autoFocus value={label} onChange={(e) => setLabel(e.target.value)} />
      <div className="overlay-actions">
        <button type="button" onClick={onClose}>cancel</button>
        <button type="submit" disabled={!label.trim()}>rename</button>
      </div>
    </form>
  );
}

function CloseForm({ server, overlay, onClose, onError }: { server: string; overlay: Extract<Overlay, { kind: "close" }>; onClose: () => void; onError: (m: string) => void }) {
  const [groupRequired, setGroupRequired] = useState(false);
  async function close(closeGroup: boolean) {
    if (overlay.target === "workspace") {
      const r = await act(server, "workspace.close", {
        workspace_id: overlay.id,
        close_group: closeGroup,
      });
      if (!r.ok && r.error?.code === "workspace_group_close_required") {
        setGroupRequired(true);
        return;
      }
      if (!r.ok) onError(r.error?.message ?? "workspace.close failed");
    } else if (overlay.target === "tab") {
      await act(server, "tab.close", { tab_id: overlay.id });
    } else {
      await act(server, "pane.close", { pane_id: overlay.id });
    }
    onClose();
  }
  return (
    <div>
      <h3>
        {groupRequired ? "close whole worktree group?" : `close ${overlay.target} ${overlay.label}?`}
      </h3>
      {groupRequired && (
        <p>This workspace has linked worktree workspaces; herdr requires closing them together.</p>
      )}
      <div className="overlay-actions">
        <button type="button" onClick={onClose}>cancel</button>
        <button type="button" className="danger" onClick={() => void close(groupRequired)}>
          {groupRequired ? "close group" : "close"}
        </button>
      </div>
    </div>
  );
}

function NewForm({ server, what, workspaceId, onClose }: { server: string; what: "workspace" | "tab"; workspaceId?: string; onClose: () => void }) {
  const [label, setLabel] = useState("");
  return (
    <form
      onSubmit={async (e) => {
        e.preventDefault();
        if (what === "workspace") {
          await act(server, "workspace.create", { label: label || null, focus: true });
        } else {
          await act(server, "tab.create", { workspace_id: workspaceId ?? null, label: label || null, focus: true });
        }
        onClose();
      }}
    >
      <h3>new {what}</h3>
      <input autoFocus placeholder={`label (optional)`} value={label} onChange={(e) => setLabel(e.target.value)} />
      <div className="overlay-actions">
        <button type="button" onClick={onClose}>cancel</button>
        <button type="submit">create</button>
      </div>
    </form>
  );
}

function WorktreeForm({ server, mode, onClose, onError }: { server: string; mode: "create" | "open"; onClose: () => void; onError: (m: string) => void }) {
  const [path, setPath] = useState("");
  const [branch, setBranch] = useState("");
  return (
    <form
      onSubmit={async (e) => {
        e.preventDefault();
        const params: any = { path: path || null, focus: true };
        if (branch.trim()) params.branch = branch.trim();
        const r = await act(server, mode === "create" ? "worktree.create" : "worktree.open", params);
        if (r.ok) onClose();
        else onError(r.error?.message ?? `worktree.${mode} failed`);
      }}
    >
      <h3>{mode} worktree</h3>
      <input autoFocus placeholder="repository path" value={path} onChange={(e) => setPath(e.target.value)} />
      <input placeholder="branch (optional)" value={branch} onChange={(e) => setBranch(e.target.value)} />
      <div className="overlay-actions">
        <button type="button" onClick={onClose}>cancel</button>
        <button type="submit" disabled={!path.trim()}>{mode}</button>
      </div>
    </form>
  );
}

function Confirm({ title, body, confirmLabel, onConfirm, onClose }: { title: string; body: string; confirmLabel: string; onConfirm: () => Promise<void>; onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  return (
    <div>
      <h3>{title}</h3>
      <p>{body}</p>
      <div className="overlay-actions">
        <button type="button" onClick={onClose}>cancel</button>
        <button
          type="button"
          className="danger"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await onConfirm();
            } finally {
              onClose();
            }
          }}
        >
          {confirmLabel}
        </button>
      </div>
    </div>
  );
}
