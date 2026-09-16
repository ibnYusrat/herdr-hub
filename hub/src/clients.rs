//! Client sessions: WebSocket protocol with token auth, per-client outbox
//! with screen coalescing, view aggregation into the interest map.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use crate::actions::Actions;
use crate::hubproto::*;
use crate::screen::{frame_for, ScreenUpdate};
use crate::state::InterestMap;

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// What one client currently sees (its last `view` message).
#[derive(Debug, Clone, Default)]
pub struct ClientView {
    pub server: String,
    pub panes: HashMap<String, bool>, // pane_id → focused
    pub scrollback: HashSet<String>,
}

struct ClientSlot {
    view: ClientView,
    outbox: mpsc::Sender<HubFrame>,
    /// Last screen revision delivered per (pane, source).
    pane_last: HashMap<(String, ScreenSource), u64>,
}

/// Shared registry: live client views + outboxes, aggregated interest.
pub struct Registry {
    slots: Mutex<HashMap<u64, ClientSlot>>,
    interest_tx: watch::Sender<InterestMap>,
}

impl Registry {
    pub fn new(interest_tx: watch::Sender<InterestMap>) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            interest_tx,
        }
    }

    fn register(&self, id: u64, outbox: mpsc::Sender<HubFrame>) {
        self.slots.lock().unwrap().insert(
            id,
            ClientSlot {
                view: ClientView::default(),
                outbox,
                pane_last: HashMap::new(),
            },
        );
        self.recompute_interest();
    }

    fn remove(&self, id: u64) {
        self.slots.lock().unwrap().remove(&id);
        self.recompute_interest();
    }

    fn set_view(&self, id: u64, view: ClientView) {
        let mut slots = self.slots.lock().unwrap();
        if let Some(slot) = slots.get_mut(&id) {
            slot.view = view;
        }
        drop(slots);
        self.recompute_interest();
    }

    fn recompute_interest(&self) {
        let slots = self.slots.lock().unwrap();
        let mut interest = InterestMap::default();
        for slot in slots.values() {
            for (pane, focused) in &slot.view.panes {
                let entry = interest.panes.entry(pane.clone()).or_default();
                entry.viewers += 1;
                if *focused {
                    entry.focused_viewers += 1;
                }
            }
            for pane in &slot.view.scrollback {
                interest.scrollback.insert(pane.clone());
            }
        }
        drop(slots);
        self.interest_tx.send_if_modified(|cur| {
            if *cur == interest {
                false
            } else {
                *cur = interest;
                true
            }
        });
    }

    /// Deliver a screen update to every client viewing that pane. Clients
    /// whose last frame matches the diff base get row deltas; everyone else
    /// (including new viewers and dropped-frame stragglers) gets a full
    /// frame. A client's tracked revision only advances when the frame was
    /// actually queued, so a dropped delta self-heals into a full send.
    pub fn deliver_screen(&self, server: &str, source: ScreenSource, update: &ScreenUpdate) {
        let mut slots = self.slots.lock().unwrap();
        for (id, slot) in slots.iter_mut() {
            let views = match source {
                ScreenSource::Visible => slot.view.server == server && slot.view.panes.contains_key(&update.pane),
                ScreenSource::Recent => slot.view.server == server && slot.view.scrollback.contains(&update.pane),
            };
            if !views {
                continue;
            }
            let key = (update.pane.clone(), source);
            let has_base = update.prev_revision != 0
                && slot.pane_last.get(&key) == Some(&update.prev_revision);
            let frame = HubFrame::Screen(frame_for(update, server, has_base));
            if slot.outbox.try_send(frame).is_ok() {
                slot.pane_last.insert(key, update.revision);
            } else {
                debug!(client = id, pane = %update.pane, "outbox full, dropping screen frame");
            }
        }
    }
}

/// Everything a client session needs.
pub struct SessionCtx {
    pub registry: Arc<Registry>,
    pub broadcast: broadcast::Sender<HubFrame>,
    pub actions: Arc<Actions>,
    pub servers: Vec<ServerHandleInfo>,
    pub token: String,
}

