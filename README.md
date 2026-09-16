# herdr-hub

A standalone gateway for [herdr](https://herdr.dev) terminal-multiplexer
servers. The hub connects to one or more running herdr instances: the local
session over its Unix socket, remote machines over SSH. It exposes a single
push-based WebSocket protocol for web and mobile clients, giving them the
live session tree, streaming terminal screens, agent status, and a guarded
subset of herdr's actions (prompt an agent, send keys, manage workspaces,
tabs, panes, and worktrees).

The web client is embedded in the hub binary: run the hub, open the page,
paste the token. Nothing else to deploy.

![herdr-hub web client: sidebar with servers, workspaces, and agents; live
terminal transcript with ANSI colors; composer at the bottom](docs/screenshot.png)

```
browser -- WebSocket (hub protocol v1) -- herdr-hub -- Unix socket -- herdr (local)
                                            \-- SSH (bridge or forward) -- herdr (remote)
```

## Features

- Multiple herdr servers aggregated into one client view: local sessions
  and remote machines over SSH, each with reconnect, dimming, and fresh
  resync when a server goes away and comes back.
- Push-based protocol: the hub watches what each client views and streams
  only those panes, as full frames or row deltas, coalesced so a flooding
  pane cannot drown a slow client.
- Live tree of servers, workspaces (with git branch and worktree info),
  tabs, panes, and agents with idle/working/blocked/done status.
- Two view modes for pane content: a live grid that mirrors herdr's
  layouts, and an append-only text transcript that keeps growing output
  even across full-screen redraws.
- ANSI colors rendered throughout: the agent's own colors (diff reds and
  greens, dim hints, syntax highlighting) arrive as styled text, not
  stripped plaintext.
- Guarded actions: prompt agents, send text or keys to panes, create and
  close workspaces, tabs, and panes, split and resize, manage worktrees.
  Everything else is rejected by an allowlist; blocked agents are never
  auto-retried.
- Browser notifications when an agent becomes blocked while the tab is
  hidden.
- Works with herdr 0.8.2 and 0.9.0+ servers without configuration: the hub
  detects each server's protocol version and compensates for known quirks.

## How it works

The hub is a single Rust binary (tokio + axum) with four main pieces:

1. **herdr wire client** (`hub/src/wire/`): speaks herdr's NDJSON API.
   Local servers use the Unix socket; SSH servers use either herdr's own
   `remote-api-bridge` pipe or OpenSSH Unix-socket forwarding, with
   ControlMaster multiplexing so one persistent connection carries all
   traffic.
2. **State actors** (`hub/src/state.rs`, `hub/src/serverconn.rs`): each
   server has an actor that owns its canonical session tree, applies
   events (or reconciles snapshots on old servers), and republishes.
3. **Screen poller** (`hub/src/screen.rs`): polls `pane.read` for exactly
   the panes clients are viewing, diffs each screen, and pushes full
   frames or row deltas.
4. **Client sessions** (`hub/src/clients/`): one WebSocket per client with
   token auth, an origin check, and a bounded outbox that coalesces screen
   frames per pane.

The web client (Vite + React + TypeScript, `web/`) consumes the protocol,
keeps 10 Hz screen frames out of the app store (so the sidebar never
re-renders on output), and renders the tree, terminal grid, transcripts,
composer, and control toolbar. It is embedded into the binary at build
time via rust-embed.

## Requirements

- Rust (stable toolchain) to build the hub.
- Node >= 20 and npm to build the web client.
- A running herdr server, version 0.8.2 or newer. The hub is built against
  the pinned 0.9.0 API schema and checks each server's protocol version at
  connect.

## Build

Build the web client first; its output is embedded into the hub binary.

```sh
cd web && npm install && npm run build    # -> hub/web-dist
cd .. && cargo build --manifest-path hub/Cargo.toml
cargo install --path hub                  # installs herdr-hub onto PATH
```

The release build serves the web bundle standalone; no Node is needed at
runtime. To serve the client from disk instead (for client development),
pass `--web-dir ./web/dist`.

## Run

```sh
herdr-hub                      # http://0.0.0.0:8787, default config
herdr-hub token                # print the auth token for the web client
herdr-hub dump                 # read-only: connect and print each server's tree
herdr-hub --config FILE --bind 127.0.0.1:9000 --log-level debug
herdr-hub --web-dir ./web/dist # serve the client from disk
```

First run generates a token at `<config dir>/token` (mode 0600,
`~/.config/herdr-hub/token` by default) and logs where. The token goes to
the browser through the login prompt; it is never accepted in the URL.

The hub binds `0.0.0.0:8787` by default so phones and LAN clients can
reach it. Restrict to loopback with `bind = "127.0.0.1:8787"`. Token auth
and the origin check are always on, but on a non-loopback bind you should
configure TLS (`tls_cert`/`tls_key`) or put the hub behind a TLS reverse
proxy; otherwise the token crosses the network in cleartext. The hub logs
a warning about this at startup.

