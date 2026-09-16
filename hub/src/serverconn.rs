//! Per-server supervisor actor: owns the canonical `ServerState`, runs the
//! gap-free bootstrap recipe, keeps the subscription set current as panes
//! come and go, and reconnects with backoff.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{info, warn};

use crate::clients::Registry;
use crate::hubproto::{self, HubFrame};
use crate::state::{
    apply_event, desired_subscriptions, subscription_set_hash, InterestMap, ServerState,
    ServerStatus,
};
use crate::wire::{protocol_check, EventEnvelope, HerdrClient, HerdrError, Subscription,
    SubscriptionReader};

/// Control messages into the server actor.
pub enum ServerMsg {
    Event(EventEnvelope),
    ReaderDied { generation: u64, reason: String },
    GetState(oneshot::Sender<Arc<ServerState>>),
}

pub struct ServerHandle {
    pub id: String,
    pub label: String,
    pub tx: mpsc::Sender<ServerMsg>,
    pub shared: Arc<ArcSwap<ServerState>>,
}

impl ServerHandle {
    /// Current canonical state (last published by the actor).
    pub async fn current_state(&self) -> Option<Arc<ServerState>> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(ServerMsg::GetState(tx)).await.ok()?;
        rx.await.ok()
    }

    pub fn peek_state(&self) -> Arc<ServerState> {
        self.shared.load_full()
    }
}

/// Spawn everything for one configured server: the actor and the screen
/// poller.
pub fn spawn_server(
    id: String,
    label: String,
    client: Arc<HerdrClient>,
    poll_ms: u64,
    reconcile_secs: u64,
    broadcast: broadcast::Sender<HubFrame>,
    interest_rx: watch::Receiver<InterestMap>,
    registry: Arc<Registry>,
) -> ServerHandle {
    let (tx, rx) = mpsc::channel(1024);
    let shared = Arc::new(ArcSwap::from_pointee(ServerState::new()));
    let (hint_tx, hint_rx) = mpsc::channel(256);

    tokio::spawn(actor(
        ActorContext {
            id: id.clone(),
            tx: tx.clone(),
            client: client.clone(),
            broadcast,
            hint_tx,
            reconcile_secs,
        },
        shared.clone(),
        rx,
    ));

    crate::screen::spawn_poller(
        id.clone(),
        client,
        shared.clone(),
        interest_rx,
        hint_rx,
        registry,
        poll_ms,
    );

    ServerHandle {
        id,
        label,
        tx,
        shared,
    }
}

struct ActorContext {
    id: String,
    tx: mpsc::Sender<ServerMsg>,
    client: Arc<HerdrClient>,
    broadcast: broadcast::Sender<HubFrame>,
    hint_tx: mpsc::Sender<(String, u64)>,
    reconcile_secs: u64,
}

async fn actor(ctx: ActorContext, shared: Arc<ArcSwap<ServerState>>, mut rx: mpsc::Receiver<ServerMsg>) {
    let mut run = ServerRun {
        ctx,
        shared,
        local: ServerState::new(),
        reader_generation: 0,
        kill_current: None,
        current_sub_hash: 0,
        drift_pending: false,
        backoff: Duration::from_millis(250),
        buffer: Vec::new(),
        stream_events: true,
    };
    info!(server = %run.ctx.id, "server actor started");
    run.main_loop(&mut rx).await;
}

struct ServerRun {
    ctx: ActorContext,
    shared: Arc<ArcSwap<ServerState>>,
    /// Actor-owned canonical state; `shared` receives a copy on every change.
    local: ServerState,
    reader_generation: u64,
    kill_current: Option<mpsc::Sender<()>>,
    current_sub_hash: u64,
    drift_pending: bool,
    backoff: Duration,
    buffer: Vec<EventEnvelope>,
    /// Streaming subscriptions are only used on herdr ≥ 0.9.0 (protocol
    /// 22). Older servers replay their whole retained event history on
    /// every new subscription connection (one match per ~100 ms tick,
    /// minutes of stale flood on a long-lived server), so below 22 the
    /// actor runs in poll mode: no event subscription, the snapshot
    /// reconciler is authoritative. Set by `bootstrap` from the ping.
    stream_events: bool,
}

