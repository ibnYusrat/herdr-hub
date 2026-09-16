//! Transports for talking to a herdr server: a local Unix socket, or SSH to a
//! remote host — either herdr's `remote-api-bridge` stdio pipe (herdr ≥ 0.9)
//! or OpenSSH Unix-socket forwarding (any version).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// A bidirectional line-oriented connection to a herdr server.
#[async_trait]
pub trait LineStream: Send {
    async fn send_line(&mut self, line: &str) -> Result<()>;
    /// `Ok(None)` on EOF (server closed).
    async fn recv_line(&mut self) -> Result<Option<String>>;
    async fn close(&mut self);
}

struct LineIo<R: AsyncRead + Unpin + Send, W: AsyncWrite + Unpin + Send> {
    reader: BufReader<R>,
    writer: W,
}

#[async_trait]
impl<R: AsyncRead + Unpin + Send, W: AsyncWrite + Unpin + Send> LineStream for LineIo<R, W> {
    async fn send_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn recv_line(&mut self) -> Result<Option<String>> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf).await {
            Ok(0) => Ok(None),
            Ok(_) => {
                if buf.ends_with('\n') {
                    buf.pop();
                    if buf.ends_with('\r') {
                        buf.pop();
                    }
                }
                Ok(Some(buf))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn close(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

/// Opens connections to one herdr server. Connections are single-use on the
/// herdr side (one request per connection, plus the long-lived subscribe
/// stream), so `open()` is a factory, not a pool.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(&self) -> Result<Box<dyn LineStream>>;
    fn kind(&self) -> TransportKind;
    fn describe(&self) -> String;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Local,
    Ssh,
}

/// Connects to a herdr Unix socket by path.
#[derive(Clone, Debug)]
pub struct LocalTransport {
    socket: PathBuf,
}

impl LocalTransport {
    /// Default-session socket for this user:
    /// `$XDG_CONFIG_HOME/herdr/herdr.sock` or `~/.config/herdr/herdr.sock`.
    pub fn default_socket() -> PathBuf {
        crate::config::herdr_socket_default()
    }

    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
}

#[async_trait]
impl Transport for LocalTransport {
    async fn open(&self) -> Result<Box<dyn LineStream>> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("connecting to herdr socket {}", self.socket.display()))?;
        let (read, write) = stream.into_split();
        Ok(Box::new(LineIo {
            reader: BufReader::new(read),
            writer: write,
        }))
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Local
    }

    fn describe(&self) -> String {
        self.socket.display().to_string()
    }
}

/// Reaches a remote herdr server over SSH. Two variants:
///
/// - `Bridge` (herdr ≥ 0.9): the undocumented-but-working `remote-api-bridge`
///   subcommand pipes the remote socket over stdio. Each `open()` spawns one
///   `ssh` process — cheap under connection multiplexing, so the hub always
///   passes its own ControlMaster/ControlPath options with
///   `ControlPersist=10m`.
/// - `Forward` (any herdr version, incl. 0.8.2 which lacks the bridge):
///   one long-lived `ssh -N -L <local-unix-sock>:<remote-rel-path>` forwarder;
///   `open()` is a plain local `UnixStream::connect` (each connection is an
///   ssh channel multiplexed over the forwarder).
#[derive(Clone, Debug)]
pub struct SshTransport {
    host: String,
    session: Option<String>,
    identity: Option<PathBuf>,
    control_dir: PathBuf,
    mode: crate::config::SshTransportMode,
    /// Forward mode: remote socket path as configured (may be relative —
    /// sshd does NOT expand relative paths, so the hub resolves the remote
    /// home once and builds an absolute path for `-L`).
    remote_socket: String,
    /// Forward mode: cached remote `$HOME` (resolved lazily, once).
    remote_home: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    /// Forward mode: serializes forwarder (re)spawns across concurrent
    /// `open()` callers (actor lanes + screen poller).
    forwarder_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// Forward mode: local endpoint the ssh forwarder listens on.
    local_socket: PathBuf,
}

