//! Canonical per-server session state, built from `session.snapshot` and kept
//! current by applying lifecycle events. All mutation is pure-function style
//! (`apply_event`, `install_snapshot`) so it is unit-testable without tokio;
//! the server actor serializes access.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde::Serialize;

use crate::wire::types::*;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase", tag = "status")]
pub enum ServerStatus {
    Connecting,
    Reconnecting,
    Online { version: String, protocol: u32 },
    Offline,
}

impl Default for ServerStatus {
    fn default() -> Self {
        ServerStatus::Connecting
    }
}

impl ServerStatus {
    pub fn is_online(&self) -> bool {
        matches!(self, ServerStatus::Online { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            ServerStatus::Connecting => "connecting",
            ServerStatus::Reconnecting => "reconnecting",
            ServerStatus::Online { .. } => "online",
            ServerStatus::Offline => "offline",
        }
    }

    pub fn version(&self) -> Option<&str> {
        match self {
            ServerStatus::Online { version, .. } => Some(version),
            _ => None,
        }
    }

    pub fn protocol(&self) -> Option<u32> {
        match self {
            ServerStatus::Online { protocol, .. } => Some(*protocol),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FocusIds {
    pub workspace: Option<String>,
    pub tab: Option<String>,
    pub pane: Option<String>,
}

/// Full canonical state for one herdr server. Record shapes are herdr's
/// (SPEC §5.3: mirror, don't reinvent) so they serialize straight into hub
/// `snapshot` messages.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ServerState {
    pub status: ServerStatus,
    /// Bumped every time a snapshot is installed (bootstrap or resync).
    pub generation: u64,
    pub workspaces: IndexMap<String, WorkspaceInfo>,
    pub tabs: IndexMap<String, TabInfo>,
    pub panes: IndexMap<String, PaneInfo>,
    pub layouts: HashMap<String, PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    pub focused: FocusIds,
}

impl ServerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, status: ServerStatus) {
        self.status = status;
    }

    /// Replace state from a fresh `session.snapshot` (bootstrap or resync).
    pub fn install_snapshot(&mut self, snap: &SessionSnapshot) {
        self.workspaces = indexmap_from(snap.workspaces.iter(), |w| w.workspace_id.clone());
        self.tabs = indexmap_from(snap.tabs.iter(), |t| t.tab_id.clone());
        // 0.8.2 flicker: snapshots intermittently still list panes of closed
        // workspaces while omitting the workspace itself. Installing such an
        // orphan poisons every per-pane subscription with pane_not_found, so
        // only take panes whose workspace AND tab are in this snapshot.
        let tab_ids: HashSet<&str> = self.tabs.keys().map(|s| s.as_str()).collect();
        let ws_ids: HashSet<&str> = self.workspaces.keys().map(|s| s.as_str()).collect();
        self.panes = indexmap_from(
            snap.panes
                .iter()
                .filter(|p| ws_ids.contains(p.workspace_id.as_str())
                    && tab_ids.contains(p.tab_id.as_str())),
            |p| p.pane_id.clone(),
        );
        self.layouts = snap
            .layouts
            .iter()
            .filter(|l| tab_ids.contains(l.tab_id.as_str()))
            .map(|l| (l.tab_id.clone(), l.clone()))
            .collect();
        self.agents = snap.agents.clone();
        self.focused = FocusIds {
            workspace: snap.focused_workspace_id.clone(),
            tab: snap.focused_tab_id.clone(),
            pane: snap.focused_pane_id.clone(),
        };
        self.generation += 1;
    }

    pub fn workspace(&self, id: &str) -> Option<&WorkspaceInfo> {
        self.workspaces.get(id)
    }

    /// Whether the canonical state already equals the snapshot at the level
    /// the snapshot reconciles (record sets, focus, pane revisions) — used to
    /// skip no-op reconciliation broadcasts.
    pub fn matches_snapshot(&self, snap: &SessionSnapshot) -> bool {
        fn sorted(mut v: Vec<String>) -> Vec<String> {
            v.sort();
            v
        }
        let ws_ids = |s: &Self| sorted(s.workspaces.keys().cloned().collect());
        let tab_ids = |s: &Self| sorted(s.tabs.keys().cloned().collect());
        let pane_ids = |s: &Self| sorted(s.panes.keys().cloned().collect());
        if ws_ids(self) != sorted(snap.workspaces.iter().map(|w| w.workspace_id.clone()).collect())
            || tab_ids(self) != sorted(snap.tabs.iter().map(|t| t.tab_id.clone()).collect())
            || pane_ids(self) != sorted(snap.panes.iter().map(|p| p.pane_id.clone()).collect())
        {
            return false;
        }
        if self.focused.workspace != snap.focused_workspace_id
            || self.focused.tab != snap.focused_tab_id
            || self.focused.pane != snap.focused_pane_id
        {
            return false;
        }
        // Content drift (missed updates): pane revisions must line up.
        snap.panes.iter().all(|sp| {
            self.panes
                .get(&sp.pane_id)
                .map(|p| p.revision == sp.revision)
                .unwrap_or(false)
        })
    }

    pub fn tab(&self, id: &str) -> Option<&TabInfo> {
        self.tabs.get(id)
    }

    pub fn pane(&self, id: &str) -> Option<&PaneInfo> {
        self.panes.get(id)
    }

    /// Tabs of a workspace in order.
    pub fn tabs_of(&self, workspace_id: &str) -> Vec<&TabInfo> {
        self.tabs
            .values()
            .filter(|t| t.workspace_id == workspace_id)
            .collect()
    }

    /// Panes of a tab in layout order.
    pub fn panes_of(&self, tab_id: &str) -> Vec<&PaneInfo> {
        if let Some(layout) = self.layouts.get(tab_id) {
            let panes: Vec<&PaneInfo> = layout
                .panes
                .iter()
                .filter_map(|e| self.panes.get(&e.pane_id))
                .collect();
            if !panes.is_empty() {
                return panes;
            }
        }
        self.panes
            .values()
            .filter(|p| p.tab_id == tab_id)
            .collect()
    }
}

fn indexmap_from<'a, T: 'a, K: std::hash::Hash + Eq + Clone>(
    items: impl Iterator<Item = &'a T>,
    key: impl Fn(&T) -> K,
) -> IndexMap<K, T>
where
    T: Clone,
{
    items.map(|i| (key(i), i.clone())).collect()
}

/// What applying an event told the caller.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    pub changed: bool,
    /// The pane set changed → recompute desired subscriptions.
    pub pane_set_changed: bool,
    /// A viewed pane's revision bumped → the poller may poll it immediately.
    pub revision_hints: Vec<(String, u64)>,
}

