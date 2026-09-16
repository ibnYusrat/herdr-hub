//! NDJSON client for the herdr public JSON API.

pub mod transport;
pub mod types;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

pub use transport::{LineStream, LocalTransport, SshTransport, Transport, TransportKind};
pub use types::*;

/// Build the transport for one configured server entry.
pub fn transport_for(entry: &crate::config::ServerEntry) -> Arc<dyn Transport> {
    use crate::config::ServerKind;
    match &entry.kind {
        ServerKind::Local { socket, session } => {
            let path = match (socket, session) {
                (Some(p), _) => p.clone(),
                (None, Some(s)) => crate::config::herdr_socket_for(Some(s)),
                (None, None) => LocalTransport::default_socket(),
            };
            Arc::new(LocalTransport::new(path))
        }
        ServerKind::Ssh {
            host,
            session,
            identity,
            transport: mode,
            socket,
        } => Arc::new(SshTransport::with_mode(
            host.clone(),
            session.clone(),
            identity.clone(),
            crate::config::data_dir().join("ssh-control"),
            mode.unwrap_or_default(),
            socket.clone(),
        )),
    }
}


/// Errors from the herdr wire, split into herdr-reported errors (code+message
/// pass through to clients) and transport failures (hub reconnects).
#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("{0}: {1}")]
    Herdr(String, String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
}

impl HerdrError {
    pub fn herdr_code(&self) -> Option<&str> {
        match self {
            HerdrError::Herdr(code, _) => Some(code),
            _ => None,
        }
    }
}