impl SshTransport {
    #[allow(clippy::too_many_arguments)]
    pub fn with_mode(
        host: String,
        session: Option<String>,
        identity: Option<PathBuf>,
        control_dir: PathBuf,
        mode: crate::config::SshTransportMode,
        remote_socket: Option<String>,
    ) -> Self {
        let remote_socket =
            remote_socket.unwrap_or_else(|| crate::config::remote_socket_rel(session.as_deref()));
        let local_socket = control_dir.join(format!(
            "fwd-{}{}.sock",
            host.replace(['/', ':'], "_"),
            session.as_deref().map(|s| format!("-{s}")).unwrap_or_default()
        ));
        Self {
            host,
            session,
            identity,
            control_dir,
            mode,
            remote_socket,
            remote_home: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            forwarder_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            local_socket,
        }
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg(format!(
                "ControlPath={}",
                self.control_dir.join("cm-%r@%h:%p").display()
            ))
            .arg("-o")
            .arg("ControlPersist=10m")
            .arg("-o")
            .arg("ConnectTimeout=10");
        if let Some(identity) = &self.identity {
            cmd.arg("-i").arg(identity).arg("-o").arg("IdentitiesOnly=yes");
        }
        if let Some(session) = &self.session {
            cmd.arg("--").arg(&self.host).arg("herdr").arg("--session").arg(session).arg("remote-api-bridge");
        } else {
            cmd.arg("--").arg(&self.host).arg("herdr").arg("remote-api-bridge");
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    /// sshd does not expand relative unix-socket paths, so resolve the remote
    /// `$HOME` once and make the remote path absolute for `-L`.
    async fn absolute_remote_socket(&self) -> Result<String> {
        if self.remote_socket.starts_with('/') {
            return Ok(self.remote_socket.clone());
        }
        let mut cache = self.remote_home.lock().await;
        if let Some(home) = cache.as_ref() {
            return Ok(format!("{home}/{}", self.remote_socket));
        }
        let mut cmd = Command::new("ssh");
        cmd.arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=10");
        if let Some(identity) = &self.identity {
            cmd.arg("-i").arg(identity).arg("-o").arg("IdentitiesOnly=yes");
        }
        cmd.arg("--")
            .arg(&self.host)
            .arg("printf %s \"$HOME\"")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = cmd
            .output()
            .await
            .with_context(|| format!("resolving remote home on {}", self.host))?;
        let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || !home.starts_with('/') {
            return Err(anyhow!(
                "could not resolve remote home on {} (got {:?})",
                self.host,
                home
            ));
        }
        tracing::debug!(server = %self.host, home, "resolved remote home");
        *cache = Some(home.clone());
        Ok(format!("{home}/{}", self.remote_socket))
    }

    /// Forward mode: make sure the `ssh -N -L` forwarder is up, then connect.
    async fn open_forwarded(&self) -> Result<Box<dyn LineStream>> {
        tokio::fs::create_dir_all(&self.control_dir)
            .await
            .context("creating ssh control dir")?;

        // Fast path: forwarder already accepting.
        if tokio::net::UnixStream::connect(&self.local_socket).await.is_ok() {
            return self.connect_forwarded().await;
        }

        // Slow path: (re)spawn under a lock so concurrent open() callers
        // (request lanes, screen poller, subscribe) don't race two ssh
        // forwarders onto the same local socket.
        {
            let _guard = self.forwarder_lock.lock().await;
            // Re-check: another caller may have brought it up while we waited.
            if tokio::net::UnixStream::connect(&self.local_socket).await.is_err() {
                self.spawn_forwarder().await?;
            }
        }
        self.connect_forwarded().await
    }

    async fn spawn_forwarder(&self) -> Result<()> {
        let remote_path = self.absolute_remote_socket().await?;
        let _ = std::fs::remove_file(&self.local_socket);
        let mut cmd = Command::new("ssh");
        cmd.arg("-N") // no remote command, forwards only
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ControlMaster=auto")
            .arg("-o").arg(format!(
                "ControlPath={}",
                self.control_dir.join("cm-%r@%h:%p").display()
            ))
            .arg("-o").arg("ControlPersist=10m")
            .arg("-o").arg("ConnectTimeout=10")
            .arg("-o").arg("ExitOnForwardFailure=yes")
            .arg("-L").arg(format!("{}:{}", self.local_socket.display(), remote_path));
        if let Some(identity) = &self.identity {
            cmd.arg("-i").arg(identity).arg("-o").arg("IdentitiesOnly=yes");
        }
        cmd.arg("--").arg(&self.host)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // NOTE: no kill_on_drop — the forwarder must outlive this call. A
        // detached reaper task waits on it so an exited forwarder (network
        // drop) is reaped instead of zombied.
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning ssh forwarder to {}", self.host))?;

        // Wait until the local endpoint accepts a connection — a plain
        // socket file is not enough (a dead forwarder can leave one behind).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match tokio::net::UnixStream::connect(&self.local_socket).await {
                Ok(_) => break,
                Err(_) => {
                    // ExitOnForwardFailure kills the child if the remote end
                    // is unreachable — surface that instead of waiting out
                    // the deadline.
                    if let Some(status) = child.try_wait().ok().flatten() {
                        anyhow::bail!(
                            "ssh forwarder to {} exited {status} (remote socket {remote_path} unreachable?)",
                            self.host
                        );
                    }
                    if std::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "ssh forwarder socket {} did not come up",
                            self.local_socket.display()
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        tracing::debug!(server = %self.host, local = %self.local_socket.display(), remote = %remote_path, "ssh forwarder up");
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }

    async fn connect_forwarded(&self) -> Result<Box<dyn LineStream>> {
        let stream = tokio::net::UnixStream::connect(&self.local_socket)
            .await
            .with_context(|| {
                format!(
                    "connecting to forwarded herdr socket {}",
                    self.local_socket.display()
                )
            })?;
        let (read, write) = stream.into_split();
        Ok(Box::new(LineIo {
            reader: BufReader::new(read),
            writer: write,
        }))
    }
}

/// A live ssh bridge child. Dropping the LineStream kills the child
/// (`kill_on_drop`), which also tears down the pipe.
struct ChildStream {
    io: LineIo<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    child: Option<Child>,
}

#[async_trait]
impl LineStream for ChildStream {
    async fn send_line(&mut self, line: &str) -> Result<()> {
        self.io.send_line(line).await
    }

    async fn recv_line(&mut self) -> Result<Option<String>> {
        self.io.recv_line().await
    }

    async fn close(&mut self) {
        self.io.close().await;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
    }
}

#[async_trait]
impl Transport for SshTransport {
    async fn open(&self) -> Result<Box<dyn LineStream>> {
        match self.mode {
            crate::config::SshTransportMode::Forward => self.open_forwarded().await,
            crate::config::SshTransportMode::Bridge => self.open_bridged().await,
        }
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Ssh
    }

    fn describe(&self) -> String {
        let mode = match self.mode {
            crate::config::SshTransportMode::Bridge => "bridge",
            crate::config::SshTransportMode::Forward => "forward",
        };
        match &self.session {
            Some(s) => format!("ssh {} (session {}, {mode})", self.host, s),
            None => format!("ssh {} ({mode})", self.host),
        }
    }
}

impl SshTransport {
    async fn open_bridged(&self) -> Result<Box<dyn LineStream>> {
        tokio::fs::create_dir_all(&self.control_dir)
            .await
            .context("creating ssh control dir")?;
        let mut child = self
            .build_command()
            .spawn()
            .with_context(|| format!("spawning ssh bridge to {}", self.host))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("ssh bridge has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ssh bridge has no stdout"))?;
        Ok(Box::new(ChildStream {
            io: LineIo {
                reader: BufReader::new(stdout),
                writer: stdin,
            },
            child: Some(child),
        }))
    }
}
