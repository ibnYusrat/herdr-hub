//! herdr-hub: a standalone gateway that connects to running herdr servers
//! and exposes one push-based client protocol for non-terminal clients.
//! See SPEC.md and protocol/hub-protocol.md.

use herdr_hub::{app, config, wire};

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "herdr-hub", version, about = "Gateway for herdr servers")]
struct Cli {
    /// Path to config.toml (default: <config_dir>/herdr-hub/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override bind address (default 0.0.0.0:8787)
    #[arg(long, global = true)]
    bind: Option<String>,

    /// Serve the web client from this directory instead of the embedded bundle
    #[arg(long, global = true)]
    web_dir: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true)]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the auth token (for pasting into the web client).
    Token,
    /// Connect to configured servers read-only and print the session tree.
    Dump {
        /// Only this server id
        #[arg(long)]
        server: Option<String>,
    },
    /// Manage the hub as a systemd user service (survives logout, restarts
    /// on crash, logs to the journal).
    Service {
        #[command(subcommand)]
        action: herdr_hub::service::Action,
    },
    /// Run the hub (default when no subcommand is given).
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.log_level.as_deref());

    let (mut cfg, cfg_path) = config::Config::load_or_default(cli.config.as_deref())?;
    if let Some(bind) = &cli.bind {
        cfg.bind = bind
            .parse()
            .with_context(|| format!("invalid bind address {bind:?}"))?;
    }
    if !cfg_path.exists() && matches!(cli.command, None | Some(Command::Run) | Some(Command::Dump { .. })) {
        info!(
            config = %cfg_path.display(),
            "no config file found; using defaults (local server)"
        );
    }
    cfg.validate()?;

    match cli.command.unwrap_or(Command::Run) {
        Command::Token => {
            let token = config::load_or_create_token(&config::data_dir())?;
            println!("{}", token.value);
            Ok(())
        }
        Command::Dump { server } => dump(cfg, server.as_deref()).await,
        Command::Service { action } => herdr_hub::service::run(action),
        Command::Run => run(cfg, cli.web_dir).await,
    }
}

fn init_logging(level: Option<&str>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            level.unwrap_or("info"),
        )
    });
    // Logs go to stderr so subcommand stdout (`herdr-hub token`) stays clean.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn run(cfg: config::Config, web_dir: Option<PathBuf>) -> Result<()> {
    let token = config::load_or_create_token(&config::data_dir())?;
    if token.generated {
        // Deliberately printed once, on generation only (SPEC §9).
        info!(
            token_file = %token.path.display(),
            "generated auth token; run `herdr-hub token` to see it again"
        );
    }

    let hub = app::spawn_hub(&cfg, token.value.clone());
    let router = app::build_app(&cfg, hub.ctx.clone(), web_dir);
    info!(
        bind = %cfg.bind,
        servers = cfg.servers.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(","),
        "herdr-hub listening"
    );

    if !cfg.bind.ip().is_loopback() && cfg.tls_cert.is_none() {
        warn!(
            bind = %cfg.bind,
            "serving plain HTTP on a non-loopback address: the auth token crosses the network in cleartext — configure tls_cert/tls_key or a TLS reverse proxy"
        );
    }

    match (cfg.tls_cert.clone(), cfg.tls_key.clone()) {
        (Some(cert), Some(key)) => run_tls(cfg.bind, router, &cert, &key).await,
        _ => {
            let listener = tokio::net::TcpListener::bind(cfg.bind)
                .await
                .with_context(|| format!("binding {}", cfg.bind))?;
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("server error")
        }
    }
}

async fn run_tls(
    addr: std::net::SocketAddr,
    router: axum::Router,
    cert: &std::path::Path,
    key: &std::path::Path,
) -> Result<()> {
    // rustls 0.23 needs a process-default CryptoProvider; the crate is built
    // with the `ring` feature, so install that one (idempotent).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_pem = std::fs::read(cert).context("reading TLS cert")?;
    let key_pem = std::fs::read(key).context("reading TLS key")?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parsing TLS cert")?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .context("parsing TLS key")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in TLS key file"))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS config")?;

    let app = axum_server::Handle::new();
    let handle = app.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });

    let tls = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config));
    axum_server::bind_rustls(addr, tls)
        .handle(app)
        .serve(router.into_make_service())
        .await
        .context("TLS server error")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("ctrl_c signal handler error: {e}");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => error!("SIGTERM handler error: {e}"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutting down");
}

/// Read-only dump: ping + snapshot per server, print the tree (M1 gate).
async fn dump(cfg: config::Config, only: Option<&str>) -> Result<()> {
    for entry in &cfg.servers {
        if let Some(want) = only {
            if entry.id != want {
                continue;
            }
        }
        let transport = wire::transport_for(entry);
        let describe = transport.describe();
        let client = Arc::new(wire::HerdrClient::new(transport, 2));
        match client.ping().await {
            Ok((version, protocol)) => {
                println!("server {} ({}, herdr {version}, protocol {protocol})", entry.id, describe);
            }
            Err(e) => {
                warn!(server = %entry.id, error = %e, "ping failed");
                continue;
            }
        }
        match client.snapshot().await {
            Ok(snap) => print_tree(&snap),
            Err(e) => error!(server = %entry.id, error = %e, "snapshot failed"),
        }
    }
    Ok(())
}

fn print_tree(snap: &wire::SessionSnapshot) {
    for ws in &snap.workspaces {
        println!(
            "  {} {:?} [active {}] agent={} tabs={} panes={}",
            ws.workspace_id,
            ws.label,
            ws.active_tab_id.as_deref().unwrap_or("-"),
            ws.agent_status.as_str(),
            ws.tab_count,
            ws.pane_count
        );
        for tab in snap.tabs.iter().filter(|t| t.workspace_id == ws.workspace_id) {
            println!(
                "    {} {:?} agent={}",
                tab.tab_id,
                tab.label,
                tab.agent_status.as_str()
            );
            for pane in snap.panes.iter().filter(|p| p.tab_id == tab.tab_id) {
                println!(
                    "      {} agent={:?} status={} rev={} cwd={}",
                    pane.pane_id,
                    pane.agent,
                    pane.agent_status.as_str(),
                    pane.revision,
                    pane.cwd.as_deref().unwrap_or("-")
                );
            }
        }
    }
    println!(
        "  focused: ws={:?} tab={:?} pane={:?}",
        snap.focused_workspace_id, snap.focused_tab_id, snap.focused_pane_id
    );
}