/// Client for one herdr server. Every request opens a fresh connection
/// (herdr closes after one reply); concurrency is bounded by a semaphore of
/// "request lanes" rather than a connection pool. The one long-lived
/// connection type is `subscribe()`.
pub struct HerdrClient {
    transport: Arc<dyn Transport>,
    lanes: Arc<Semaphore>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl HerdrClient {
    pub fn new(transport: Arc<dyn Transport>, lanes: usize) -> Self {
        Self {
            transport,
            lanes: Arc::new(Semaphore::new(lanes)),
            next_id: AtomicU64::new(1),
            timeout: Duration::from_secs(20),
        }
    }

    pub fn transport_kind(&self) -> TransportKind {
        self.transport.kind()
    }

    fn alloc_id(&self) -> String {
        format!("hub{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// One-shot request on a fresh connection.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        let _lane = self
            .lanes
            .acquire()
            .await
            .map_err(|_| HerdrError::Transport("lane semaphore closed".into()))?;
        let mut stream = self
            .transport
            .open()
            .await
            .map_err(|e| HerdrError::Transport(e.to_string()))?;
        let id = self.alloc_id();
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        tokio::time::timeout(self.timeout, async {
            stream
                .send_line(&line)
                .await
                .map_err(|e| HerdrError::Transport(e.to_string()))?;
            let resp = stream
                .recv_line()
                .await
                .map_err(|e| HerdrError::Transport(e.to_string()))?
                .ok_or_else(|| HerdrError::Transport("connection closed before reply".into()))?;
            stream.close().await;
            let parsed: SocketLine = serde_json::from_str(&resp)
                .map_err(|e| HerdrError::Decode(format!("{e}: {resp:.200}")))?;
            match parsed {
                SocketLine::Success(s) => Ok(s.result),
                SocketLine::Error(e) => Err(HerdrError::Herdr(e.error.code, e.error.message)),
                SocketLine::Event(_) => Err(HerdrError::Decode(
                    "unexpected event envelope on request connection".into(),
                )),
            }
        })
        .await
        .map_err(|_| HerdrError::Transport(format!("timeout calling {method}")))?
    }

    /// Open the long-lived subscription connection. Waits for the
    /// `subscription_started` ack; the returned reader yields pushed events.
    pub async fn subscribe(
        &self,
        subs: Vec<Subscription>,
    ) -> Result<SubscriptionReader, HerdrError> {
        let mut stream = self
            .transport
            .open()
            .await
            .map_err(|e| HerdrError::Transport(e.to_string()))?;
        let id = self.alloc_id();
        let params = json!({"subscriptions": subs});
        let line = json!({"id": id, "method": "events.subscribe", "params": params}).to_string();
        stream
            .send_line(&line)
            .await
            .map_err(|e| HerdrError::Transport(e.to_string()))?;
        let ack = tokio::time::timeout(self.timeout, async {
            stream
                .recv_line()
                .await
                .map_err(|e| HerdrError::Transport(e.to_string()))?
                .ok_or_else(|| HerdrError::Transport("closed before subscribe ack".into()))
        })
        .await
        .map_err(|_| HerdrError::Transport("timeout waiting for subscribe ack".into()))??;
        let parsed: SocketLine = serde_json::from_str(&ack)
            .map_err(|e| HerdrError::Decode(format!("{e}: {ack:.200}")))?;
        match parsed {
            SocketLine::Success(s) if result_tag(&s.result) == Some("subscription_started") => {
                Ok(SubscriptionReader { stream })
            }
            SocketLine::Error(e) => Err(HerdrError::Herdr(e.error.code, e.error.message)),
            other => Err(HerdrError::Decode(format!(
                "unexpected subscribe ack: {other:.200?}"
            ))),
        }
    }

    pub async fn ping(&self) -> Result<(String, u32), HerdrError> {
        let result = self.request("ping", json!({})).await?;
        let version = result
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let protocol = result.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        Ok((version, protocol))
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, HerdrError> {
        let result = self.request("session.snapshot", json!({})).await?;
        let snap: SessionSnapshot = unwrap_result(&result, "session_snapshot", "snapshot")
            .map_err(|e| HerdrError::Decode(e.to_string()))?;
        Ok(snap)
    }

    pub async fn pane_read(
        &self,
        pane_id: &str,
        source: &str,
        format: &str,
        lines: Option<u32>,
    ) -> Result<PaneReadResult, HerdrError> {
        let mut params = json!({"pane_id": pane_id, "source": source, "format": format});
        if let Some(n) = lines {
            params["lines"] = json!(n);
        }
        let result = self.request("pane.read", params).await?;
        let read: PaneReadResult = unwrap_result(&result, "pane_read", "read")
            .map_err(|e| HerdrError::Decode(e.to_string()))?;
        Ok(read)
    }
}

/// Reader for one live subscription connection. Dropping it closes the socket.
pub struct SubscriptionReader {
    stream: Box<dyn LineStream>,
}

impl SubscriptionReader {
    /// Next pushed event, or `Err`/`None` when the connection died or was closed.
    pub async fn next_event(&mut self) -> Result<Option<EventEnvelope>, HerdrError> {
        loop {
            let line = match self.stream.recv_line().await {
                Ok(Some(line)) => line,
                Ok(None) => return Ok(None),
                Err(e) => return Err(HerdrError::Transport(e.to_string())),
            };
            if !line.trim().is_empty() {
                return serde_json::from_str::<EventEnvelope>(&line)
                    .map(Some)
                    .map_err(|e| HerdrError::Decode(format!("{e}: {line:.200}")));
            }
        }
    }

    pub async fn close(&mut self) {
        self.stream.close().await;
    }
}

/// Protocol gate per SPEC §12: check at connect and warn on mismatch. The hub
/// is built against protocol 22 (herdr 0.9.0 schema); the running server may
/// be older — 0.8.2 speaks protocol 20 and is a strict method subset.
pub fn protocol_check(version: &str, protocol: u32) -> Option<String> {
    let built_against: u32 = 22;
    if protocol > built_against {
        Some(format!(
            "herdr {version} speaks protocol {protocol}, newer than the pinned {built_against}; events may be missed"
        ))
    } else if protocol < 20 {
        Some(format!(
            "herdr {version} speaks protocol {protocol}, older than any tested version (20)"
        ))
    } else {
        None
    }
}