impl ServerRun {
    async fn main_loop(&mut self, rx: &mut mpsc::Receiver<ServerMsg>) {
        loop {
            match self.bootstrap(rx).await {
                Ok(()) => {}
                Err(e) => {
                    warn!(server = %self.ctx.id, error = %e, "bootstrap failed");
                    self.set_status(ServerStatus::Reconnecting).await;
                    self.sleep_backoff().await;
                    continue;
                }
            }
            self.backoff = Duration::from_millis(250);

            let drift_timer = tokio::time::sleep(Duration::from_secs(3600));
            tokio::pin!(drift_timer);
            // Bootstrap subscribed before the snapshot was installed, so the
            // initial set lacks per-pane subscriptions — arm the debounce now.
            if self.drift_pending {
                drift_timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + Duration::from_millis(250));
            }
            // Poll mode (herdr < 0.9.0): reconcile is the only change
            // signal, so it runs fast. Streaming mode keeps the lazier
            // drift-bounding cadence.
            let reconcile_every = Duration::from_secs(if self.stream_events {
                self.ctx.reconcile_secs.max(1)
            } else {
                2
            });
            let reconcile_timer = tokio::time::sleep(reconcile_every);
            tokio::pin!(reconcile_timer);
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(ServerMsg::Event(ev)) => {
                                let out = apply_event(&mut self.local, &ev);
                                self.after_event(&ev, out).await;
                                if self.drift_pending {
                                    drift_timer.as_mut().reset(
                                        tokio::time::Instant::now() + Duration::from_millis(250),
                                    );
                                }
                            }
                            Some(ServerMsg::ReaderDied { generation, reason }) => {
                                if generation == self.reader_generation {
                                    warn!(server = %self.ctx.id, %reason, "subscription connection lost");
                                    break;
                                }
                            }
                            Some(ServerMsg::GetState(reply)) => {
                                let _ = reply.send(Arc::new(self.local.clone()));
                            }
                            None => return,
                        }
                    }
                    _ = &mut drift_timer, if self.drift_pending && self.stream_events => {
                        self.drift_pending = false;
                        if let Err(e) = self.refresh_subscriptions().await {
                            warn!(server = %self.ctx.id, error = %e, "subscription refresh failed");
                            break;
                        }
                        if self.drift_pending {
                            drift_timer.as_mut().reset(
                                tokio::time::Instant::now() + Duration::from_millis(250),
                            );
                        }
                    }
                    _ = &mut reconcile_timer => {
                        reconcile_timer
                            .as_mut()
                            .reset(tokio::time::Instant::now() + reconcile_every);
                        if let Err(e) = self.reconcile(rx).await {
                            warn!(server = %self.ctx.id, error = %e, "reconcile failed");
                        }
                        if self.drift_pending {
                            drift_timer.as_mut().reset(
                                tokio::time::Instant::now() + Duration::from_millis(250),
                            );
                        }
                    }
                }
            }

            self.set_status(ServerStatus::Reconnecting).await;
            self.sleep_backoff().await;
        }
    }

    /// Periodic snapshot reconciliation. herdr (verified live on 0.8.2)
    /// sometimes drops close events under churn and emits lifecycle events
    /// causally inverted, so event-only state drifts. Every
    /// `reconcile_secs`, fetch the authoritative snapshot on a request lane
    /// and install it when it differs — the same buffer-then-fold pattern as
    /// the bootstrap, bounding drift no matter what was missed. Events keep
    /// flowing on the subscription connection the whole time.
    async fn reconcile(&mut self, rx: &mut mpsc::Receiver<ServerMsg>) -> anyhow::Result<()> {
        self.buffer.clear();
        let snapshot = loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(ServerMsg::Event(ev)) => {
                        if self.buffer.len() < 10_000 {
                            self.buffer.push(ev);
                        }
                    }
                    Some(ServerMsg::ReaderDied { generation: g, reason }) => {
                        if g == self.reader_generation {
                            anyhow::bail!("reader died during reconcile: {reason}");
                        }
                    }
                    Some(ServerMsg::GetState(reply)) => {
                        let _ = reply.send(Arc::new(self.local.clone()));
                    }
                    None => anyhow::bail!("hub shutting down"),
                },
                snap = self.ctx.client.snapshot() => break snap?,
            }
        };
        if self.local.matches_snapshot(&snapshot) {
            self.buffer.clear();
            return Ok(());
        }
        self.local.install_snapshot(&snapshot);
        let buffered: Vec<EventEnvelope> = std::mem::take(&mut self.buffer);
        for ev in buffered {
            let _ = apply_event(&mut self.local, &ev);
        }
        self.publish();
        let _ = self
            .ctx
            .broadcast
            .send(hubproto::snapshot_frame(&self.ctx.id, &self.local));
        // The pane set may have changed (stale panes dropped, missed ones
        // added) — recompute the subscription set. Streaming only: poll
        // mode has no subscription to drift.
        if self.stream_events {
            self.drift_pending = true;
        }
        info!(
            server = %self.ctx.id,
            generation = self.local.generation,
            "reconciled state from snapshot"
        );
        Ok(())
    }

    async fn after_event(&mut self, ev: &EventEnvelope, out: crate::state::Applied) {
        if out.changed || out.pane_set_changed {
            self.publish();
        }
        if out.pane_set_changed {
            self.drift_pending = true;
        }
        for (pane, rev) in out.revision_hints {
            let _ = self.ctx.hint_tx.try_send((pane, rev));
        }
        let _ = self.ctx.broadcast.send(HubFrame::Event {
            server: self.ctx.id.clone(),
            event: ev.kind(),
            data: ev.data.clone(),
        });
    }

    fn publish(&mut self) {
        self.shared.store(Arc::new(self.local.clone()));
    }

    async fn set_status(&mut self, status: ServerStatus) {
        self.local.set_status(status.clone());
        self.publish();
        let detail = status_detail(&status);
        let _ = self.ctx.broadcast.send(HubFrame::ServerStatus {
            server: self.ctx.id.clone(),
            status: status.label().to_string(),
            detail,
        });
    }

    async fn sleep_backoff(&mut self) {
        let max_jitter = self.backoff.as_millis() as u64 / 4 + 1;
        let jitter = rand::Rng::random_range(&mut rand::rng(), 0..=max_jitter);
        tokio::time::sleep(self.backoff + Duration::from_millis(jitter)).await;
        self.backoff = std::cmp::min(self.backoff * 2, Duration::from_secs(10));
    }

    /// Gap-free bootstrap (SPEC §3.4): subscribe → ack → buffer → snapshot on
    /// a second connection → install → drain buffer → broadcast snapshot.
    async fn bootstrap(&mut self, rx: &mut mpsc::Receiver<ServerMsg>) -> anyhow::Result<()> {
        self.set_status(ServerStatus::Connecting).await;

        let (version, protocol) = self.ctx.client.ping().await?;
        if let Some(warnmsg) = protocol_check(&version, protocol) {
            warn!(server = %self.ctx.id, "{warnmsg}");
        }
        // herdr < 0.9.0 (protocol < 22) replays its retained event history
        // on every new subscription connection — one match per ~100 ms
        // server tick, so a long-lived server floods minutes of stale
        // events (focus flapping, phantom churn) into hub state and every
        // connected client. 0.9.0 clamps subscriptions to the current
        // sequence (verified in source). Below 22: poll mode — no event
        // subscription at all; the fast snapshot reconciler is the change
        // signal and stays authoritative.
        self.stream_events = protocol >= 22;
        if !self.stream_events {
            info!(
                server = %self.ctx.id, protocol,
                "herdr < 0.9.0: running in poll mode (no event subscriptions; 2s snapshot reconcile)"
            );
        }

        if self.stream_events {
            let (reader, subs) = self.subscribe_current().await?;
            self.current_sub_hash = subscription_set_hash(&subs);
            self.reader_generation += 1;
            let generation = self.reader_generation;
            self.buffer.clear();
            let (kill_tx, kill_rx) = mpsc::channel(1);
            self.kill_current = Some(kill_tx);
            spawn_reader(reader, self.ctx.tx.clone(), generation, kill_rx);
        }

        // Buffer events while the snapshot is fetched on another connection.
        let snapshot = loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(ServerMsg::Event(ev)) => {
                        if self.buffer.len() < 10_000 {
                            self.buffer.push(ev);
                        }
                    }
                    Some(ServerMsg::ReaderDied { generation: g, reason }) => {
                        if g == self.reader_generation {
                            anyhow::bail!("reader died during bootstrap: {reason}");
                        }
                    }
                    Some(ServerMsg::GetState(reply)) => {
                        let _ = reply.send(Arc::new(self.local.clone()));
                    }
                    None => anyhow::bail!("hub shutting down"),
                },
                snap = self.ctx.client.snapshot() => { break snap },
            }
        }?;

        // Install, then drain the buffer with upsert semantics (stale buffered
        // events are no-ops against the newer snapshot). Buffered events are
        // folded into hub state but not broadcast: clients are about to
        // receive the snapshot, which already reflects them.
        self.local
            .set_status(ServerStatus::Online {
                version: version.clone(),
                protocol,
            });
        self.local.install_snapshot(&snapshot);
        let buffered: Vec<EventEnvelope> = std::mem::take(&mut self.buffer);
        tracing::debug!(
            server = %self.ctx.id,
            buffered = buffered.len(),
            "draining bootstrap buffer"
        );
        for ev in buffered {
            let _ = apply_event(&mut self.local, &ev);
        }
        self.publish();
        let _ = self.ctx.broadcast.send(hubproto::snapshot_frame(
            &self.ctx.id,
            &self.local,
        ));
        self.set_status(ServerStatus::Online { version, protocol }).await;
        self.drift_pending = true;
        info!(server = %self.ctx.id, generation = self.local.generation, "bootstrap complete");
        Ok(())
    }

    /// Swap to a new subscription connection when the desired set drifted
    /// (panes created/closed, or scrollback interest changed). The overlap
    /// window is safe: both connections' events apply with upsert semantics,
    /// so duplicates are no-ops.
    async fn refresh_subscriptions(&mut self) -> anyhow::Result<()> {
        if !self.stream_events {
            return Ok(());
        }
        let wanted = desired_subscriptions(&self.local);
        let hash = subscription_set_hash(&wanted);
        if hash == self.current_sub_hash {
            return Ok(());
        }

        let (reader, subs) = self.subscribe_current().await?;

        self.reader_generation += 1;
        let generation = self.reader_generation;
        self.current_sub_hash = subscription_set_hash(&subs);
        if let Some(kill) = self.kill_current.take() {
            let _ = kill.try_send(());
        }
        let (kill_tx, kill_rx) = mpsc::channel(1);
        self.kill_current = Some(kill_tx);
        spawn_reader(reader, self.ctx.tx.clone(), generation, kill_rx);
        Ok(())
    }

    /// Subscribe with the current desired set, healing around panes that died
    /// server-side. herdr fails the WHOLE subscribe when the set names a dead
    /// pane, so a pane that went away without our seeing the close event would
    /// otherwise poison every subscribe — and every reconnect bootstrap, since
    /// retained state keeps naming it (verified against the live server). On
    /// `pane_not_found` we drop the pane from state — it is provably gone —
    /// and retry without it; if the error doesn't name the pane, degrade to
    /// the filterless set for this attempt (lifecycle events still flow and
    /// the next drift refresh recomputes from healed state).
    async fn subscribe_current(&mut self) -> anyhow::Result<(SubscriptionReader, Vec<Subscription>)> {
        let mut dead: HashSet<String> = HashSet::new();
        loop {
            let mut subs = desired_subscriptions(&self.local);
            subs.retain(|s| s.pane_id.as_ref().map_or(true, |p| !dead.contains(p)));
            match self.ctx.client.subscribe(subs.clone()).await {
                Ok(reader) => return Ok((reader, subs)),
                Err(HerdrError::Herdr(code, msg)) if code == "pane_not_found" => {
                    match extract_dead_pane(&msg).filter(|p| !dead.contains(p)) {
                        Some(pane) => {
                            warn!(
                                server = %self.ctx.id, pane = %pane,
                                "pane gone server-side; dropping from state"
                            );
                            dead.insert(pane.clone());
                            self.drop_pane(&pane);
                        }
                        None => {
                            let filterless: Vec<Subscription> = subs
                                .into_iter()
                                .filter(|s| s.pane_id.is_none())
                                .collect();
                            let reader = self.ctx.client.subscribe(filterless.clone()).await?;
                            return Ok((reader, filterless));
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Remove a pane the server proved dead: apply a synthetic close event
    /// (upsert semantics make the real event, if it ever arrives, a no-op) and
    /// broadcast it so client trees drop it too.
    fn drop_pane(&mut self, pane_id: &str) {
        let synthetic = EventEnvelope {
            event: "pane_closed".into(),
            data: serde_json::json!({ "pane_id": pane_id }),
        };
        let out = apply_event(&mut self.local, &synthetic);
        if out.pane_set_changed {
            self.publish();
            let _ = self.ctx.broadcast.send(HubFrame::Event {
                server: self.ctx.id.clone(),
                event: synthetic.event,
                data: synthetic.data,
            });
        }
    }
}

fn status_detail(status: &ServerStatus) -> Option<String> {
    match status {
        ServerStatus::Online { version, protocol } => {
            Some(format!("herdr {version} (protocol {protocol})"))
        }
        _ => None,
    }
}

/// Parse the pane id out of herdr's `pane_not_found` message
/// (`"pane w2:p1 not found"` → `w2:p1`). Returns None for other phrasings —
/// the caller then degrades to a filterless subscribe instead of retrying.
fn extract_dead_pane(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("pane ")?;
    let id = rest.strip_suffix(" not found")?;
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::extract_dead_pane;

    #[test]
    fn extracts_dead_pane_id() {
        assert_eq!(extract_dead_pane("pane w2:p1 not found").as_deref(), Some("w2:p1"));
        assert_eq!(extract_dead_pane("pane gone"), None);
        assert_eq!(extract_dead_pane("no such pane: w1:p1"), None);
    }
}

fn spawn_reader(
    reader: SubscriptionReader,
    tx: mpsc::Sender<ServerMsg>,
    generation: u64,
    mut kill: mpsc::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut reader = reader;
        loop {
            tokio::select! {
                _ = kill.recv() => {
                    reader.close().await;
                    break;
                }
                ev = reader.next_event() => match ev {
                    Ok(Some(e)) => {
                        if tx.send(ServerMsg::Event(e)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(ServerMsg::ReaderDied {
                            generation,
                            reason: "connection closed".into(),
                        }).await;
                        break;
                    }
                    Err(e) => {
                        let _ = tx.send(ServerMsg::ReaderDied {
                            generation,
                            reason: e.to_string(),
                        }).await;
                        break;
                    }
                }
            }
        }
    });
}