/// Apply one lifecycle event with upsert-by-id semantics:
/// created/updated/layout events carry full records → upsert; closed/exited →
/// remove; moved/reordered carry whole ordered id lists → replace order;
/// focused → last-write-wins. Stale buffered events replayed against a newer
/// snapshot are therefore harmless no-ops (SPEC §3.4 bootstrap recipe).
pub fn apply_event(state: &mut ServerState, ev: &EventEnvelope) -> Applied {
    let kind = ev.kind();
    let data = &ev.data;
    let mut out = Applied::default();

    match kind.as_str() {
        "workspace_created" | "workspace_updated" | "workspace_metadata_updated" => {
            if let Some(w) = nested_record::<WorkspaceInfo>(data, "workspace") {
                upsert(&mut state.workspaces, w.workspace_id.clone(), w);
                out.changed = true;
            }
        }
        "workspace_renamed" => {
            if let (Some(id), Some(label)) = (
                str_field(data, "workspace_id"),
                str_field(data, "label"),
            ) {
                if let Some(w) = state.workspaces.get_mut(&id) {
                    w.label = label;
                    out.changed = true;
                }
            }
        }
        "workspace_moved" | "workspace_reordered" => {
            let list: Vec<WorkspaceInfo> = data
                .get("workspaces")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if !list.is_empty() {
                state.workspaces = indexmap_from(list.iter(), |w| w.workspace_id.clone());
                out.changed = true;
            }
        }
        "workspace_closed" => {
            if let Some(id) = str_field(data, "workspace_id") {
                let (removed, panes_removed) = remove_workspace_cascade(state, &id);
                if removed {
                    out.changed = true;
                }
                if panes_removed {
                    out.pane_set_changed = true;
                }
            }
        }
        "workspace_focused" => {
            if let Some(id) = str_field(data, "workspace_id") {
                state.focused.workspace = Some(id.clone());
                for (wid, w) in state.workspaces.iter_mut() {
                    w.focused = *wid == id;
                }
                out.changed = true;
            }
        }
        "tab_created" => {
            if let Some(t) = nested_record::<TabInfo>(data, "tab") {
                upsert(&mut state.tabs, t.tab_id.clone(), t);
                out.changed = true;
            }
        }
        "tab_renamed" => {
            if let (Some(id), Some(label)) = (str_field(data, "tab_id"), str_field(data, "label")) {
                if let Some(t) = state.tabs.get_mut(&id) {
                    t.label = label;
                    out.changed = true;
                }
            }
        }
        "tab_moved" => {
            let list: Vec<TabInfo> = data
                .get("tabs")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if !list.is_empty() {
                state.tabs = indexmap_from(list.iter(), |t| t.tab_id.clone());
                out.changed = true;
            }
        }
        "tab_closed" => {
            if let Some(id) = str_field(data, "tab_id") {
                let ws = state.tabs.get(&id).map(|t| t.workspace_id.clone());
                let removed = state.tabs.shift_remove(&id).is_some();
                state.layouts.remove(&id);
                // herdr emits only `tab_closed` — no per-pane events — so the
                // tab's panes cascade here.
                let panes: Vec<String> = state
                    .panes
                    .iter()
                    .filter(|(_, p)| p.tab_id == id)
                    .map(|(pid, _)| pid.clone())
                    .collect();
                for pid in &panes {
                    state.panes.shift_remove(pid);
                    state.agents.retain(|a| &a.pane_id != pid);
                }
                if let Some(ws) = ws {
                    // Clean up workspace bookkeeping when its last tab dies.
                    let next_active = state
                        .tabs_of(&ws)
                        .first()
                        .map(|t| t.tab_id.clone());
                    if let Some(w) = state.workspaces.get_mut(&ws) {
                        w.tab_count = w.tab_count.saturating_sub(1);
                        if w.active_tab_id.as_deref() == Some(id.as_str()) {
                            w.active_tab_id = next_active;
                        }
                    }
                }
                if removed {
                    out.changed = true;
                }
                if !panes.is_empty() {
                    out.pane_set_changed = true;
                }
            }
        }
        "tab_focused" => {
            if let (Some(id), Some(ws)) = (str_field(data, "tab_id"), str_field(data, "workspace_id")) {
                state.focused.tab = Some(id.clone());
                state.focused.workspace = Some(ws.clone());
                for (tid, t) in state.tabs.iter_mut() {
                    t.focused = *tid == id;
                }
                if let Some(w) = state.workspaces.get_mut(&ws) {
                    w.active_tab_id = Some(id.clone());
                }
                out.changed = true;
            }
        }
        "pane_created" => {
            if let Some(p) = nested_record::<PaneInfo>(data, "pane") {
                // herdr emits workspace_created → tab_created → pane_created
                // in that order (app/creation.rs), so a pane whose workspace
                // or tab is missing from state is a phantom of a closed
                // workspace (0.8.2 keeps probing those). Creating it would
                // poison per-pane subscriptions; drop it instead.
                if state.workspaces.contains_key(&p.workspace_id)
                    && state.tabs.contains_key(&p.tab_id)
                {
                    let pid = p.pane_id.clone();
                    let existed = state.panes.contains_key(&pid);
                    upsert(&mut state.panes, pid, p);
                    if !existed {
                        out.pane_set_changed = true;
                    }
                    out.changed = true;
                }
            }
        }
        "pane_updated" => {
            // Update-only. herdr keeps probing and pushing pane_updated for
            // panes of closed workspaces long after its snapshot and
            // subscription validator forgot them (verified live on 0.8.2);
            // resurrecting such zombies poisons every per-pane subscription.
            if let Some(p) = nested_record::<PaneInfo>(data, "pane") {
                let revision = p.revision;
                let pid = p.pane_id.clone();
                if state.panes.contains_key(&pid) {
                    upsert(&mut state.panes, pid.clone(), p);
                    out.revision_hints.push((pid, revision));
                    out.changed = true;
                }
            }
        }
        "pane_focused" => {
            if let Some(id) = str_field(data, "pane_id") {
                state.focused.pane = Some(id.clone());
                for (pid, p) in state.panes.iter_mut() {
                    p.focused = *pid == id;
                }
                out.changed = true;
            }
        }
        "pane_moved" => {
            // Cross-tab moves reassign the public pane id (herdr semantics).
            // The event may also carry workspace/tab records created for the
            // pane's new home.
            if let Some(prev) = str_field(data, "previous_pane_id") {
                state.panes.shift_remove(&prev);
            }
            if let Some(w) = nested_record::<WorkspaceInfo>(data, "created_workspace") {
                upsert(&mut state.workspaces, w.workspace_id.clone(), w);
            }
            if let Some(t) = nested_record::<TabInfo>(data, "created_tab") {
                upsert(&mut state.tabs, t.tab_id.clone(), t);
            }
            if let Some(p) = nested_record::<PaneInfo>(data, "pane") {
                upsert(&mut state.panes, p.pane_id.clone(), p);
            }
            out.pane_set_changed = true;
            out.changed = true;
        }
        "pane_closed" | "pane_exited" => {
            if let Some(id) = str_field(data, "pane_id") {
                let ws_tab = state
                    .panes
                    .get(&id)
                    .map(|p| (p.workspace_id.clone(), p.tab_id.clone()));
                let removed = state.panes.shift_remove(&id).is_some();
                if let Some((ws, tab)) = ws_tab {
                    if let Some(w) = state.workspaces.get_mut(&ws) {
                        w.pane_count = w.pane_count.saturating_sub(1);
                    }
                    if let Some(t) = state.tabs.get_mut(&tab) {
                        t.pane_count = t.pane_count.saturating_sub(1);
                    }
                    if let Some(l) = state.layouts.get_mut(&tab) {
                        l.panes.retain(|e| e.pane_id != id);
                    }
                    state.agents.retain(|a| a.pane_id != id);
                }
                if removed {
                    out.pane_set_changed = true;
                    out.changed = true;
                }
            }
        }
        "pane_agent_detected" => {
            if let Some(id) = str_field(data, "pane_id") {
                if let Some(p) = state.panes.get_mut(&id) {
                    if let Some(agent) = str_field(data, "agent") {
                        p.agent = Some(agent);
                    }
                    out.changed = true;
                }
            }
        }
        "pane_agent_status_changed" => {
            // Data shape (dotted subscription events and emitted events both
            // carry these keys; parse leniently).
            if let Some(id) = str_field(data, "pane_id") {
                let mut touched = false;
                if let Some(status) = data.get("agent_status").and_then(|v| v.as_str()) {
                    let status = agent_status_from_str(status);
                    if let Some(p) = state.panes.get_mut(&id) {
                        p.agent_status = status;
                        touched = true;
                    }
                    for a in state.agents.iter_mut() {
                        if a.pane_id == id {
                            a.agent_status = status;
                        }
                    }
                    if let Some(tab_id) = state.panes.get(&id).map(|p| p.tab_id.clone()) {
                        if let Some(t) = state.tabs.get_mut(&tab_id) {
                            t.agent_status = status;
                        }
                    }
                }
                for key in ["agent", "display_agent", "title"] {
                    if let Some(v) = str_field(data, key) {
                        if let Some(p) = state.panes.get_mut(&id) {
                            match key {
                                "agent" => p.agent = Some(v),
                                "display_agent" => p.display_agent = Some(v),
                                _ => p.title = Some(v),
                            }
                            touched = true;
                        }
                    }
                }
                if let Some(labels) = data.get("state_labels") {
                    if let Ok(map) =
                        serde_json::from_value::<std::collections::BTreeMap<String, String>>(
                            labels.clone(),
                        )
                    {
                        if let Some(p) = state.panes.get_mut(&id) {
                            p.state_labels = map;
                            touched = true;
                        }
                    }
                }
                out.changed = touched;
            }
        }
        "pane_scroll_changed" => {
            if let (Some(id), Some(scroll)) = (
                str_field(data, "pane_id"),
                data.get("scroll").cloned(),
            ) {
                if let Ok(scroll) = serde_json::from_value::<PaneScrollInfo>(scroll) {
                    if let Some(p) = state.panes.get_mut(&id) {
                        p.scroll = Some(scroll);
                        out.changed = true;
                    }
                }
            }
        }
        "pane_output_changed" => {
            if let Some(id) = str_field(data, "pane_id") {
                if let Some(rev) = data.get("revision").and_then(|v| v.as_u64()) {
                    out.revision_hints.push((id, rev));
                }
            }
        }
        "layout_updated" => {
            if let Some(l) = nested_record::<PaneLayoutSnapshot>(data, "layout") {
                state.layouts.insert(l.tab_id.clone(), l);
                out.changed = true;
            }
        }
        "worktree_created" | "worktree_opened" => {
            if let Some(w) = nested_record::<WorkspaceInfo>(data, "workspace") {
                upsert(&mut state.workspaces, w.workspace_id.clone(), w);
                out.changed = true;
            }
        }
        "worktree_removed" => {
            if let Some(id) = str_field(data, "workspace_id") {
                let (removed, panes_removed) = remove_workspace_cascade(state, &id);
                if removed {
                    out.changed = true;
                }
                if panes_removed {
                    out.pane_set_changed = true;
                }
            }
        }
        _ => {
            // Unknown kind: forward to clients anyway (they may know it);
            // nothing to apply locally.
        }
    }

    out
}