#[derive(Clone)]
pub struct ServerHandleInfo {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub peek: Arc<arc_swap::ArcSwap<crate::state::ServerState>>,
}

/// Run one authenticated-or-not WebSocket session to completion.
pub async fn run_session(socket: WebSocket, ctx: Arc<SessionCtx>) {
    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();

    // --- Auth: first frame must be hello (SPEC §9: token in hello, never URL).
    let hello = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(msg))) => msg,
        _ => {
            let _ = sink
                .send(Message::text(encode(&error_response(
                    None,
                    "hub.auth_failed",
                    "expected hello as the first frame",
                ))))
                .await;
            return;
        }
    };
    let frame = match parse_client_frame(&hello) {
        Some(f) => f,
        None => {
            let _ = sink
                .send(Message::text(encode(&error_response(
                    None,
                    "hub.bad_message",
                    "first frame must be a JSON hello",
                ))))
                .await;
            return;
        }
    };
    let client_info = match frame {
        ClientFrame::Hello {
            protocol,
            token,
            client,
        } => {
            if protocol != PROTOCOL {
                let _ = sink
                    .send(Message::text(encode(&error_response(
                        None,
                        "hub.protocol_unsupported",
                        format!("hub speaks protocol {PROTOCOL}, client sent {protocol}"),
                    ))))
                    .await;
                return;
            }
            if !constant_time_eq(&token, &ctx.token) {
                let _ = sink
                    .send(Message::text(encode(&error_response(
                        None,
                        "hub.auth_failed",
                        "invalid token",
                    ))))
                    .await;
                info!(client = id, "rejected hello: bad token");
                return;
            }
            client
        }
        _ => {
            let _ = sink
                .send(Message::text(encode(&error_response(
                    None,
                    "hub.auth_failed",
                    "expected hello as the first frame",
                ))))
                .await;
            return;
        }
    };
    info!(client = id, name = ?client_info.name, "client authenticated");

    // --- Outbox + writer task (coalescing per-pane screens, capped rate).
    let (outbox_tx, outbox_rx) = mpsc::channel::<HubFrame>(256);
    ctx.registry.register(id, outbox_tx.clone());
    let writer = tokio::spawn(writer_task(sink, outbox_rx));

    // --- Welcome + initial snapshots. The broadcast receiver is created
    // BEFORE the snapshots are read, so events racing the join are buffered
    // and delivered after the snapshots (same ordering trick as bootstrap).
    let mut bc = ctx.broadcast.subscribe();
    let _ = outbox_tx
        .send(HubFrame::Welcome {
            protocol: PROTOCOL,
            hub: HubIdentity {
                name: "herdr-hub",
                version: env!("CARGO_PKG_VERSION"),
            },
            servers: ctx
                .servers
                .iter()
                .map(|s| server_entry_info(s))
                .collect(),
            heartbeat_ms: 15000,
        })
        .await;
    for server in &ctx.servers {
        let state = server.peek.load_full();
        if state.status.is_online() {
            let _ = outbox_tx
                .send(snapshot_frame(&server.id, &state))
                .await;
        }
    }

    // --- Main loop.
    let actions = ctx.actions.clone();
    let registry = ctx.registry.clone();
    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(m)) => {
                        match parse_client_frame(&m) {
                            Some(ClientFrame::View { server, panes, scrollback, .. }) => {
                                let mut view = ClientView {
                                    server,
                                    panes: HashMap::new(),
                                    scrollback: scrollback.into_iter().collect(),
                                };
                                for p in panes {
                                    view.panes.insert(p.pane, p.focused);
                                }
                                registry.set_view(id, view);
                            }
                            Some(ClientFrame::Request { id: rid, server, action, params }) => {
                                // Never log params: they may contain prompt
                                // text (SPEC §9).
                                debug!(client = id, req = %rid, server = %server, action = %action, "action");
                                let result = actions.execute(&server, &action, params).await;
                                let frame = match result {
                                    Ok(value) => HubFrame::Response {
                                        id: Some(rid),
                                        server: Some(server),
                                        ok: true,
                                        result: Some(value),
                                        error: None,
                                    },
                                    Err(e) => HubFrame::Response {
                                        id: Some(rid),
                                        server: Some(server),
                                        ok: false,
                                        result: None,
                                        error: Some(e),
                                    },
                                };
                                if outbox_tx.send(frame).await.is_err() {
                                    break;
                                }
                            }
                            Some(ClientFrame::Ping) => {
                                if outbox_tx.send(HubFrame::Pong).await.is_err() {
                                    break;
                                }
                            }
                            Some(ClientFrame::Hello { .. }) => {
                                let _ = outbox_tx.send(error_response(
                                    None,
                                    "hub.bad_message",
                                    "already authenticated",
                                )).await;
                            }
                            None => {
                                let _ = outbox_tx.send(error_response(
                                    None,
                                    "hub.bad_message",
                                    "unparseable frame",
                                )).await;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        debug!(client = id, error = %e, "ws receive error");
                        break;
                    }
                    None => break,
                }
            }
            bc_msg = bc.recv() => {
                match bc_msg {
                    Ok(frame) => {
                        if outbox_tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(client = id, skipped = n, "client lagged on broadcast; resyncing");
                        // Missed deltas: recover with fresh snapshots.
                        for server in &ctx.servers {
                            let state = server.peek.load_full();
                            if state.status.is_online() {
                                let _ = outbox_tx.send(snapshot_frame(&server.id, &state)).await;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    ctx.registry.remove(id);
    writer.abort();
    info!(client = id, "client disconnected");
}

/// Writer task: drains the outbox, sending ordered frames promptly and
/// coalescing screen frames per pane at ~30 fps max (SPEC §5.1.6).
async fn writer_task(mut sink: futures_util::stream::SplitSink<WebSocket, Message>, mut rx: mpsc::Receiver<HubFrame>) {
    let mut flush = tokio::time::interval(Duration::from_millis(33));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending_screens: HashMap<(String, String, ScreenSource), HubFrame> = HashMap::new();
    let mut ordered: Vec<HubFrame> = Vec::new();

    loop {
        tokio::select! {
            item = rx.recv() => {
                match item {
                    Some(HubFrame::Screen(f)) => {
                        let key = (f.server.clone(), f.pane.clone(), f.source);
                        pending_screens.insert(key, HubFrame::Screen(f));
                    }
                    Some(other) => ordered.push(other),
                    None => break,
                }
            }
            _ = flush.tick() => {
                // Ordered frames first, then at most one screen per pane.
                for frame in ordered.drain(..) {
                    if sink.send(Message::text(encode(&frame))).await.is_err() {
                        return;
                    }
                }
                let mut screens: Vec<HubFrame> = pending_screens.drain().map(|(_, v)| v).collect();
                screens.sort_by_key(|f| match f {
                    HubFrame::Screen(s) => (s.server.clone(), s.pane.clone()),
                    _ => (String::new(), String::new()),
                });
                for frame in screens {
                    if sink.send(Message::text(encode(&frame))).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

fn parse_client_frame(msg: &Message) -> Option<ClientFrame> {
    let text = msg.to_text().ok()?;
    serde_json::from_str(text).ok()
}

fn server_entry_info(s: &ServerHandleInfo) -> ServerEntryInfo {
    let state = s.peek.load_full();
    ServerEntryInfo {
        id: s.id.clone(),
        label: s.label.clone(),
        kind: s.kind.to_string(),
        status: state.status.label().to_string(),
        herdr_version: state.status.version().map(str::to_string),
        protocol: state.status.protocol(),
    }
}

/// Constant-time-ish token comparison (avoids early-exit timing leaks; not
/// cryptographic, but better than ==).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_compare() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
