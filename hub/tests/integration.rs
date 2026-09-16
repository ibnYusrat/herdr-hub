//! Integration tests: a fake herdr NDJSON server drives the full hub
//! (bootstrap, subscription swap, reconnect, screen polling) over real
//! WebSockets. No live herdr needed.

use std::ops::Not;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use herdr_hub::app;
use herdr_hub::config::{Config, ServerEntry, ServerKind};

// ---------------------------------------------------------------------------
// Fake herdr server
// ---------------------------------------------------------------------------

struct FakeInner {
    snapshot: Value,
    /// Reported by `ping`; 22 = streaming-capable, 20 = hub poll mode.
    protocol: u64,
    /// Bumped on every `set_text`; drives `revision`.
    revision: u64,
    text: String,
    subscribers: Vec<mpsc::UnboundedSender<String>>,
    subscribe_sets: Vec<Vec<String>>,
    /// Panes that failed a subscribe with `pane_not_found` (live herdr
    /// rejects the whole set when it names an unknown pane).
    dead_pane_rejects: Vec<String>,
    /// When set, `session.snapshot` waits for it before replying.
    snapshot_gate: Option<oneshot::Receiver<()>>,
    /// When set, `agent.prompt` waits for it before replying.
    prompt_gate: Option<oneshot::Receiver<()>>,
    /// Every `pane.read` seen: (pane_id, params) — lets tests assert the
    /// wire source the poller chose.
    pane_reads: Vec<(String, Value)>,
}

#[derive(Clone)]
struct FakeHerdr {
    dir: Arc<tempfile::TempDir>,
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeHerdr {
    fn start(base: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("herdr.sock");
        let fake = Self {
            dir: Arc::new(dir),
            inner: Arc::new(Mutex::new(FakeInner {
                snapshot: base,
                protocol: 22,
                revision: 1,
                text: "line one\nline two\n".into(),
                subscribers: Vec::new(),
                subscribe_sets: Vec::new(),
                dead_pane_rejects: Vec::new(),
                snapshot_gate: None,
                prompt_gate: None,
                pane_reads: Vec::new(),
            })),
        };
        fake.serve(socket);
        fake
    }

    fn socket_path(&self) -> PathBuf {
        self.dir.path().join("herdr.sock")
    }

    /// Pretend to be herdr 0.8.x (protocol 20): the hub must run in poll
    /// mode — no event subscriptions, snapshot reconcile only.
    fn set_protocol(&self, protocol: u64) {
        self.inner.lock().unwrap().protocol = protocol;
    }

    fn serve(&self, socket: PathBuf) {
        let listener = UnixListener::bind(&socket).unwrap();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let inner = inner.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, inner).await {
                        eprintln!("fake herdr conn error: {e}");
                    }
                });
            }
        });
    }

    /// Push one event line to all subscribers (underscore envelope form).
    fn emit(&self, line: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.subscribers.retain(|s| s.send(line.clone()).is_ok());
    }

    /// Add a pane record to the base snapshot (used with pane_created).
    fn add_pane(&self, pane: Value) {
        let mut inner = self.inner.lock().unwrap();
        inner.snapshot["panes"].as_array_mut().unwrap().push(pane);
    }

    fn set_text(&self, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.text = text.to_string();
        inner.revision += 1;
    }

    fn subscribe_sets(&self) -> Vec<Vec<String>> {
        self.inner.lock().unwrap().subscribe_sets.clone()
    }

    /// All `pane.read` requests seen so far: (pane_id, params).
    fn pane_reads(&self) -> Vec<(String, Value)> {
        self.inner.lock().unwrap().pane_reads.clone()
    }

    fn dead_pane_rejects(&self) -> Vec<String> {
        self.inner.lock().unwrap().dead_pane_rejects.clone()
    }

    /// Remove a pane from the snapshot WITHOUT emitting pane_closed — the
    /// hub's retained state now names a pane the server no longer knows.
    fn drop_pane_from_snapshot(&self, pane_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .snapshot["panes"]
            .as_array_mut()
            .unwrap()
            .retain(|p| p["pane_id"].as_str() != Some(pane_id));
    }

    fn hold_snapshots(&self) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap().snapshot_gate = Some(rx);
        tx
    }

    /// Hold the next `agent.prompt` until released.
    fn hold_prompts(&self) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap().prompt_gate = Some(rx);
        tx
    }

    /// Drop all subscriber connections (simulates server restart).
    fn kill_subscribers(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.subscribers.clear();
    }
}

