//! Curated action proxy: client `request` messages become herdr API calls,
//! restricted to an allowlist, with the in-flight prompt guard and
//! protocol-based method availability gating (SPEC §5.2, §7, §12).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use serde_json::Value;

use crate::hubproto::{hub_error, HubWireError};
use crate::state::ServerState;
use crate::wire::{HerdrClient, HerdrError};

/// The curated subset of herdr methods clients may invoke through the hub.
pub const ALLOWED_ACTIONS: &[&str] = &[
    // workspaces + worktrees
    "workspace.create",
    "workspace.focus",
    "workspace.rename",
    "workspace.close",
    "worktree.create",
    "worktree.open",
    "worktree.remove",
    // tabs
    "tab.create",
    "tab.focus",
    "tab.rename",
    "tab.close",
    // panes
    "pane.focus",
    "pane.zoom",
    "pane.rename",
    "pane.close",
    "pane.split",
    "pane.resize",
    "pane.scroll",
    "pane.send_text",
    "pane.send_keys",
    "pane.send_input",
    // layouts
    "layout.set_split_ratio",
    // agents
    "agent.prompt",
    "agent.send_keys",
];

/// Methods that only exist in newer herdr protocols; gated by the server's
/// advertised protocol number (e.g. `pane.scroll` arrived with 0.9.0/22).
const MIN_PROTOCOL: &[(&str, u32)] = &[("pane.scroll", 22)];

struct ServerTarget {
    client: Arc<HerdrClient>,
    state: Arc<ArcSwap<ServerState>>,
}

pub struct Actions {
    servers: HashMap<String, ServerTarget>,
    /// `agent.prompt` in flight per (server, target) — the hub enforces the
    /// no-duplicate-submission rule (SPEC §12), not just the client.
    in_flight: Mutex<HashSet<String>>,
}

impl Actions {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    pub fn register(
        &mut self,
        server: &str,
        client: Arc<HerdrClient>,
        state: Arc<ArcSwap<ServerState>>,
    ) {
        self.servers.insert(
            server.to_string(),
            ServerTarget { client, state },
        );
    }

    pub async fn execute(
        &self,
        server: &str,
        action: &str,
        params: Value,
    ) -> Result<Value, HubWireError> {
        let target = self
            .servers
            .get(server)
            .ok_or_else(|| hub_error("hub.unknown_server", format!("unknown server {server:?}")))?;

        if !ALLOWED_ACTIONS.contains(&action) {
            return Err(hub_error(
                "hub.action_not_allowed",
                format!("action {action:?} is not in the hub allowlist"),
            ));
        }

        let state = target.state.load_full();
        if !state.status.is_online() {
            return Err(hub_error(
                "hub.server_offline",
                format!("server {server:?} is {}", state.status.label()),
            ));
        }
        if let Some((_, min)) = MIN_PROTOCOL.iter().find(|(m, _)| *m == action) {
            let actual = state.status.protocol().unwrap_or(0);
            if actual < *min {
                return Err(hub_error(
                    "hub.action_not_allowed",
                    format!(
                        "{action} needs herdr protocol {min}, server speaks {actual}; update herdr on the server"
                    ),
                ));
            }
        }

        // One in-flight agent.prompt per target; herdr refuses prompts to
        // blocked agents with agent_blocked and we never auto-retry
        // (SPEC §7): a second attempt while one is in flight is rejected
        // rather than queued, so a timeout can never double-send.
        let prompt_key = (action == "agent.prompt").then(|| {
            params
                .get("target")
                .and_then(|v| v.as_str())
                .map(|t| format!("{server}:{t}"))
        });
        if let Some(Some(key)) = &prompt_key {
            let mut guard = self.in_flight.lock().unwrap();
            if !guard.insert(key.clone()) {
                return Err(hub_error(
                    "hub.prompt_in_flight",
                    "a prompt to this target is still being submitted; read the pane before retrying",
                ));
            }
        }

        let result = target.client.request(action, params).await;

        if let Some(Some(key)) = &prompt_key {
            self.in_flight.lock().unwrap().remove(key);
        }

        match result {
            Ok(value) => Ok(value),
            Err(HerdrError::Herdr(code, message)) => {
                // herdr errors (agent_blocked, not_found, …) pass through
                // verbatim — the client sees herdr's own semantics.
                Err(hub_error(&code, message))
            }
            Err(e) => Err(hub_error(
                "hub.server_offline",
                format!("herdr call failed: {e}"),
            )),
        }
    }
}

impl Default for Actions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_contains_expected_actions() {
        for a in [
            "agent.prompt",
            "agent.send_keys",
            "pane.send_text",
            "pane.send_keys",
            "pane.send_input",
            "workspace.create",
            "workspace.close",
            "layout.set_split_ratio",
        ] {
            assert!(ALLOWED_ACTIONS.contains(&a), "{a} missing");
        }
        for forbidden in ["server.stop", "pane.read", "events.subscribe", "plugin.action.invoke"] {
            assert!(!ALLOWED_ACTIONS.contains(&forbidden), "{forbidden} must not be allowed");
        }
    }
}
