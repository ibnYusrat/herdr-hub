//! Manage the hub as a systemd user service: `herdr-hub service install`.
//!
//! A user unit (not root) — it runs as this user, survives logout once
//! lingering is enabled, and starts at boot. The unit's `ExecStart` points
//! at the binary that ran `service install`, so `cargo install --path hub`
//! followed by `herdr-hub service restart` upgrades the service in place.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

pub const UNIT_NAME: &str = "herdr-hub.service";

#[derive(clap::Subcommand)]
pub enum Action {
    /// Write the systemd user unit, enable it, and start the hub now.
    Install,
    /// Stop, disable, and remove the unit.
    Uninstall,
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
    /// Restart the service (after a config change or a binary upgrade).
    Restart,
    /// Show the service status.
    Status,
    /// Show recent service logs.
    Logs,
}

pub fn run(action: Action) -> Result<()> {
    match action {
        Action::Install => install(),
        Action::Uninstall => uninstall(),
        Action::Start => control("start"),
        Action::Stop => control("stop"),
        Action::Restart => control("restart"),
        Action::Status => status(),
        Action::Logs => logs(),
    }
}

/// `~/.config/systemd/user/herdr-hub.service` (systemd ignores
/// XDG_CONFIG_HOME for units, so this is always under `$HOME/.config`).
pub fn unit_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME)
}

pub fn unit_file(exe: &str) -> String {
    format!(
        "[Unit]\n\
         Description=herdr-hub gateway for herdr servers\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         # Config lives at ~/.config/herdr-hub/config.toml —\n\
         # edit it, then: herdr-hub service restart\n\
         ExecStart={exe}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn systemctl_ok(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running systemctl — is this a systemd system?")?;
    if !out.status.success() {
        bail!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn install() -> Result<()> {
    let exe = std::env::current_exe().context("resolving hub binary path")?;
    let unit = unit_path();
    std::fs::create_dir_all(unit.parent().expect("unit path has a parent"))
        .context("creating systemd user unit dir")?;
    std::fs::write(&unit, unit_file(&exe.display().to_string()))
        .with_context(|| format!("writing {}", unit.display()))?;
    systemctl_ok(&["daemon-reload"])?;

    // Survive logout / run from boot, with no graphical session needed.
    // (Users may enable linger for themselves; if polkit forbids it, the
    // service still runs whenever the user is logged in.)
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty()
            && !Command::new("loginctl")
                .args(["enable-linger", &user])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        {
            eprintln!("note: could not enable lingering — the hub will only run while you are logged in");
        }
    }

    systemctl_ok(&["enable", "--now", UNIT_NAME])?;
    println!("installed {UNIT_NAME} -> {}", exe.display());
    println!("token: herdr-hub token | logs: herdr-hub service logs");
    if !is_active() {
        eprintln!(
            "warning: service is not active yet — check `herdr-hub service status` (is another hub still holding the port?)"
        );
    }
    Ok(())
}

fn uninstall() -> Result<()> {
    // The service may not exist / not be running — that is fine here.
    let _ = systemctl_ok(&["disable", "--now", UNIT_NAME]);
    let unit = unit_path();
    if unit.exists() {
        std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
    }
    systemctl_ok(&["daemon-reload"])?;
    println!("removed {UNIT_NAME} (lingering left as-is: `loginctl disable-linger $USER` to undo)");
    Ok(())
}

fn control(action: &str) -> Result<()> {
    systemctl_ok(&[action, UNIT_NAME])?;
    println!("{UNIT_NAME}: {}", if is_active() { "active" } else { "inactive" });
    Ok(())
}

fn status() -> Result<()> {
    let code = Command::new("systemctl")
        .args(["--user", "status", UNIT_NAME])
        .status()
        .context("running systemctl")?;
    std::process::exit(code.code().unwrap_or(1));
}

fn logs() -> Result<()> {
    let code = Command::new("journalctl")
        .args(["--user", "-u", UNIT_NAME, "-n", "200", "--no-pager"])
        .status()
        .context("running journalctl")?;
    std::process::exit(code.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_shape() {
        let f = unit_file("/home/x/.cargo/bin/herdr-hub");
        assert!(f.contains("ExecStart=/home/x/.cargo/bin/herdr-hub"));
        assert!(f.contains("Restart=on-failure"));
        assert!(f.contains("[Install]"));
        assert!(f.contains("WantedBy=default.target"));
    }
}
