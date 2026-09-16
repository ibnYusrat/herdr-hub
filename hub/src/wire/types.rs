//! Hand-written serde types for the herdr API subset the hub uses.
//!
//! Shape source: `schemas/herdr-api-0.9.0-protocol22.json` (pinned contract).
//! Parsing is lenient by design: every field herdr may omit is
//! `Option`/`#[serde(default)]`, and unknown fields are ignored, so newer
//! herdr versions do not break the hub.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuccessResponse {
    #[serde(default)]
    pub id: String,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorResponse {
    #[serde(default)]
    pub id: String,
    pub error: WireError,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

/// One line on the socket: success, error, or (on subscribe connections) a
/// pushed event envelope.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SocketLine {
    Success(SuccessResponse),
    Error(ErrorResponse),
    Event(EventEnvelope),
}

/// Pushed event envelope: `{"event": "<kind>", "data": {...}}`.
/// Lifecycle kinds arrive underscored with a `data.type` echo; the three
/// filtered pane kinds arrive dotted with no `data.type`. We normalize to
/// underscore and keep `data` raw.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventEnvelope {
    pub event: String,
    pub data: serde_json::Value,
}

impl EventEnvelope {
    /// Kind normalized to underscore form (`pane.agent_status_changed` →
    /// `pane_agent_status_changed`).
    pub fn kind(&self) -> String {
        self.event.replace('.', "_")
    }
}

// ---------------------------------------------------------------------------
// Records (session.snapshot and event payloads)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub layouts: Vec<PaneLayoutSnapshot>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkspaceInfo {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u32,
    #[serde(default)]
    pub tab_count: u32,
    #[serde(default)]
    pub active_tab_id: Option<String>,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub worktree: Option<WorkspaceWorktreeInfo>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkspaceWorktreeInfo {
    #[serde(default)]
    pub repo_key: String,
    #[serde(default)]
    pub repo_name: String,
    #[serde(default)]
    pub repo_root: String,
    #[serde(default)]
    pub checkout_path: String,
    #[serde(default)]
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TabInfo {
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u32,
    #[serde(default)]
    pub agent_status: AgentStatus,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PaneInfo {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub scroll: Option<PaneScrollInfo>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PaneScrollInfo {
    #[serde(default)]
    pub offset_from_bottom: u64,
    #[serde(default)]
    pub max_offset_from_bottom: u64,
    #[serde(default)]
    pub viewport_rows: u64,
}

/// Per-tab layout: a FLAT list of pane rects and split rects in cell
/// coordinates. Structure is implied by rect containment — the web client
/// positions panes absolutely from `rect / area`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PaneLayoutSnapshot {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub area: PaneLayoutRect,
    #[serde(default)]
    pub focused_pane_id: String,
    #[serde(default)]
    pub panes: Vec<LayoutPaneEntry>,
    #[serde(default)]
    pub splits: Vec<LayoutSplit>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PaneLayoutRect {
    #[serde(default)]
    pub x: u16,
    #[serde(default)]
    pub y: u16,
    #[serde(default)]
    pub width: u16,
    #[serde(default)]
    pub height: u16,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LayoutPaneEntry {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub rect: PaneLayoutRect,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LayoutSplit {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub direction: String, // "right" | "down"
    #[serde(default)]
    pub ratio: f32,
    #[serde(default)]
    pub rect: PaneLayoutRect,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentInfo {
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub interactive_ready: Option<bool>,
    #[serde(default)]
    pub launch_pending: Option<bool>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

/// One `events.subscribe` filter entry. The three pane-scoped kinds REQUIRE a
/// `pane_id`; everything else is filterless.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Subscription {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

impl Subscription {
    pub fn filterless(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            pane_id: None,
        }
    }

    pub fn pane(kind: &str, pane_id: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            pane_id: Some(pane_id.into()),
        }
    }
}

/// Every filterless subscribable kind (source: pinned schema, verified the
/// live server accepts exactly these without filters).
pub const FILTERLESS_KINDS: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.metadata_updated",
    "workspace.renamed",
    "workspace.moved",
    "workspace.reordered",
    "workspace.closed",
    "workspace.focused",
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
    "tab.created",
    "tab.closed",
    "tab.focused",
    "tab.renamed",
    "tab.moved",
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.focused",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    "layout.updated",
];

// ---------------------------------------------------------------------------
// pane.read
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct PaneReadResult {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Result extraction helpers (results are `type`-discriminated)
// ---------------------------------------------------------------------------

pub fn result_tag(result: &serde_json::Value) -> Option<&str> {
    result.get("type").and_then(|v| v.as_str())
}

pub fn unwrap_result<T: serde::de::DeserializeOwned>(
    result: &serde_json::Value,
    tag: &str,
    key: &str,
) -> anyhow::Result<T> {
    let actual = result_tag(result).unwrap_or("");
    if actual != tag {
        anyhow::bail!(
            "herdr result type mismatch: expected {tag}, got {actual:?} in {result}"
        );
    }
    let inner = result
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("herdr result missing field {key:?}: {result}"))?;
    Ok(serde_json::from_value(inner.clone())?)
}
