//! Hub protocol v1 frames (see protocol/hub-protocol.md). One JSON object per
//! WebSocket text frame, discriminated by `type`. herdr ids pass through
//! verbatim; every server-scoped frame carries a `server` field.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{FocusIds, ServerState};

pub const PROTOCOL: u32 = 1;

// ---------------------------------------------------------------------------
// Client → hub
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClientFrame {
    Hello {
        protocol: u32,
        token: String,
        #[serde(default)]
        client: ClientHelloInfo,
    },
    View {
        server: String,
        #[serde(default)]
        active: Option<ActiveView>,
        #[serde(default)]
        panes: Vec<ViewPane>,
        #[serde(default)]
        scrollback: Vec<String>,
    },
    Request {
        id: String,
        server: String,
        action: String,
        #[serde(default)]
        params: Value,
    },
    Ping,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientHelloInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub caps: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ActiveView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ViewPane {
    pub pane: String,
    #[serde(default)]
    pub focused: bool,
}

// ---------------------------------------------------------------------------
// Hub → client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubFrame {
    Welcome {
        protocol: u32,
        hub: HubIdentity,
        servers: Vec<ServerEntryInfo>,
        heartbeat_ms: u32,
    },
    Snapshot {
        server: String,
        generation: u64,
        state: SnapshotState,
    },
    Event {
        server: String,
        event: String,
        data: Value,
    },
    Screen(ScreenFrame),
    Response {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<HubWireError>,
    },
    ServerStatus {
        server: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize)]
pub struct HubIdentity {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerEntryInfo {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub herdr_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotState {
    pub workspaces: Vec<Value>,
    pub tabs: Vec<Value>,
    pub panes: Vec<Value>,
    pub layouts: Vec<Value>,
    pub agents: Vec<Value>,
    pub focused: FocusIds,
}

#[derive(Debug, Clone, Serialize)]
pub struct HubWireError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenFrame {
    pub server: String,
    pub pane: String,
    pub revision: u64,
    pub rows: u32,
    pub cols: u32,
    pub mode: ScreenMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<RowChange>>,
    /// Which read produced this: `visible` (ANSI grid) or `recent`
    /// (plain-text scrollback).
    pub source: ScreenSource,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenMode {
    Full,
    Rows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenSource {
    Visible,
    Recent,
}

#[derive(Debug, Clone, Serialize)]
pub struct RowChange {
    pub row: u32,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

pub fn hub_error(code: &str, message: impl Into<String>) -> HubWireError {
    HubWireError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn error_response(id: Option<String>, code: &str, message: impl Into<String>) -> HubFrame {
    HubFrame::Response {
        id,
        server: None,
        ok: false,
        result: None,
        error: Some(hub_error(code, message)),
    }
}

/// Build the full snapshot frame for one server from canonical state.
pub fn snapshot_frame(server: &str, state: &ServerState) -> HubFrame {
    HubFrame::Snapshot {
        server: server.to_string(),
        generation: state.generation,
        state: SnapshotState {
            workspaces: state
                .workspaces
                .values()
                .map(|w| serde_json::to_value(w).unwrap_or_default())
                .collect(),
            tabs: state
                .tabs
                .values()
                .map(|t| serde_json::to_value(t).unwrap_or_default())
                .collect(),
            panes: state
                .panes
                .values()
                .map(|p| serde_json::to_value(p).unwrap_or_default())
                .collect(),
            layouts: state
                .layouts
                .values()
                .map(|l| serde_json::to_value(l).unwrap_or_default())
                .collect(),
            agents: state
                .agents
                .iter()
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .collect(),
            focused: state.focused.clone(),
        },
    }
}

/// Serialize a frame to a WebSocket text payload.
pub fn encode(frame: &HubFrame) -> String {
    serde_json::to_string(frame).unwrap_or_else(|_| {
        serde_json::json!({"type":"response","ok":false,
            "error":{"code":"hub.encode_failed","message":"frame serialization failed"}})
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frames_parse() {
        let hello = r#"{"type":"hello","protocol":1,"token":"t","client":{"name":"web"}}"#;
        assert!(matches!(
            serde_json::from_str::<ClientFrame>(hello).unwrap(),
            ClientFrame::Hello { .. }
        ));
        let view = r#"{"type":"view","server":"local","active":{"workspace":"w1","tab":"w1:t1"},"panes":[{"pane":"w1:p1","focused":true}],"scrollback":[]}"#;
        assert!(matches!(
            serde_json::from_str::<ClientFrame>(view).unwrap(),
            ClientFrame::View { .. }
        ));
        let req = r#"{"type":"request","id":"c1","server":"local","action":"agent.prompt","params":{"target":"w1:p1","text":"hi"}}"#;
        assert!(matches!(
            serde_json::from_str::<ClientFrame>(req).unwrap(),
            ClientFrame::Request { .. }
        ));
        let bad = r#"{"type":"nonsense"}"#;
        assert!(serde_json::from_str::<ClientFrame>(bad).is_err());
    }

    #[test]
    fn screen_frame_encodes() {
        let f = HubFrame::Screen(ScreenFrame {
            server: "local".into(),
            pane: "w1:p1".into(),
            revision: 5,
            rows: 2,
            cols: 4,
            mode: ScreenMode::Full,
            lines: Some(vec!["ab".into(), "cd".into()]),
            changes: None,
            source: ScreenSource::Visible,
            truncated: false,
        });
        let s = encode(&f);
        assert!(s.contains(r#""mode":"full""#));
        assert!(s.contains(r#""source":"visible""#));
    }
}
