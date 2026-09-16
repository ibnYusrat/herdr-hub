//! Hub configuration: TOML file, auth token, data directories.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Default bind: all interfaces, so the hub is reachable from the LAN (and
/// phone clients) out of the box. Token auth + origin check are always on;
/// put TLS (or a reverse proxy) in front for anything beyond a trusted
/// network — without it the token crosses the wire in cleartext.
pub const DEFAULT_BIND: &str = "0.0.0.0:8787";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind: SocketAddr,
    /// Optional TLS with user-provided certificates (SPEC §9).
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Extra allowed Origins for the WebSocket upgrade check, in addition to
    /// the hub's own bind origin. Empty = same-origin only.
    pub origin_allow: Vec<String>,
    pub log_level: String,
    /// Poll tick for viewed panes, milliseconds (SPEC §6: 100–250).
    pub poll_ms: u64,
    /// Snapshot reconciliation interval, seconds: bounds state drift from
    /// missed or causally-inverted events (verified live on herdr 0.8.2).
    pub reconcile_secs: u64,
    pub servers: Vec<ServerEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerEntry {
    /// Server id used in hub protocol messages (short slug, e.g. "local").
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(flatten)]
    pub kind: ServerKind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ServerKind {
    Local {
        /// Explicit socket path. Default: this user's default herdr socket.
        #[serde(default)]
        socket: Option<PathBuf>,
        /// Named herdr session: `<config_dir>/sessions/<name>/herdr.sock`.
        #[serde(default)]
        session: Option<String>,
    },
    Ssh {
        /// Anything `ssh` accepts: alias, user@host, with port.
        host: String,
        #[serde(default)]
        session: Option<String>,
        /// Dedicated private key for this server (`ssh -i`), so the hub does
        /// not depend on the user's default identities or ssh-agent.
        #[serde(default)]
        identity: Option<PathBuf>,
        /// How to reach the remote herdr API (default `bridge`):
        /// - `bridge` — `ssh <host> herdr remote-api-bridge`, herdr's own
        ///   remote transport (herdr ≥ 0.9; undocumented surface).
        /// - `forward` — OpenSSH Unix-socket forwarding to the remote
        ///   `herdr.sock` (`ssh -N -L <local>:<remote>`). Works against any
        ///   herdr version, including 0.8.2 which lacks `remote-api-bridge`.
        #[serde(default)]
        transport: Option<SshTransportMode>,
        /// Remote socket path override (forward mode). A relative path is
        /// resolved against the remote home by the hub (sshd does not expand
        /// relative unix-socket paths). Default derives from `session`:
        /// `.config/herdr[/sessions/<name>]/herdr.sock`.
        #[serde(default)]
        socket: Option<String>,
    },
}

/// SSH transport variant (see `ServerKind::Ssh::transport`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshTransportMode {
    #[default]
    Bridge,
    Forward,
}

/// Remote herdr socket path relative to the remote home (forward mode). The
/// hub resolves this to an absolute path for `ssh -L` (sshd does no expansion).
pub fn remote_socket_rel(session: Option<&str>) -> String {
    match session {
        Some(name) => format!(".config/herdr/sessions/{name}/herdr.sock"),
        None => ".config/herdr/herdr.sock".into(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.parse().unwrap(),
            tls_cert: None,
            tls_key: None,
            origin_allow: Vec::new(),
            log_level: "info".into(),
            poll_ms: 200,
            reconcile_secs: 30,
            servers: vec![ServerEntry {
                id: "local".into(),
                label: None,
                kind: ServerKind::Local {
                    socket: None,
                    session: None,
                },
            }],
        }
    }
}

/// `<config_dir>/herdr-hub` — XDG_CONFIG_HOME honored, else ~/.config.
pub fn data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("herdr-hub");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("herdr-hub")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// Default herdr socket for this user (mirrors herdr's `api_socket_path_for`).
pub fn herdr_socket_default() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("herdr").join("herdr.sock");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("herdr")
        .join("herdr.sock")
}