### Run as a service

Instead of a background job tied to your terminal, install the hub as a
systemd user service. It survives logout, starts at boot (lingering is
enabled), restarts on crash, and logs to the journal:

```sh
herdr-hub service install    # write + enable + start the unit
herdr-hub service status     # systemctl --user status
herdr-hub service logs       # recent journal logs
herdr-hub service restart    # after editing config.toml or upgrading
herdr-hub service stop
herdr-hub service uninstall
```

The unit runs the installed binary (`~/.cargo/bin/herdr-hub`), so
`cargo install --path hub && herdr-hub service restart` upgrades in place.

## Configuration

`~/.config/herdr-hub/config.toml`. All keys are optional.

```toml
bind           = "0.0.0.0:8787"   # or 127.0.0.1:8787 for loopback-only
log_level      = "info"
poll_ms        = 200              # screen poll tick for viewed panes
reconcile_secs = 30               # periodic snapshot reconciliation per server
# origin_allow = ["https://herdr.example.com"]  # extra allowed WebSocket origins

# TLS (optional): serve wss:// directly instead of behind a proxy
# tls_cert = "/path/cert.pem"
# tls_key  = "/path/key.pem"

# A local herdr session.
[[servers]]
id      = "local"           # short slug used in protocol messages
label   = "this machine"
kind    = "local"
# socket  = "/run/user/1000/herdr.sock"  # explicit socket, or ...
# session = "work"                       # a named herdr session

# A remote machine, reached over SSH. Two transports:
#
#   transport = "bridge"   (default)  ssh <host> herdr [--session S]
#                          remote-api-bridge, herdr's own remote pipe.
#                          Requires herdr >= 0.9 on the remote.
#   transport = "forward"  OpenSSH Unix-socket forwarding: one long-lived
#                          ssh -N -L forwarder to the remote herdr.sock.
#                          Works with any herdr version; use this for 0.8.x.
#
# Both use ControlMaster multiplexing, so one persistent SSH connection
# carries all traffic.
[[servers]]
id        = "storage"
label     = "storage box"
kind      = "ssh"
host      = "storage.example.com"  # anything ssh accepts: alias, user@host, with port
transport = "forward"              # herdr 0.8.x on this box: no remote-api-bridge
# session  = "default"
# identity = "~/.ssh/id_herdr_hub"  # dedicated key (ssh -i + IdentitiesOnly)
# socket   = ".local/herdr.sock"    # forward mode: override the remote socket path
```

With no `[[servers]]` entries the hub configures one implicit `local`
server using the default session.

## The web client

Open the hub URL, paste the token once (it is remembered per browser), and
the tree appears: servers, workspaces with branch and worktree badges,
tabs, and agent rows with status.

- **Grid mode (default)**: the live pane grid, laid out from herdr's BSP
  layouts and rendered as terminals. It mirrors what herdr's TUI shows,
  including scroll position.
- **TXT mode**: an append-only transcript of the focused pane's output:
  wrapped, selectable text in its own scroller, with real ANSI colors. It
  seeds from the pane's current screen and then only ever appends new
  lines; scrolling is entirely yours (it stays put when you scroll up,
  with a "latest" jump-back button) and is never tied to herdr's screen
  state. The choice is persisted per browser.

In both modes, and in the scrollback overlay, the agent console's own
input area (the `❯` input box, its dash rules, and the hint line) is
detected and stripped from what is rendered: the web client has its own
composer, so the console's would be redundant and misleading. The
permission mode from that hint line (`⏵⏵ auto mode on`,
`⏵⏵ bypass permissions on`, and friends) is parsed and shown as a colored
pill next to the composer instead. Detection is structural (a hint line
anchored at the bottom, then rule, input, rule above it), so pane content
is never eaten and plain panes are untouched.

The composer under the grid prompts the focused agent (or sends a line to
a plain pane). The control toolbar offers enter/esc/arrows/tab/yes/no/
ctrl+c shortcuts and pane management; when an agent is blocked it is
highlighted and the banner explains that the prompt was not sent. Raw mode
forwards keystrokes straight to the pane. On narrow screens the sidebar
becomes a drawer and panes become a swipe carousel.

## Protocol, in one breath

JSON text frames over WebSocket, `{type}`-discriminated, protocol `1`.
Auth in the first `hello` frame (never the URL). The hub pushes `welcome`,
`snapshot`, `event`, `screen` (full or row deltas, coalesced), `response`,
and `server_status`. Clients send `view` (which panes they watch; this
drives screen polling), `request` (allowlisted herdr actions, params
verbatim), and `ping`. herdr errors pass through verbatim
(`agent_blocked`, ...); hub-added errors are `hub.*`
(`hub.action_not_allowed`, `hub.prompt_in_flight`, ...). The full contract
lives in `protocol/hub-protocol.md`.

