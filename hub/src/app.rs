//! HTTP + WebSocket server: serves the embedded web client, terminates the
//! hub protocol, checks Origins, optional TLS.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;
use tokio::sync::{broadcast, watch};

use crate::clients::{run_session, Registry, ServerHandleInfo, SessionCtx};
use crate::config::Config;
use crate::state::InterestMap;

#[derive(RustEmbed)]
#[folder = "web-dist"]
struct WebDist;

pub struct App {
    pub ctx: Arc<SessionCtx>,
    pub web_dir: Option<PathBuf>,
    pub origin_allow: Vec<String>,
}

pub fn build_app(config: &Config, ctx: Arc<SessionCtx>, web_dir: Option<PathBuf>) -> Router {
    let origin_allow = config.origin_allow.clone();
    let app_state = Arc::new(App {
        ctx,
        web_dir,
        origin_allow,
    });
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/", get(index_handler))
        .route("/{*path}", get(asset_handler))
        .with_state(app_state)
}

async fn ws_handler(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !origin_allowed(&headers, &host, &app.origin_allow) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ctx = app.ctx.clone();
    ws.on_upgrade(move |socket| run_session(socket, ctx))
}

/// Origin check on the WebSocket upgrade (SPEC §9). Non-browser clients send
/// no Origin and pass; browsers must be same-origin (or explicitly allowed).
fn origin_allowed(headers: &HeaderMap, host: &str, allow: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
        return true;
    };
    if allow.iter().any(|a| a == origin) {
        return true;
    }
    let Some((_, origin_hostport)) = origin.split_once("://") else {
        return false;
    };
    let origin_hostport = origin_hostport.trim_end_matches('/');
    if origin_hostport.eq_ignore_ascii_case(host) {
        return true;
    }
    // localhost / 127.0.0.1 / ::1 all name the same machine (vite dev server
    // on :5173 talking to the hub on :8787 is the normal case).
    let (oh, _op) = split_host_port(origin_hostport);
    let (hh, _hp) = split_host_port(host);
    is_loopback_host(oh) && is_loopback_host(hh)
}

fn split_host_port(s: &str) -> (&str, Option<&str>) {
    if let Some(rest) = s.strip_prefix('[') {
        // [::1]:8000
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':');
            return (host, port);
        }
    }
    match s.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (host, Some(port)),
        _ => (s, None),
    }
}

fn is_loopback_host(h: &str) -> bool {
    h == "localhost" || h == "127.0.0.1" || h == "::1"
}

async fn index_handler(State(app): State<Arc<App>>) -> Response {
    serve_asset(&app, "index.html").await
}

async fn asset_handler(State(app): State<Arc<App>>, axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    serve_asset(&app, &path).await
}

async fn serve_asset(app: &App, path: &str) -> Response {
    // Dev override: serve straight from disk (vite dev/build output).
    if let Some(dir) = &app.web_dir {
        let rel = path.trim_start_matches('/');
        let full = dir.join(rel);
        if full.is_file() {
            if let Ok(bytes) = tokio::fs::read(&full).await {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime_for(rel))],
                    bytes,
                )
                    .into_response();
            }
        }
        let index = dir.join("index.html");
        if index.is_file() {
            if let Ok(bytes) = tokio::fs::read(&index).await {
                return (StatusCode::OK, [(header::CONTENT_TYPE, "text/html")], bytes)
                    .into_response();
            }
        }
        return (StatusCode::NOT_FOUND, "web dist not built").into_response();
    }

    match WebDist::get(path.trim_start_matches('/')) {
        Some(asset) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime_for(path))],
            asset.data.into_owned(),
        )
            .into_response(),
        None => match WebDist::get("index.html") {
            Some(asset) => Html(asset.data.into_owned()).into_response(),
            None => (StatusCode::NOT_FOUND, "web client not built").into_response(),
        },
    }
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Everything the hub needs at runtime, assembled from config.
pub struct Hub {
    pub ctx: Arc<SessionCtx>,
}

/// Build and spawn all servers; return the shared session context.
pub fn spawn_hub(config: &Config, token: String) -> Hub {
    let (interest_tx, interest_rx) = watch::channel(InterestMap::default());
    let registry = Arc::new(Registry::new(interest_tx));
    let (broadcast_tx, _broadcast_rx) = broadcast::channel::<crate::hubproto::HubFrame>(1024);

    let mut actions = crate::actions::Actions::new();
    let mut handles = Vec::new();

    for entry in &config.servers {
        let transport = crate::wire::transport_for(entry);
        let client = Arc::new(crate::wire::HerdrClient::new(transport, 8));
        let handle = crate::serverconn::spawn_server(
            entry.id.clone(),
            entry.label.clone().unwrap_or_else(|| entry.id.clone()),
            client.clone(),
            config.poll_ms,
            config.reconcile_secs,
            broadcast_tx.clone(),
            interest_rx.clone(),
            registry.clone(),
        );
        actions.register(&entry.id, client, handle.shared.clone());
        handles.push(ServerHandleInfo {
            id: handle.id,
            label: handle.label,
            kind: match entry.kind {
                crate::config::ServerKind::Local { .. } => "local",
                crate::config::ServerKind::Ssh { .. } => "ssh",
            },
            peek: handle.shared,
        });
    }

    let ctx = Arc::new(SessionCtx {
        registry: registry.clone(),
        broadcast: broadcast_tx,
        actions: Arc::new(actions),
        servers: handles,
        token,
    });

    Hub { ctx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_rules() {
        let mut headers = HeaderMap::new();
        assert!(origin_allowed(&headers, "127.0.0.1:8787", &[])); // no origin
        headers.insert(header::ORIGIN, "http://127.0.0.1:8787".parse().unwrap());
        assert!(origin_allowed(&headers, "127.0.0.1:8787", &[])); // same
        assert!(origin_allowed(
            &headers,
            "localhost:8787",
            &[]
        )); // loopback swap
        headers.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert!(!origin_allowed(&headers, "127.0.0.1:8787", &[]));
        assert!(origin_allowed(
            &headers,
            "127.0.0.1:8787",
            &["http://evil.example".to_string()]
        )); // explicit allow
    }
}