pub fn herdr_socket_for(session: Option<&str>) -> PathBuf {
    match session {
        Some(name) => {
            let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg).join("herdr")
            } else {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".config").join("herdr")
            };
            base.join("sessions").join(name).join("herdr.sock")
        }
        None => herdr_socket_default(),
    }
}

impl Config {
    pub fn load_or_default(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => config_path(),
        };
        if !path.exists() {
            return Ok((Self::default(), path));
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok((cfg, path))
    }

    pub fn validate(&self) -> Result<()> {
        if self.servers.is_empty() {
            anyhow::bail!("no servers configured");
        }
        let mut ids = std::collections::HashSet::new();
        for s in &self.servers {
            if !is_slug(&s.id) {
                anyhow::bail!("server id {:?} must be [a-z0-9_-]+", s.id);
            }
            if !ids.insert(s.id.clone()) {
                anyhow::bail!("duplicate server id {:?}", s.id);
            }
        }
        if (self.tls_cert.is_some()) != (self.tls_key.is_some()) {
            anyhow::bail!("tls_cert and tls_key must be set together");
        }
        Ok(())
    }
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Auth token: generated on first run, stored mode 0600, never in URLs
/// (SPEC §9).
pub struct Token {
    pub value: String,
    pub path: PathBuf,
    pub generated: bool,
}

pub fn load_or_create_token(dir: &Path) -> Result<Token> {
    fs::create_dir_all(dir).context("creating hub data dir")?;
    let path = dir.join("token");
    if path.exists() {
        let value = fs::read_to_string(&path)
            .context("reading auth token")?
            .trim()
            .to_string();
        if value.is_empty() {
            anyhow::bail!("auth token file {} is empty", path.display());
        }
        return Ok(Token {
            value,
            path,
            generated: false,
        });
    }
    let value = generate_token();
    fs::write(&path, &value).context("writing auth token")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("restricting token file permissions")?;
    }
    Ok(Token {
        value,
        path,
        generated: true,
    })
}

fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation() {
        assert!(is_slug("local"));
        assert!(is_slug("mbp-2"));
        assert!(is_slug("work_laptop"));
        assert!(!is_slug("Local"));
        assert!(!is_slug(""));
        assert!(!is_slug("a b"));
    }

    #[test]
    fn parses_config() {
        let raw = r#"
bind = "127.0.0.1:9999"
[[servers]]
id = "local"
kind = "local"
[[servers]]
id = "mbp"
kind = "ssh"
host = "mbp.local"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.bind.port(), 9999);
        assert_eq!(cfg.servers.len(), 2);
        assert!(matches!(cfg.servers[1].kind, ServerKind::Ssh { .. }));
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_ssh_forward_transport() {
        let raw = r#"
[[servers]]
id = "storage"
kind = "ssh"
host = "storage"
session = "hub-test"
transport = "forward"
socket = ".local/share/herdr/herdr.sock"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let ServerKind::Ssh {
            transport,
            socket,
            ..
        } = &cfg.servers[0].kind
        else {
            panic!("expected ssh server");
        };
        assert_eq!(*transport, Some(SshTransportMode::Forward));
        assert_eq!(socket.as_deref(), Some(".local/share/herdr/herdr.sock"));
    }

    #[test]
    fn ssh_transport_defaults_to_bridge() {
        let raw = r#"
[[servers]]
id = "storage"
kind = "ssh"
host = "storage"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let ServerKind::Ssh { transport, .. } = &cfg.servers[0].kind else {
            panic!("expected ssh server");
        };
        assert_eq!(*transport, None);
        assert_eq!(SshTransportMode::default(), SshTransportMode::Bridge);
    }

    #[test]
    fn remote_socket_paths() {
        assert_eq!(
            remote_socket_rel(None),
            ".config/herdr/herdr.sock"
        );
        assert_eq!(
            remote_socket_rel(Some("hub-test")),
            ".config/herdr/sessions/hub-test/herdr.sock"
        );
    }
}