Guards worth knowing: one in-flight `agent.prompt` per target (a second is
rejected, never queued; nothing auto-retries a blocked prompt), actions
are allowlisted, and methods newer than the server's protocol are gated
with `hub.action_not_allowed`.

## herdr version tolerance

The hub is built against the pinned 0.9.0 schema (protocol 22) and checks
the server's protocol at connect; it runs unchanged against 0.8.2
(protocol 20), which is a strict method subset. Several 0.8.2 stream
quirks are compensated in the hub and web client: dropped close events,
causally inverted lifecycle events, phantom panes of closed workspaces,
and `pane.read` revisions that stay at 0 across output. One behavior is
by design but violent to poll: `pane.read {source: recent}` on an idle
alternate-screen agent pane makes herdr scroll the app itself (wheel-event
harvest, then restore) to capture history. The hub's scrollback stream
therefore reads agent panes via `visible` and reserves `recent` for plain
panes. Details and mitigations: SPEC.md sections 3.4 and 3.5. Servers
newer than 0.9.0 are tolerated with a warning.

**Poll mode on herdr < 0.9.0**: 0.8.x servers replay their entire retained
event history on every new `events.subscribe` connection (one match per
100 ms tick; minutes of stale flood on a long-lived server, fixed in
0.9.0). The hub therefore does not subscribe to events at all on pre-0.9.0
servers and instead reconciles from `session.snapshot` every 2 s: tree,
focus, and agent-status changes land within about 2 s instead of
instantly, pane screens are unaffected (the screen poller still pushes at
full cadence), and blocked notifications still fire (derived from snapshot
diffs). On 0.9.0+ servers the hub streams events as designed.

Remote servers add one version constraint: the default SSH transport
(`bridge`) needs herdr >= 0.9 on the remote side (`remote-api-bridge`);
set `transport = "forward"` for 0.8.x remotes.

## Security notes

- Binds `0.0.0.0:8787` by default (reachable from the LAN); the startup
  log warns when serving cleartext HTTP non-loopback. Token auth on every
  WebSocket, an origin check on upgrade, and the token never appears in
  URLs or logs.
- The hub never logs prompt text or pane content at the default log
  level.
- The SSH transport is read-only on your SSH setup: it only needs
  `ssh <host>` to work (agent or the configured `identity` key) and uses
  its own ControlPath under the hub's data dir. It never writes to the
  remote beyond running herdr itself.

## Testing and verification

```sh
# hub unit + integration tests (a fake herdr server; no live session needed)
cargo test --manifest-path hub/Cargo.toml

# web client unit tests + typecheck
cd web && npm test && npx tsc --noEmit

# read-only smoke against a running hub:
# hello -> welcome -> snapshot -> view -> screen -> request -> ping
herdr-hub token | node scripts/smoke.mjs --url ws://127.0.0.1:8787/ws
herdr-hub token | node scripts/smoke.mjs --url wss://127.0.0.1:8787/ws --insecure  # TLS
```

The mutating end-to-end gates only ever run against a throwaway herdr
session (`--session hub-test`), never the real one:

```sh
# M3 gate: workspace.create -> pane.split -> send_text/send_keys -> screen
# echo -> pane.rename -> allowlist rejection -> pane.close -> tab ops -> close
herdr --session hub-test server &      # headless throwaway session
herdr-hub --config <(printf '[[servers]]\nid="hub-test"\nkind="local"\nsession="hub-test"\n') \
     --bind 127.0.0.1:8788 &
herdr-hub token | node scripts/e2e-m3.mjs --url ws://127.0.0.1:8788/ws
herdr session stop hub-test && herdr session delete hub-test   # teardown

# M4 gate: two servers (local + ssh) in one hub; kill and restart the remote
# throwaway session; the ssh server must dim and recover with a fresh
# snapshot and no duplicate tree entries.
ssh <host> 'setsid herdr --session hub-test server >/tmp/hub-test.log 2>&1 < /dev/null &'
herdr-hub --config <two-server-config.toml> &
herdr-hub token | node scripts/e2e-m4-ssh.mjs --url ws://127.0.0.1:8789/ws --host <host>
ssh <host> 'herdr session stop hub-test && herdr session delete hub-test'
```

## Repository layout

| Path | What it is |
| --- | --- |
| `SPEC.md` | The working specification, amended with everything verified live |
| `protocol/hub-protocol.md` | The hub/client wire contract (protocol v1) |
| `schemas/` | Pinned herdr API schemas: 0.9.0/protocol 22 (the contract), 0.8.2/protocol 20 (live export) |
| `hub/` | The Rust gateway binary (`herdr-hub`) |
| `web/` | The web client (Vite + React + TypeScript) |
| `scripts/` | `smoke.mjs` (read-only smoke), `e2e-m3.mjs` and `e2e-m4-ssh.mjs` (mutation gates, throwaway sessions only) |

## Not implemented (deliberate)

`pane.output_matched` subscriptions, plugin actions, graphics, the private
binary endpoint, and Windows named pipes. See SPEC.md section 11.