fn upsert<K: std::hash::Hash + Eq + Clone, V>(map: &mut IndexMap<K, V>, key: K, value: V) {
    map.insert(key, value);
}

/// Parse a full record nested under `key` in the event data (schema shape:
/// `{"type": ..., "workspace": {...}}`). A missing key yields None; parsing
/// is lenient like everywhere else in this crate.
fn nested_record<T: serde::de::DeserializeOwned>(
    data: &serde_json::Value,
    key: &str,
) -> Option<T> {
    let inner = data.get(key)?;
    serde_json::from_value::<T>(inner.clone()).ok()
}

fn str_field(data: &serde_json::Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// herdr emits only `workspace_closed` (no per-tab/per-pane events), so
/// removing a workspace cascades to its tabs, panes, layouts, and agents.
/// Returns (workspace_removed, any_panes_removed).
fn remove_workspace_cascade(state: &mut ServerState, ws: &str) -> (bool, bool) {
    let tabs: Vec<String> = state
        .tabs
        .iter()
        .filter(|(_, t)| t.workspace_id == ws)
        .map(|(id, _)| id.clone())
        .collect();
    let panes: Vec<String> = state
        .panes
        .iter()
        .filter(|(_, p)| p.workspace_id == ws)
        .map(|(id, _)| id.clone())
        .collect();
    let removed = state.workspaces.shift_remove(ws).is_some();
    for tab in &tabs {
        state.tabs.shift_remove(tab);
        state.layouts.remove(tab);
    }
    for pane in &panes {
        state.panes.shift_remove(pane);
        state.agents.retain(|a| &a.pane_id != pane);
    }
    (removed, !panes.is_empty())
}

pub fn agent_status_from_str(s: &str) -> AgentStatus {
    match s {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::Blocked,
        "done" => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

/// Which panes at least one client currently sees (and how), aggregated from
/// all `view` messages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InterestMap {
    pub panes: HashMap<String, PaneInterest>,
    /// Panes whose scrollback overlay is open (slower `recent` polling).
    pub scrollback: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneInterest {
    pub viewers: u32,
    pub focused_viewers: u32,
}

/// Desired subscription set for a server: all filterless kinds, plus
/// per-pane `agent_status_changed` for every pane in state.
///
/// Deliberately independent of viewer interest: herdr allows one
/// subscription set per connection and a swap means a new connection, and
/// on servers that replay event history a re-subscription re-floods
/// (see ServerRun::stream_events). View-dependent data (scrollback text)
/// is pulled by the screen poller instead — `pane.scroll_changed` is not
/// subscribed at all; nothing consumes it.
pub fn desired_subscriptions(state: &ServerState) -> Vec<Subscription> {
    let mut subs: Vec<Subscription> = FILTERLESS_KINDS
        .iter()
        .map(|k| Subscription::filterless(k))
        .collect();
    for pane_id in state.panes.keys() {
        subs.push(Subscription::pane(
            "pane.agent_status_changed",
            pane_id.clone(),
        ));
    }
    subs
}

/// Cheap drift detection: hash of the pane-relevant part of a subscription set.
pub fn subscription_set_hash(subs: &[Subscription]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in subs {
        s.kind.hash(&mut h);
        s.pane_id.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str, data: serde_json::Value) -> EventEnvelope {
        EventEnvelope {
            event: kind.to_string(),
            data,
        }
    }

    /// Records ride nested under a key (`data.workspace`, `data.pane`, ...)
    /// with a `type` echo — the pinned 0.9.0 schema shape. Regression guard
    /// against parsing `data` directly (which yields default-filled records).
    #[test]
    fn event_records_are_nested() {
        let mut state = ServerState::new();

        let out = apply_event(
            &mut state,
            &ev(
                "workspace_created",
                json!({"type": "workspace_created", "workspace": {
                    "workspace_id": "w9", "number": 9, "label": "New", "focused": false,
                    "pane_count": 0, "tab_count": 0, "active_tab_id": null,
                    "agent_status": "idle"}}),
            ),
        );
        assert!(out.changed);
        assert_eq!(state.workspaces.get("w9").unwrap().label, "New");
        assert_eq!(state.workspaces.len(), 1);

        let out = apply_event(
            &mut state,
            &ev("tab_created", json!({"type": "tab_created", "tab": {
                "tab_id": "w9:t1", "workspace_id": "w9", "number": 1, "label": "1",
                "focused": true, "pane_count": 0, "agent_status": "idle"}})),
        );
        assert!(out.changed);
        assert_eq!(state.tabs.get("w9:t1").unwrap().workspace_id, "w9");

        let out = apply_event(
            &mut state,
            &ev("pane_created", json!({"type": "pane_created", "pane": {
                "pane_id": "w9:p1", "terminal_id": "term_9", "workspace_id": "w9",
                "tab_id": "w9:t1", "focused": true, "agent_status": "idle",
                "revision": 3, "agent": "coder"}})),
        );
        assert!(out.changed && out.pane_set_changed);
        assert_eq!(state.panes.get("w9:p1").unwrap().agent.as_deref(), Some("coder"));

        let out = apply_event(
            &mut state,
            &ev("layout_updated", json!({"type": "layout_updated", "layout": {
                "workspace_id": "w9", "tab_id": "w9:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 80, "height": 24},
                "focused_pane_id": "w9:p1",
                "panes": [{"pane_id": "w9:p1", "focused": true,
                           "rect": {"x": 0, "y": 0, "width": 80, "height": 24}}],
                "splits": []}})),
        );
        assert!(out.changed);
        assert_eq!(state.layouts["w9:t1"].panes.len(), 1);
    }

    /// Flat-key events: renames, focus, and the dotted subscription kinds.
    #[test]
    fn flat_events_apply() {
        let mut state = ServerState::new();
        apply_event(
            &mut state,
            &ev("workspace_created", json!({"type": "workspace_created", "workspace": {
                "workspace_id": "w1", "number": 1, "label": "Old", "focused": true,
                "pane_count": 1, "tab_count": 1, "active_tab_id": "w1:t1",
                "agent_status": "idle"}})),
        );
        apply_event(
            &mut state,
            &ev("tab_created", json!({"type": "tab_created", "tab": {
                "tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1",
                "focused": true, "pane_count": 1, "agent_status": "idle"}})),
        );
        apply_event(
            &mut state,
            &ev("pane_created", json!({"type": "pane_created", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": true, "agent_status": "idle",
                "revision": 1, "agent": null}})),
        );

        apply_event(
            &mut state,
            &ev("workspace_renamed", json!({"type": "workspace_renamed",
                "workspace_id": "w1", "label": "Fresh"})),
        );
        assert_eq!(state.workspaces["w1"].label, "Fresh");

        // Dotted subscription kind (no `type` echo).
        apply_event(
            &mut state,
            &ev("pane.agent_status_changed", json!({
                "pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "blocked"})),
        );
        assert_eq!(state.panes["w1:p1"].agent_status, AgentStatus::Blocked);
        assert_eq!(state.tabs["w1:t1"].agent_status, AgentStatus::Blocked);

        apply_event(
            &mut state,
            &ev("pane_focused", json!({"type": "pane_focused",
                "pane_id": "w1:p1", "workspace_id": "w1"})),
        );
        assert_eq!(state.focused.pane.as_deref(), Some("w1:p1"));

        let out = apply_event(
            &mut state,
            &ev("pane_closed", json!({"type": "pane_closed",
                "pane_id": "w1:p1", "workspace_id": "w1"})),
        );
        assert!(out.changed && out.pane_set_changed);
        assert!(!state.panes.contains_key("w1:p1"));
    }

    /// Stale buffered events replayed over a newer snapshot are no-ops, and
    /// unknown kinds never panic (forwarded untouched).
    #[test]
    fn stale_and_unknown_events() {
        let mut state = ServerState::new();
        apply_event(
            &mut state,
            &ev("pane_closed", json!({"type": "pane_closed", "pane_id": "gone", "workspace_id": "w1"})),
        );
        let out = apply_event(
            &mut state,
            &ev("future_event_kind", json!({"whatever": true})),
        );
        assert!(!out.changed && !out.pane_set_changed);
    }

    /// herdr keeps probing and pushing pane_updated for panes of closed
    /// workspaces long after its snapshot and subscription validator forgot
    /// them (verified live on 0.8.2). Such zombie updates must not resurrect
    /// a removed pane.
    #[test]
    fn zombie_pane_update_is_a_noop() {
        let mut state = ServerState::new();
        apply_event(
            &mut state,
            &ev("pane_created", json!({"type": "pane_created", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": true, "agent_status": "idle",
                "revision": 1, "agent": null}})),
        );
        apply_event(
            &mut state,
            &ev("pane_closed", json!({"type": "pane_closed",
                "pane_id": "w1:p1", "workspace_id": "w1"})),
        );

        let out = apply_event(
            &mut state,
            &ev("pane_updated", json!({"type": "pane_updated", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": false, "agent_status": "idle",
                "revision": 7, "agent": null}})),
        );
        assert!(!out.changed && !out.pane_set_changed);
        assert!(!state.panes.contains_key("w1:p1"));

        // An update for a pane we never saw created is equally inert.
        let out = apply_event(
            &mut state,
            &ev("pane_updated", json!({"type": "pane_updated", "pane": {
                "pane_id": "w2:p9", "terminal_id": "term_9", "workspace_id": "w2",
                "tab_id": "w2:t1", "focused": false, "agent_status": "idle",
                "revision": 1, "agent": null}})),
        );
        assert!(!out.changed);
        assert!(!state.panes.contains_key("w2:p9"));
    }

    /// herdr emits only workspace_closed / tab_closed — no per-pane events —
    /// so closures cascade: the workspace's tabs, panes, layouts, and agents
    /// all go, and the pane-set change is reported (drives a resubscribe).
    #[test]
    fn closures_cascade() {
        let mut state = ServerState::new();
        for (ws, tab, pane) in [("w1", "w1:t1", "w1:p1"), ("w1", "w1:t2", "w1:p2"), ("w2", "w2:t1", "w2:p1")] {
            apply_event(&mut state, &ev("workspace_created", json!({
                "type": "workspace_created", "workspace": {
                    "workspace_id": ws, "number": 1, "label": ws, "focused": false,
                    "pane_count": 1, "tab_count": 1, "active_tab_id": tab,
                    "agent_status": "idle"}})));
            apply_event(&mut state, &ev("tab_created", json!({
                "type": "tab_created", "tab": {
                    "tab_id": tab, "workspace_id": ws, "number": 1, "label": "1",
                    "focused": false, "pane_count": 1, "agent_status": "idle"}})));
            apply_event(&mut state, &ev("pane_created", json!({
                "type": "pane_created", "pane": {
                    "pane_id": pane, "terminal_id": "term", "workspace_id": ws,
                    "tab_id": tab, "focused": false, "agent_status": "idle",
                    "revision": 1, "agent": null}})));
            apply_event(&mut state, &ev("layout_updated", json!({
                "type": "layout_updated", "layout": {
                    "workspace_id": ws, "tab_id": tab, "zoomed": false,
                    "area": {"x": 0, "y": 0, "width": 80, "height": 24},
                    "focused_pane_id": pane,
                    "panes": [{"pane_id": pane, "focused": false,
                               "rect": {"x": 0, "y": 0, "width": 80, "height": 24}}],
                    "splits": []}})));
        }

        let out = apply_event(
            &mut state,
            &ev("workspace_closed", json!({"type": "workspace_closed", "workspace_id": "w1"})),
        );
        assert!(out.changed && out.pane_set_changed);
        assert!(!state.workspaces.contains_key("w1"));
        assert!(!state.tabs.contains_key("w1:t1") && !state.tabs.contains_key("w1:t2"));
        assert!(!state.panes.contains_key("w1:p1") && !state.panes.contains_key("w1:p2"));
        assert!(!state.layouts.contains_key("w1:t1"));
        // The other workspace is untouched.
        assert!(state.panes.contains_key("w2:p1"));

        let out = apply_event(
            &mut state,
            &ev("tab_closed", json!({"type": "tab_closed", "tab_id": "w2:t1"})),
        );
        assert!(out.changed && out.pane_set_changed);
        assert!(!state.panes.contains_key("w2:p1"));
    }

    /// The bootstrap recipe's drain step: events buffered between the
    /// subscribe ack and the snapshot install must fold ON TOP of the
    /// snapshot (upsert), and a stale event already reflected by the
    /// snapshot must be a harmless no-op.
    #[test]
    fn bootstrap_drain_folds_over_snapshot() {
        let snap: SessionSnapshot = serde_json::from_value(json!({
            "version": "0.9.0", "protocol": 22,
            "focused_workspace_id": "w1", "focused_tab_id": "w1:t1", "focused_pane_id": "w1:p1",
            "workspaces": [{"workspace_id": "w1", "number": 1, "label": "Snapshot",
                            "focused": true, "pane_count": 1, "tab_count": 1,
                            "active_tab_id": "w1:t1", "agent_status": "idle"}],
            "tabs": [], "panes": [], "layouts": [], "agents": []
        }))
        .unwrap();

        let mut state = ServerState::new();
        state.install_snapshot(&snap);
        // Buffered during the snapshot fetch: a brand-new workspace...
        apply_event(
            &mut state,
            &ev("workspace_created", json!({"type": "workspace_created", "workspace": {
                "workspace_id": "w2", "number": 2, "label": "Buffered", "focused": false,
                "pane_count": 0, "tab_count": 0, "active_tab_id": null,
                "agent_status": "unknown"}})),
        );
        // ...and a stale rename that the snapshot already reflects.
        apply_event(
            &mut state,
            &ev("workspace_renamed", json!({"type": "workspace_renamed",
                "workspace_id": "w1", "label": "Snapshot"})),
        );

        let labels: Vec<&str> = state.workspaces.values().map(|w| w.label.as_str()).collect();
        assert!(labels.contains(&"Snapshot"), "{labels:?}");
        assert!(labels.contains(&"Buffered"), "{labels:?}");
        assert_eq!(state.generation, 1);
    }

    /// 0.8.2 flicker seen live: `session.snapshot` intermittently lists
    /// panes of closed workspaces while omitting the workspace itself.
    /// Installing such an orphan must not happen — it fails every per-pane
    /// subscription with pane_not_found and loops drop/re-add forever.
    #[test]
    fn install_snapshot_drops_orphan_panes() {
        let snap: SessionSnapshot = serde_json::from_value(json!({
            "version": "0.8.2", "protocol": 20,
            "focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null,
            "workspaces": [],
            "tabs": [],
            "panes": [
                {"pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                 "tab_id": "w1:t1", "focused": false, "agent_status": "idle",
                 "revision": 0, "agent": null},
                {"pane_id": "w1:p2", "terminal_id": "term_2", "workspace_id": "w1",
                 "tab_id": "w1:t1", "focused": false, "agent_status": "idle",
                 "revision": 0, "agent": null}
            ],
            "layouts": [{"workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                         "area": {"x": 0, "y": 0, "width": 80, "height": 24},
                         "focused_pane_id": "w1:p1",
                         "panes": [{"pane_id": "w1:p1", "focused": true,
                                    "rect": {"x": 0, "y": 0, "width": 40, "height": 24}}],
                         "splits": []}],
            "agents": []
        }))
        .unwrap();

        let mut state = ServerState::new();
        state.install_snapshot(&snap);
        assert!(state.panes.is_empty(), "orphan panes must not install");
        assert!(state.layouts.is_empty(), "orphan layouts must not install");
    }

    /// Same flicker on the event stream: a pane_created whose workspace or
    /// tab is absent is a phantom of a closed workspace (herdr emits real
    /// creates strictly workspace → tab → pane, so this drops nothing legit).
    #[test]
    fn phantom_pane_created_is_ignored() {
        let mut state = ServerState::new();
        // No workspace w1 / tab w1:t1 exists.
        let out = apply_event(
            &mut state,
            &ev("pane_created", json!({"type": "pane_created", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": true, "agent_status": "idle",
                "revision": 0, "agent": null}})),
        );
        assert!(!out.changed && !out.pane_set_changed);
        assert!(!state.panes.contains_key("w1:p1"));

        // A real create (workspace and tab present) still lands.
        apply_event(&mut state, &ev("workspace_created", json!({
            "type": "workspace_created", "workspace": {
                "workspace_id": "w1", "number": 1, "label": "W", "focused": false,
                "pane_count": 0, "tab_count": 0, "active_tab_id": null,
                "agent_status": "idle"}})));
        apply_event(&mut state, &ev("tab_created", json!({
            "type": "tab_created", "tab": {
                "tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1",
                "focused": true, "pane_count": 0, "agent_status": "idle"}})));
        let out = apply_event(
            &mut state,
            &ev("pane_created", json!({"type": "pane_created", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": true, "agent_status": "idle",
                "revision": 0, "agent": null}})),
        );
        assert!(out.changed && out.pane_set_changed);
        assert!(state.panes.contains_key("w1:p1"));
    }
}