async fn handle_conn(stream: UnixStream, inner: Arc<Mutex<FakeInner>>) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: Value = serde_json::from_str(&line)?;
        let id = req.get("id").cloned().unwrap_or(json!(""));
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        match method.as_str() {
            "ping" => {
                let protocol = inner.lock().unwrap().protocol;
                reply(
                    &mut write,
                    json!({"id": id, "result": {"type": "pong", "version": "0.9.0", "protocol": protocol}}),
                )
                .await?;
            }
            "session.snapshot" => {
                // Optional gate so tests can emit events mid-bootstrap.
                let gate = inner.lock().unwrap().snapshot_gate.take();
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                let snap = inner.lock().unwrap().snapshot.clone();
                reply(
                    &mut write,
                    json!({"id": id, "result": {"type": "session_snapshot", "snapshot": snap}}),
                )
                .await?;
            }
            "events.subscribe" => {
                // Live-herdr semantics: a per-pane sub for an unknown pane
                // fails the whole subscribe with pane_not_found.
                let known: Vec<String> = {
                    let g = inner.lock().unwrap();
                    g.snapshot["panes"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| p["pane_id"].as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                if let Some(dead) = req["params"]["subscriptions"].as_array().and_then(|subs| {
                    subs.iter().find_map(|s| {
                        let p = s["pane_id"].as_str()?;
                        known.iter().any(|k| k == p).not().then(|| p.to_string())
                    })
                }) {
                    inner.lock().unwrap().dead_pane_rejects.push(dead.clone());
                    reply(
                        &mut write,
                        json!({"id": id, "error": {
                            "code": "pane_not_found",
                            "message": format!("pane {dead} not found")
                        }}),
                    )
                    .await?;
                    continue;
                }
                let subs: Vec<String> = req["params"]["subscriptions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|s| {
                                let kind = s["type"].as_str().unwrap_or("").to_string();
                                match s["pane_id"].as_str() {
                                    Some(p) => format!("{kind}:{p}"),
                                    None => kind,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                reply(&mut write, json!({"id": id, "result": {"type": "subscription_started"}}))
                    .await?;
                // Register the subscriber BEFORE recording the set (same
                // lock), so a test that waits for the set can emit without a
                // delivery race. Then hold the connection open, forwarding
                // pushes; when `kill_subscribers` clears the list the channel
                // closes and we drop the connection entirely (real EOF).
                let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                {
                    let mut guard = inner.lock().unwrap();
                    guard.subscribers.push(tx);
                    guard.subscribe_sets.push(subs);
                }
                while let Some(ev) = rx.recv().await {
                    write.write_all(ev.as_bytes()).await?;
                    write.write_all(b"\n").await?;
                }
                return Ok(());
            }
            "pane.read" => {
                let pane_id = req["params"]["pane_id"].as_str().unwrap_or("").to_string();
                {
                    let mut g = inner.lock().unwrap();
                    g.pane_reads.push((pane_id.clone(), req["params"].clone()));
                }
                let (text, revision) = {
                    let g = inner.lock().unwrap();
                    (g.text.clone(), g.revision)
                };
                reply(
                    &mut write,
                    json!({"id": id, "result": {"type": "pane_read", "read": {
                        "pane_id": pane_id, "workspace_id": "w1", "tab_id": "w1:t1",
                        "source": "visible", "format": "ansi",
                        "text": text, "revision": revision, "truncated": false
                    }}}),
                )
                .await?;
            }
            other => {
                if other == "agent.prompt" {
                    // Optional gate so tests can hold a prompt in flight.
                    let gate = inner.lock().unwrap().prompt_gate.take();
                    if let Some(gate) = gate {
                        let _ = gate.await;
                    }
                }
                reply(&mut write, json!({"id": id, "result": {"type": "ok", "method": other}}))
                    .await?;
            }
        }
    }
    Ok(())
}

async fn reply<W: tokio::io::AsyncWrite + Unpin + ?Sized>(
    write: &mut W,
    v: Value,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    write.write_all(v.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

fn base_snapshot() -> Value {
    json!({
        "version": "0.9.0", "protocol": 22,
        "focused_workspace_id": "w1", "focused_tab_id": "w1:t1", "focused_pane_id": "w1:p1",
        "workspaces": [{
            "workspace_id": "w1", "number": 1, "label": "Base", "focused": true,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "w1:t1", "agent_status": "idle"
        }],
        "tabs": [{
            "tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1",
            "focused": true, "pane_count": 1, "agent_status": "idle"
        }],
        "panes": [{
            "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1", "tab_id": "w1:t1",
            "focused": true, "agent_status": "idle", "revision": 1, "agent": null
        }],
        "layouts": [{
            "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
            "area": {"x": 0, "y": 0, "width": 80, "height": 24}, "focused_pane_id": "w1:p1",
            "panes": [{"pane_id": "w1:p1", "focused": true, "rect": {"x": 0, "y": 0, "width": 80, "height": 24}}],
            "splits": []
        }],
        "agents": []
    })
}

/// Spawn hub + fake herdr; return the WS url and auth token.
async fn spawn_hub_with_fake(base: Value) -> (FakeHerdr, String, String) {
    let fake = FakeHerdr::start(base);
    let (url, token) = spawn_hub_against(&fake).await;
    (fake, url, token)
}

/// Spawn the hub against an already-running fake (so tests can arm gates
/// before any hub connection is made). Reconciliation runs every second so
/// drift-healing is observable in tests.
async fn spawn_hub_against(fake: &FakeHerdr) -> (String, String) {
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        poll_ms: 100,
        reconcile_secs: 1,
        servers: vec![ServerEntry {
            id: "fake".into(),
            label: Some("Fake".into()),
            kind: ServerKind::Local {
                socket: Some(fake.socket_path()),
                session: None,
            },
        }],
        ..Config::default()
    };
    let token = "test-token-1234".to_string();
    let hub = app::spawn_hub(&cfg, token.clone());
    let router = app::build_app(&cfg, hub.ctx.clone(), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("ws://{addr}/ws"), token)
}

struct WsClient {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl WsClient {
    async fn connect(url: &str) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        Self { ws }
    }

    async fn send(&mut self, v: Value) {
        self.ws
            .send(tokio_tungstenite::tungstenite::Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    #[allow(dead_code)]
    async fn recv(&mut self) -> Value {
        let msg = tokio::time::timeout(Duration::from_secs(10), self.ws.next())
            .await
            .expect("timeout waiting for frame")
            .expect("stream ended")
            .expect("ws error");
        serde_json::from_str(msg.into_text().unwrap().as_str()).unwrap()
    }

    /// Read frames until one matches; returns it. Panics after 10 s.
    async fn until(&mut self, pred: impl Fn(&Value) -> bool + std::marker::Copy) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, self.ws.next())
                .await
                .expect("timeout waiting for matching frame")
                .expect("stream ended")
                .expect("ws error");
            let v: Value = serde_json::from_str(msg.into_text().unwrap().as_str()).unwrap();
            if pred(&v) {
                return v;
            }
        }
    }
}

async fn hello_handshake(client: &mut WsClient, token: &str) -> Value {
    client
        .send(json!({"type": "hello", "protocol": 1, "token": token,
                     "client": {"name": "test", "version": "0", "caps": []}}))
        .await;
    client.until(|v| v["type"] == "welcome").await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_rejects_bad_token() {
    let (_fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    client
        .send(json!({"type": "hello", "protocol": 1, "token": "wrong",
                     "client": {"name": "test"}}))
        .await;
    let v = client.until(|v| v["type"] == "response").await;
    assert_eq!(v["error"]["code"], "hub.auth_failed");

    let mut client = WsClient::connect(&url).await;
    let welcome = hello_handshake(&mut client, &token).await;
    assert_eq!(welcome["protocol"], 1);
    assert_eq!(welcome["servers"][0]["id"], "fake");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_gap_folds_buffered_events() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    // Arm the snapshot gate BEFORE the hub spawns, so the bootstrap's
    // snapshot call blocks and events land while the hub is buffering.
    let fake = FakeHerdr::start(base_snapshot());
    let release = fake.hold_snapshots();
    let (url, token) = spawn_hub_against(&fake).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;

    // Wait for the hub's subscribe, then emit an event mid-bootstrap, then
    // let the snapshot through.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fake.subscribe_sets().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "hub never subscribed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fake.emit(
        json!({"event": "workspace_created", "data": {"type": "workspace_created",
            "workspace": {"workspace_id": "w2", "number": 2, "label": "Mid-bootstrap",
                          "focused": false, "pane_count": 0, "tab_count": 0,
                          "active_tab_id": null, "agent_status": "unknown"}}})
        .to_string(),
    );
    // Give the event time to land in the hub's bootstrap buffer before the
    // snapshot reply is released (favors the fold path; scheduling may still
    // deliver it just after the drain, which the assertion below tolerates).
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = release.send(());

    // No bootstrap gap: the workspace created during the buffering window
    // must reach the client — either folded into the snapshot or as the very
    // next streamed event. Never lost.
    let snap = client.until(|v| v["type"] == "snapshot").await;
    let labels: Vec<&str> = snap["state"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"Base"), "base missing: {labels:?}");
    if !labels.contains(&"Mid-bootstrap") {
        // Fold path lost the race: the event must arrive right after instead.
        let ev = client
            .until(|v| v["type"] == "event" && v["event"] == "workspace_created")
            .await;
        assert_eq!(ev["data"]["workspace"]["workspace_id"], "w2");
        assert_eq!(ev["data"]["workspace"]["label"], "Mid-bootstrap");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_stream_and_resubscribe_on_pane_churn() {
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    // Streamed lifecycle events pass through verbatim (underscore kinds).
    fake.emit(
        json!({"event": "workspace_renamed", "data": {"type": "workspace_renamed",
            "workspace_id": "w1", "label": "Renamed"}})
        .to_string(),
    );
    let ev = client.until(|v| v["type"] == "event").await;
    assert_eq!(ev["event"], "workspace_renamed");
    assert_eq!(ev["server"], "fake");
    assert_eq!(ev["data"]["label"], "Renamed");

    // Pane churn: a created pane must trigger a new subscription set that
    // includes a per-pane agent_status_changed subscription for it.
    fake.add_pane(json!({"pane_id": "w1:p2", "terminal_id": "term_2", "workspace_id": "w1",
                         "tab_id": "w1:t1", "focused": false, "agent_status": "unknown",
                         "revision": 1, "agent": "coder"}));
    fake.emit(
        json!({"event": "pane_created", "data": {"type": "pane_created", "pane": {
            "pane_id": "w1:p2", "terminal_id": "term_2", "workspace_id": "w1",
            "tab_id": "w1:t1", "focused": false, "agent_status": "unknown",
            "revision": 1, "agent": "coder"}}})
        .to_string(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let sets = fake.subscribe_sets();
        let swapped = sets.iter().any(|s| s.contains(&"pane.agent_status_changed:w1:p2".to_string()));
        if swapped {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no resubscribe with the new pane: {sets:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The new per-pane subscription delivers the pane-scoped (dotted) event.
    fake.emit(
        json!({"event": "pane.agent_status_changed", "data": {"pane_id": "w1:p2",
            "workspace_id": "w1", "agent_status": "blocked"}})
        .to_string(),
    );
    let ev = client
        .until(|v| v["type"] == "event" && v["event"] == "pane_agent_status_changed")
        .await;
    assert_eq!(ev["data"]["agent_status"], "blocked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_dims_then_resyncs() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    // Kill the subscribe connections; the hub must report reconnecting.
    fake.kill_subscribers();
    let status = client
        .until(|v| v["type"] == "server_status" && v["status"] != "online")
        .await;
    assert_eq!(status["server"], "fake");

    // The hub retries and re-bootstraps; client gets a fresh snapshot.
    let snap = client.until(|v| v["type"] == "snapshot").await;
    assert!(snap["state"]["workspaces"].as_array().unwrap().len() >= 1);
}

/// Regression (seen live): a pane that vanished server-side without a close
/// event stays in the hub's retained state; on reconnect every subscribe —
/// and then every bootstrap — names it and herdr rejects the whole set
/// (`pane_not_found`), poisoning the connection forever. The hub must drop
/// the stale pane and come back online.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_pane_does_not_poison_reconnect() {
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    let snap1 = client.until(|v| v["type"] == "snapshot").await;
    assert_eq!(snap1["state"]["panes"].as_array().unwrap().len(), 1);

    // The pane vanishes server-side with no close event, then the
    // subscription connection dies.
    fake.drop_pane_from_snapshot("w1:p1");
    fake.kill_subscribers();
    client
        .until(|v| v["type"] == "server_status" && v["status"] != "online")
        .await;

    // Reconnect: the first subscribe names the stale pane and is rejected;
    // the hub drops it and re-bootstraps with the healed (empty) snapshot.
    let snap2 = client
        .until(|v| v["type"] == "snapshot" && v["state"]["panes"].as_array().unwrap().is_empty())
        .await;
    assert_eq!(snap2["state"]["panes"].as_array().unwrap().len(), 0);
    let status = client
        .until(|v| v["type"] == "server_status" && v["status"] == "online")
        .await;
    assert_eq!(status["server"], "fake");
    assert!(
        fake.dead_pane_rejects().iter().any(|p| p == "w1:p1"),
        "expected a pane_not_found reject for w1:p1, got {:?}",
        fake.dead_pane_rejects()
    );

    // The zombie stream (verified live on 0.8.2: herdr keeps probing panes of
    // closed workspaces) must not resurrect the dropped pane — no event to
    // clients, no further subscribe rejections.
    let rejects_before = fake.dead_pane_rejects().len();
    for _ in 0..3 {
        fake.emit(
            json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": {
                "pane_id": "w1:p1", "terminal_id": "term_1", "workspace_id": "w1",
                "tab_id": "w1:t1", "focused": true, "agent_status": "idle",
                "revision": 42, "agent": null}}})
            .to_string(),
        );
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let zombie = client
        .until(|v| v["type"] == "event" && v["event"] == "pane_updated")
        .await;
    // The hub forwards the raw event to clients, but its own state must stay
    // clean: no new pane_not_found rejections follow.
    assert_eq!(zombie["data"]["pane"]["revision"], 42);
    assert_eq!(
        fake.dead_pane_rejects().len(),
        rejects_before,
        "zombie pane_updated resurrected the pane in hub state"
    );
}

/// herdr can drop close events under churn (verified live on 0.8.2): the
/// workspace/pane vanishes server-side with no event ever arriving. The
/// periodic snapshot reconcile must heal hub state — and broadcast a fresh
/// snapshot so client trees heal too — without dropping the subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_reconcile_heals_missed_closes() {
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    // The server forgets w1 (and its pane) silently — no close events.
    fake.drop_pane_from_snapshot("w1:p1");
    fake.inner.lock().unwrap()
        .snapshot["workspaces"]
        .as_array_mut()
        .unwrap()
        .clear();
    fake.inner.lock().unwrap()
        .snapshot["tabs"]
        .as_array_mut()
        .unwrap()
        .clear();

    // Within a reconcile interval the hub installs the healed snapshot.
    let snap = client
        .until(|v| v["type"] == "snapshot" && v["state"]["panes"].as_array().unwrap().is_empty())
        .await;
    assert_eq!(snap["state"]["workspaces"].as_array().unwrap().len(), 0);

    // The subscription connection survived: new events still stream.
    fake.emit(
        json!({"event": "workspace_created", "data": {"type": "workspace_created", "workspace": {
            "workspace_id": "w9", "number": 9, "label": "Back", "focused": true,
            "pane_count": 0, "tab_count": 0, "active_tab_id": null,
            "agent_status": "idle"}}})
        .to_string(),
    );
    let ev = client
        .until(|v| v["type"] == "event" && v["event"] == "workspace_created")
        .await;
    assert_eq!(ev["data"]["workspace"]["workspace_id"], "w9");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_drives_screen_polling() {
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    // Declare a view; a full screen frame for that pane must arrive.
    client
        .send(json!({"type": "view", "server": "fake",
                     "active": {"workspace": "w1", "tab": "w1:t1"},
                     "panes": [{"pane": "w1:p1", "focused": true}],
                     "scrollback": []}))
        .await;
    let first = client.until(|v| v["type"] == "screen").await;
    assert_eq!(first["pane"], "w1:p1");
    assert_eq!(first["mode"], "full");
    assert_eq!(first["source"], "visible");
    let first_rev = first["revision"].as_u64().unwrap();

    // Content change → a follow-up frame with the new revision.
    fake.set_text("line one\nline CHANGED\n");
    let second = client
        .until(|v| v["type"] == "screen" && v["revision"].as_u64() > Some(first_rev))
        .await;
    let changed = second["changes"].as_array().unwrap();
    assert!(changed.iter().any(|c| c["text"].as_str().unwrap().contains("CHANGED")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrollback_agent_panes_read_visible_not_recent() {
    // herdr captures alternate-screen history by scrolling an idle agent
    // pane's app viewport (wheel-event harvest + restore, SPEC §3.5): a
    // `recent` read on an idle agent pane visibly scrolls the user's TUI
    // up and down on every poll. The scrollback poller must read agent
    // panes via `visible` and keep real `recent` scrollback for plain
    // panes.
    let mut base = base_snapshot();
    base["panes"][0]["agent"] = json!("coder");
    base["panes"].as_array_mut().unwrap().push(json!({
        "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": false, "agent_status": "idle", "revision": 1, "agent": null
    }));
    let (fake, url, token) = spawn_hub_with_fake(base).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    client
        .send(json!({"type": "view", "server": "fake",
                     "active": {"workspace": "w1", "tab": "w1:t1"},
                     "panes": [],
                     "scrollback": ["w1:p1", "w1:p2"]}))
        .await;

    // Wait until both panes have been scrollback-polled at least once
    // (the scrollback cadence is every 10th poll round).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let reads = fake.pane_reads();
        let done = reads.iter().any(|(p, _)| p == "w1:p1")
            && reads.iter().any(|(p, _)| p == "w1:p2");
        if done || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Agent pane: `visible` ansi with no lines cap → no wheel dance, and
    // colors for the transcript renderer.
    let p1: Vec<_> = fake
        .pane_reads()
        .into_iter()
        .filter(|(p, _)| p == "w1:p1")
        .collect();
    assert!(!p1.is_empty(), "agent pane was never scrollback-polled");
    assert!(
        p1.iter().all(|(_, params)| params["source"] == "visible"
            && params["format"] == "ansi"),
        "agent pane reads must use source=visible format=ansi, got {p1:?}"
    );
    assert!(p1.iter().all(|(_, params)| params["lines"].is_null()));

    // Plain pane: real `recent` scrollback with the 500-line window.
    let p2: Vec<_> = fake
        .pane_reads()
        .into_iter()
        .filter(|(p, _)| p == "w1:p2")
        .collect();
    assert!(!p2.is_empty(), "plain pane was never scrollback-polled");
    assert!(
        p2.iter()
            .all(|(_, params)| params["source"] == "recent" && params["lines"] == 500),
        "plain pane reads must use source=recent lines=500, got {p2:?}"
    );

    // The client-facing frame keeps the `recent` channel (the web
    // transcript subscribes to "recent" and diff-treats any window).
    let frame = client
        .until(|v| v["type"] == "screen" && v["pane"] == "w1:p1" && v["source"] == "recent")
        .await;
    assert_eq!(frame["mode"], "full");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_proxy_through_with_herdr_errors() {
    let (_fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;
    client.until(|v| v["type"] == "snapshot").await;

    // Allowed action → ok result (fake returns {"type":"ok"}).
    client
        .send(json!({"type": "request", "id": "r1", "server": "fake",
                     "action": "tab.focus", "params": {"tab_id": "w1:t1"}}))
        .await;
    let resp = client.until(|v| v["type"] == "response" && v["id"] == "r1").await;
    assert_eq!(resp["ok"], true);

    // Forbidden action → hub.action_not_allowed.
    client
        .send(json!({"type": "request", "id": "r2", "server": "fake",
                     "action": "server.stop", "params": {}}))
        .await;
    let resp = client.until(|v| v["type"] == "response" && v["id"] == "r2").await;
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "hub.action_not_allowed");

    // Unknown server → hub.unknown_server.
    client
        .send(json!({"type": "request", "id": "r3", "server": "nope",
                     "action": "tab.focus", "params": {}}))
        .await;
    let resp = client.until(|v| v["type"] == "response" && v["id"] == "r3").await;
    assert_eq!(resp["error"]["code"], "hub.unknown_server");
}

#[tokio::test]
async fn second_prompt_to_same_target_rejected_while_in_flight() {
    let (fake, url, token) = spawn_hub_with_fake(base_snapshot()).await;
    let mut a = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut a, &token).await;
    a.until(|v| v["type"] == "snapshot").await;
    // A second client (phone + desktop): requests are sequential per
    // connection, so the guard's job is cross-client.
    let mut b = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut b, &token).await;
    b.until(|v| v["type"] == "snapshot").await;

    // Hold client A's prompt inside herdr; the hub must refuse client B's
    // prompt to the SAME target rather than queue it (SPEC §7: never
    // auto-retry — a queued retry could double-send after a timeout).
    let release = fake.hold_prompts();
    a.send(json!({"type": "request", "id": "p1", "server": "fake",
                  "action": "agent.prompt", "params": {"target": "w1:p1", "text": "first"}}))
        .await;
    b.send(json!({"type": "request", "id": "p2", "server": "fake",
                  "action": "agent.prompt", "params": {"target": "w1:p1", "text": "second"}}))
        .await;
    let resp = b.until(|v| v["type"] == "response" && v["id"] == "p2").await;
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "hub.prompt_in_flight");

    // Release: the first prompt completes normally.
    let _ = release.send(());
    let resp = a.until(|v| v["type"] == "response" && v["id"] == "p1").await;
    assert_eq!(resp["ok"], true);

    // The guard is per-prompt, not sticky: the next prompt goes through.
    b.send(json!({"type": "request", "id": "p3", "server": "fake",
                  "action": "agent.prompt", "params": {"target": "w1:p1", "text": "third"}}))
        .await;
    let resp = b.until(|v| v["type"] == "response" && v["id"] == "p3").await;
    assert_eq!(resp["ok"], true);
}

/// herdr < 0.9.0 (protocol < 22) replays its retained event history on
/// every new subscription connection. The hub must therefore never
/// subscribe on such servers (poll mode): the tree, focus, and agent
/// status come from the fast snapshot reconcile instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poll_mode_never_subscribes_and_reconciles() {
    let fake = FakeHerdr::start(base_snapshot());
    fake.set_protocol(20);
    let (url, token) = spawn_hub_against(&fake).await;
    let mut client = WsClient::connect(&url).await;
    let _ = hello_handshake(&mut client, &token).await;

    // Bootstrap snapshot arrives (via the reconcile path — no reader).
    client
        .until(|v| v["type"] == "snapshot" && v["server"] == "fake")
        .await;

    // No subscription may EVER be opened: watch longer than the drift
    // debounce so a latent subscribe would have fired.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        fake.subscribe_sets().is_empty(),
        "poll mode must not subscribe, got {:?}",
        fake.subscribe_sets()
    );

    // A server-side change with NO event emitted is still picked up by the
    // 2s snapshot reconcile and pushed to clients as a fresh snapshot.
    fake.inner.lock().unwrap().snapshot["workspaces"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "workspace_id": "w2", "number": 2, "label": "Silent", "focused": false,
            "pane_count": 0, "tab_count": 0, "active_tab_id": null, "agent_status": "idle"
        }));
    let snap = client
        .until(|v| {
            v["type"] == "snapshot"
                && v["state"]["workspaces"]
                    .as_array()
                    .map(|a| a.iter().any(|w| w["workspace_id"] == "w2"))
                    .unwrap_or(false)
        })
        .await;
    assert_eq!(snap["server"], "fake");

    // Still no subscriptions after the reconcile loop ran.
    assert!(fake.subscribe_sets().is_empty());
}
