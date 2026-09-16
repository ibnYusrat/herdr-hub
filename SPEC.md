# herdr-hub

A standalone gateway that connects to running [herdr](https://github.com/herdrdev/herdr)
servers and exposes one push-based client protocol for non-terminal clients.
The first client is a web manager UI. Android and iOS clients are planned later
and will speak the same hub protocol.

This document is the working spec. It records the decisions made so far, the
facts about herdr they depend on, and the plan for the first implementation.
Nothing here has been built yet.

Status: draft, 2026-09-11. Written against herdr 0.9.0 source on master
(commit `61ca85d5`), API protocol 22, API schema_version 1.

---

## 1. Goals

- Let a person see and drive all of their herdr sessions from a browser, with
  roughly the same information architecture as the herdr TUI: machines,
  workspaces, worktree groups, tabs, panes, agents, and agent status.
- Be an independent project. No herdr source code in this repo, no build-time
  dependency on the herdr crate. The only dependency is an installed `herdr`
  binary on the machine where a herdr server runs.
- Put all polling, connection multiplexing, reconnection, and multi-machine
  aggregation into one middle layer (the hub). Clients open one socket and read
  a stream. Clients never poll herdr and never know herdr's wire details.
- Keep the client protocol client-agnostic so later mobile apps reuse it.

## 2. Non-goals for the first version

- Byte-for-byte terminal fidelity or a general purpose remote terminal.
  The hub is a manager, not a replacement for `herdr --remote`.
- Streaming keystrokes in real time. See section 7.
- Kitty graphics and inline images.
- Copy mode. Browsers have native text selection.
- Replacing herdr's own SSH multi-machine feature. The hub may reuse it.

---

## 3. Facts about herdr this design depends on

All of these were verified by reading the herdr source and docs. File paths
are in the herdr repo and are given so they can be re-checked when herdr
updates. If any of these change, the hub design needs review.

### 3.1 Process model

- herdr runs as a background server process that owns every PTY, runs the
  terminal emulator (vendored libghostty-vt), does agent detection, and
  persists session state. The TUI is a separate thin client process.
- The server listens on two local sockets, both mode 0600, Unix domain
  sockets on Linux and macOS, named pipes on Windows:
  - `herdr.sock`: the public JSON API. This is what the hub uses.
  - `herdr-client.sock`: the private binary client protocol used by the TUI.
    Not used in v1. See section 11.
- There is no TCP listener, no HTTP, no WebSocket anywhere in herdr. Tokio is
  built without the `net` feature. Remote access is SSH only.
- There is no authentication of any kind beyond socket file permissions. Any
  process running as the same user has full control.

### 3.2 Socket location

- Default session: `<config_dir>/herdr.sock` where `<config_dir>` is
  `$XDG_CONFIG_HOME/herdr` if set, otherwise the platform config dir
  (`~/.config/herdr` on Linux).
- Named session `<name>`: `<config_dir>/sessions/<name>/herdr.sock`.
- `HERDR_SOCKET_PATH` overrides the path for CLI and tools.
- Source: `src/session.rs` (`data_dir_for`, `api_socket_path_for`),
  `src/config/io.rs` (`config_dir`).

### 3.3 JSON API transport

- Newline-delimited JSON. Custom envelope, not JSON-RPC.
- Request: `{"id": "<any string>", "method": "<dotted name>", "params": {...}}`
- Success: `{"id": "...", "result": {"type": "...", ...}}`
- Error: `{"id": "...", "error": {"code": "...", "message": "..."}}`
- One request per connection. The server reads a single line, replies, and
  closes. Only `events.subscribe` and `pane.graphics.stream` keep the
  connection open. Connections are single-use, so the hub bounds concurrency
  with a semaphore of "request lanes" (each request opens a fresh connection)
  rather than a reusable connection pool.
- The full JSON Schema for requests, responses, events, and subscription
  events is embedded in the binary and can be exported:

  ```bash
  herdr api schema            # summary: protocol, schema_version
  herdr api schema --json     # full JSON Schema to stdout
  herdr api schema --output herdr-api.schema.json
  herdr api snapshot          # live session.snapshot as JSON
  ```

  Generate TypeScript types from the exported schema. Commit the exported
  schema to this repo alongside the herdr version it came from.
- Source: `src/api/server.rs`, `src/api/schema.rs`, `src/api/schema/*`,
  docs `docs/next/website/src/content/docs/socket-api.mdx`.

### 3.4 Bootstrap and lifecycle events

- `session.snapshot` returns version, protocol, focused workspace/tab/pane ids,
  `workspaces`, `tabs`, `panes`, `layouts` (BSP split trees per tab), and
  `agents`. Sidebar and layout can be built from this alone.
- `events.subscribe` holds a connection open. First line acknowledges, later
  lines are pushed events. Verified details (herdr 0.8.2 live, 0.9.0 source):
  - **One subscription set per connection, fixed at subscribe time.** There is
    no add/remove message; refreshing the set means opening a new
    `events.subscribe` connection with the full desired set, then dropping the
    old one (overlap is safe — events are applied with upsert semantics).
  - **No replay before connect on 0.9.0.** The subscription starts at the
    current sequence for each new subscriber (0.9.0 source), which is
    exactly why the snapshot-plus-buffer recipe below is needed. This is
    NOT true of 0.8.2, which replays retained history — see the replay
    caveat below.
  Documented recipe to avoid a bootstrap gap:
  1. Open `events.subscribe` and wait for its acknowledgement.
  2. Buffer incoming events.
  3. Call `session.snapshot` on another connection and install it.
  4. Apply buffered events in order, then keep streaming.
  5. Call `session.snapshot` again after any reconnect.
- Subscribable lifecycle events (all push, no polling):
  - workspace: created, updated, metadata_updated, renamed, moved, reordered,
    closed, focused
  - tab: created, closed, focused, renamed, moved
  - pane: created, updated, closed, focused, moved, exited, agent_detected,
    agent_status_changed, output_matched, scroll_changed
  - layout: updated (carries the full layout snapshot for one tab)
  - worktree: created, opened, removed
- `pane.agent_status_changed`, `pane.output_matched`, and `pane.scroll_changed`
  are evaluated by the server on a 100 ms tick, then pushed. Everything else
  is emitted at the moment it happens.
- **These three tick-evaluated kinds are pane-scoped and REQUIRE a per-pane
  `pane_id` filter** — a filterless subscribe for them is rejected. Each
  per-pane subscription also costs the server a probe (`pane_get`) per tick,
  so the hub subscribes `agent_status_changed` per pane for every known pane.
  `scroll_changed` is NOT subscribed at all: nothing consumes it, and a
  view-dependent subscription set would force a connection swap on every
  client view change (see the replay caveat below). A dead
  `pane_id` in the set fails the whole subscribe (error instead of ack), so
  the hub recomputes from state and retries when panes churn.
- Envelope duality (verified live): lifecycle events arrive as
  `{"event":"<underscore_kind>","data":{"type":"<kind>",...,"<record>":{...}}}`
  with the full record nested under a key (`workspace`, `tab`, `pane`,
  `layout`); the three filtered pane kinds arrive with dotted event names
  (`"pane.agent_status_changed"`) and flat `data` with no `type` echo. Parse
  leniently and normalize dotted → underscore.
- **History replay on subscribe (verified live on 0.8.2; fixed in 0.9.0
  source):** every new `events.subscribe` connection walks the server's
  retained event history from the beginning, delivering one matching event
  per 100 ms poll tick. On a long-lived server this floods minutes of stale
  events — the user's whole navigation history, phantom closes, focus
  flapping — into hub state and every connected client, and any
  subscription swap re-triggers it. 0.9.0 clamps each subscription to the
  sequence at subscribe time. Events carry no sequence on the wire and the
  protocol has no replay opt-out, so the hub gates by protocol version:
  `protocol >= 22` streams subscriptions as designed; older servers run in
  **poll mode** — no event subscriptions at all, with the snapshot
  reconciler at a 2 s cadence as the change signal (tree, focus, agent
  status; screens are unaffected — the hub's screen poller pushes those).
  The web client derives agent-status transitions (notifications) from
  snapshot diffs in poll mode.
- **Event reliability caveat (verified live on 0.8.2 under heavy workspace
  churn):** herdr can drop close events entirely and can emit lifecycle
  events causally inverted (a `workspace_closed` for the previous instance
  arrives before the next instance's `pane_created`, because workspace
  numbers are reused immediately). Event-only state therefore drifts. The hub
  bounds this with periodic snapshot reconciliation (default every 30 s):
  fetch `session.snapshot` on a request lane, buffer-then-fold streaming
  events across the fetch (the same pattern as the bootstrap), and install +
  rebroadcast the snapshot whenever it differs. Related rules proven by the
  same live testing: `pane_updated` must be update-only (never resurrect a
  removed pane), and workspace/tab closures must cascade to their panes
  locally — herdr emits no per-pane events for them.
- **Phantom-pane flicker (verified live on 0.8.2):** `session.snapshot`
  intermittently lists panes of already-closed workspaces while omitting the
  workspace itself (observed as "0 workspaces, 1–3 orphan panes" — exactly
  the ids of a just-closed workspace), and the event stream can emit a
  matching `pane_created` for such an orphan. Installing one poisons every
  per-pane subscription (`pane_not_found` → drop → phantom event re-adds it
  → infinite subscribe churn). Guards on both hub and web client: a snapshot
  installs only panes whose workspace AND tab are in the same snapshot, and
  `pane_created` applies only when the parent workspace and tab already
  exist (herdr emits real creates strictly workspace → tab → pane, so this
  drops nothing legitimate).
- **Revision quirk (verified live on 0.8.2):** `pane.read` can leave
  `revision` at 0 across output. Screen change detection must therefore
  treat text equality — not revision equality — as the final word: skip an
  update only when BOTH revision and content are unchanged.
- There is no push event for "pane screen content changed". An
  `pane.output_changed` event kind exists in the schema but is not in the
  documented subscribable list and no emission site was found. Do not rely on
  it. Screen content must be polled. See section 6.

### 3.5 Reading and writing panes

- `pane.read` params: `pane_id`, `source` (`visible`, `recent`,
  `recent_unwrapped`, `detection`), optional `lines`, `format` (`text`,
  `ansi`), `strip_ansi`. Result includes `text`, a content `revision` (u64),
  and `truncated`. Line count is capped at 1000.
- The `revision` value is the change detector. If it has not moved since the
  last read, the content has not changed.
- Quirk: `format: ansi` with `source: detection` returns plain text.
- `pane.wait_for_output` blocks until a substring or regex matches in the
  selected snapshot, with an optional timeout. It matches text that is
  already present. It is not a "wait for any change" primitive.
- **Alternate-screen history capture SCROLLS the app (verified live on
  0.8.2, source 0.9.0 `src/server/alt_screen_read.rs`):** a
  `pane.read`/`agent.read` with `format: text`, `source: recent` or
  `recent_unwrapped`, on a pane with a known agent, idle, on the
  alternate screen, with mouse reporting enabled, and asking for more
  lines than the viewport shows, does NOT just read — herdr sends mouse
  wheel events INTO the pane to scroll the app up in batches, harvests
  rows, then wheels back down to restore the viewport. While busy the
  read instead fails with `agent_not_idle` ("use --source visible").
  Polling such a read repeatedly (as a scrollback stream would) makes
  the user's TUI visibly scroll up and down forever. Hub rule: the
  scrollback poller reads agent panes via `source: visible` (window diff
  semantics are identical client-side) and uses `recent` only for
  non-agent panes, where real scrollback exists and the read is passive.
- `pane.send_text` writes a string. `pane.send_keys` and `pane.send_input`
  accept herdr key-combo strings such as `enter`, `esc`, `up`, `ctrl+c`,
  `shift+tab`, `f1`.
- Pane ids are public strings like `w1:p1`. Agents can also be targeted by
  name. Ids and names are scoped to one server. Two machines may both have
  `w1:p1`. The hub must namespace everything by server.

### 3.6 Agent methods and status

- `AgentStatus` enum: `idle`, `working`, `blocked`, `done`, `unknown`.
  `idle` and `done` both mean ready for input. `done` is idle and not yet
  marked seen. `blocked` means herdr recognized an approval or question UI.
  `unknown` means an agent is present but not confidently classified.
- `agent.prompt` params: `target`, `text`, optional `wait` with `until` and
  `timeout_ms`. Sends the text plus an encoded Enter as one ordered
  submission, honors the terminal's bracketed-paste mode, and can prompt an
  agent that is already working. If the agent is already `blocked` it returns
  the error `agent_blocked` without sending anything.
- `agent.send_keys` sends interactive keys to an agent's UI. Use it for
  `esc`, `up`, `down`, `enter`, `ctrl+c`, `y`, `n`, `tab`.
- `agent.wait` waits for a lifecycle state, server-owned and event-driven.
- `agent.read` and `agent.explain` read the agent pane and explain detection.
- Caution from the docs: a timeout on `agent.prompt` does not prove nothing
  was sent. Read the pane before retrying to avoid a duplicate prompt.
- Source: `src/api/schema/agents.rs`, docs `agent-automation.mdx`.

### 3.7 Remote machines

- herdr's TUI federates Local plus saved SSH machines. It does this by
  spawning `ssh <host> herdr remote-client-bridge`, which exposes the remote
  `herdr-client.sock` over stdin and stdout.
- The same mechanism exists for the JSON API:
  `ssh <host> herdr [--session <name>] remote-api-bridge`. This exposes the
  remote `herdr.sock` over stdio.
- Both `remote-api-bridge` and `remote-client-bridge` are handled in
  `src/main.rs` but are not in the CLI reference. Treat them as unsupported
  for outside use: they work today and could change without notice. Pin the
  herdr version tested against.
- **Version caveat (verified live):** `remote-api-bridge` exists only in
  herdr ≥ 0.9. On 0.8.2 the remote rejects it (`unknown command`). For older
  remotes the hub's SSH transport has a second mode, `forward`: one
  long-lived `ssh -N -L <local-unix-sock>:<remote-socket>` forwarder, with
  every herdr connection multiplexed over it as a local `UnixStream::connect`.
  Note sshd does not expand relative unix-socket paths, so the hub resolves
  the remote `$HOME` once over SSH and forwards to an absolute path. This
  works against any herdr version.
- Alternative that uses only public surface: run one hub instance on every
  machine, next to its herdr server, and let the web client connect to each
  hub. Or run one hub and have it tunnel to the others over SSH using the
  bridge subcommand above.

### 3.8 What the TUI draws that the hub must reproduce from data

The TUI's chrome is not reusable. It is ~20k lines of Rust drawing into
terminal buffers with no serializable view model. Everything below has to be
rebuilt as web UI from the snapshot and events:

- Sidebar: machines, workspaces with branch and git ahead/behind, worktree
  groups, tabs, panes, agent rows with status icon and configurable tokens
  (state icon, state text, machine, workspace, tab, pane, agent name,
  terminal title, branch, git status).
- Tab bar with right-side status segments.
- Pane area following the tab's BSP layout from `layouts`.
- Overlays: rename, confirm close, help, navigator, settings, worktree
  create/open/remove, context menu, global menu, notifications, release
  notes, onboarding.
- A narrow "mobile" layout.

---

## 4. Decisions made

1. **Independent repository, no herdr source dependency.** Confirmed
   feasible. The hub talks to the installed binary's socket and uses the
   schema that binary exports.
2. **Integrate through the public JSON API (herdr.sock) first.** It is
   documented, versioned, language-agnostic, and covers the whole session
   tree plus pane read and write. The private binary endpoint is deferred
   (section 11).
3. **The hub is the only thing that polls.** Clients subscribe to one socket
   and receive a snapshot followed by deltas.
4. **Input is whole-message by default, not keystroke streaming.** A local
   composer submits prompts once via `agent.prompt`. A small control toolbar
   sends single keys via `agent.send_keys`. Raw keystroke passthrough is an
   optional per-pane mode, not the default. See section 7.
5. **The hub owns security.** Authentication, TLS or a localhost bind behind
   a tunnel, and origin checks live in the hub because herdr has none.
6. **Generic naming.** The project is `herdr-hub`, not web-specific, because
   mobile clients will share it.

---

## 5. Architecture

```
+-------------+      hub protocol       +-----------+   NDJSON over    +---------------+
| web client  | <--- WebSocket -------> |           | <- unix socket -> | herdr server  |
+-------------+                         |  herdr-   |                   |  (Local)      |
+-------------+                         |   hub     |   ssh stdio       +---------------+
| mobile app  | <--- WebSocket -------> |           | <- bridge -----> | herdr server  |
+-------------+                         +-----------+                   |  (SSH host)   |
                                                                        +---------------+
```

### 5.1 Hub responsibilities

1. **Server connections.** For each configured herdr server (local socket
   path, or an SSH target), maintain a connection pool for one-shot requests
   and one long-lived `events.subscribe` connection. Reconnect with backoff.
   Re-snapshot after every reconnect.
2. **Canonical session state.** One in-memory model per server built from
   `session.snapshot` and kept current by applying lifecycle events. This is
   the source of truth for every client.
3. **Screen polling.** Poll `pane.read` only for panes that at least one
   client is currently viewing. Compare `revision`, diff rows, and push only
   changes. See section 6.
4. **Per-client view.** Track which server, workspace, tab, and panes each
   client is viewing, and the client's viewport size, so polling and fanout
   are scoped to what is on screen.
5. **Request proxy.** Turn client action messages into herdr API calls and
   return the result, tagged with the client's request id.
6. **Coalescing and rate limiting.** Merge bursts of screen updates per
   client and cap outbound rate so a chatty pane cannot flood a slow client.
7. **Security.** Token or session auth on the WebSocket, optional TLS,
   origin checks, and safe defaults (bind to localhost unless configured).
8. **Multi-server aggregation.** Present all servers as one tree with every
   id namespaced by server.

### 5.2 Hub protocol (client facing)

Design goal: a client needs only this protocol. Exact shapes are to be
defined in the first implementation session, but the message families are
fixed:

- `hello` / `welcome`: auth token, client capabilities, hub version, list of
  servers and their connection status.
- `snapshot`: full state for one server: workspaces, tabs, panes, layouts,
  agents, focus. Sent on connect and after any hub-side resync.
- `event`: one lifecycle delta, mirroring herdr's event kinds but with
  server-namespaced ids. Clients apply these to their local copy.
- `screen`: pane content update. Either full content for a pane or a set of
  changed rows, with the pane's `revision`. Sent only for panes the client
  has declared it is viewing.
- `view`: client to hub. Declares which panes are visible and at what size,
  and which server/workspace/tab is active. Drives polling scope.
- `request` / `response`: client action with an id, hub replies with the
  herdr result or error. Actions are a curated subset of herdr methods:
  focus, split, resize, zoom, rename, close, create workspace/tab, worktree
  operations, `agent.prompt`, `agent.send_keys`, `pane.send_text`,
  `pane.send_keys`, `pane.scroll`, layout set split ratio, plugin actions.
- `server_status`: a server went offline, is reconnecting, or came back.
  Clients dim cached state rather than discarding it, matching TUI behavior.

Transport: WebSocket with JSON text frames. Consider MessagePack later if
screen updates become the bottleneck. Keep the protocol versioned from day
one with a version field in `hello` and `welcome`.

### 5.3 Session state model

Mirror herdr's records. Do not invent a different shape; that keeps the
schema-generated types usable end to end.

- Server: id, label, kind (local, ssh), status, herdr version, protocol.
- Workspace: id, number, label, cwd, branch, git ahead/behind, worktree
  provenance, active tab, agent status rollup, tokens.
- Tab: id, workspace id, number, label, zoomed, agent status rollup.
- Pane: id, workspace id, tab id, label, cwd, focused, scroll metrics,
  agent info (name, kind, title, status, state labels, tokens), revision.
- Layout: per tab BSP tree with ratios and rects, from `layouts`.

---

## 6. Screen content strategy

- Poll interval for viewed panes: start at 100 to 250 ms. herdr's own
  subscription checks run on a 100 ms tick, so faster gains nothing.
- Only viewed panes are polled. A pane that no client has on screen costs
  nothing. Unfocused but visible panes may poll slower than the focused one.
- Use `source: visible` for the on-screen grid. Use `recent` or
  `recent_unwrapped` with `lines` for a scrollback view. Remember the
  1000-line cap and the `truncated` flag.
- Prefer `format: ansi` and render in the browser with xterm.js in a
  write-only mode, or `format: text` painted into a plain grid. Decide in the
  first implementation session; ANSI preserves colors and is the safer
  default for agent UIs.
- Change detection: compare `revision`. If unchanged, send nothing. If
  changed, diff against the last sent content line by line and send changed
  rows, or send the whole visible buffer if most rows changed.
- Expected user experience: watching agents work is smooth. Typing into a
  raw terminal pane shows echo with one poll interval of delay. This is
  acceptable because the default input model does not echo through the
  terminal (section 7).

---

## 7. Input model

Three tiers. The first two cover almost all manager usage.

1. **Composer, sent once.** A text box under the agent or pane. The user
   types locally with zero latency. On send, the hub calls `agent.prompt`
   with the whole text. For a shell pane without an agent, the hub calls
   `pane.send_text` followed by `pane.send_keys ["enter"]`. If herdr returns
   `agent_blocked`, the UI shows the current screen and offers the control
   toolbar instead of retrying.
2. **Control toolbar, sent immediately.** Buttons for the keys agents need:
   Enter, Esc, Up, Down, Tab, Shift+Tab, y, n, Ctrl+C, plus number keys for
   menu selection. Sent via `agent.send_keys` or `pane.send_keys`. Show this
   prominently when status is `blocked`.
3. **Raw terminal mode, optional, per pane.** A toggle that streams
   keystrokes through `pane.send_input` for the rare case of running vim or
   an interactive shell. Echo comes back on the next poll. Off by default.

Retry rule: after a timeout on any submission, read the pane before
resubmitting. Never auto-retry a prompt.

---

## 8. Web client

- Reproduce the TUI information architecture rather than inventing a new
  one. Left sidebar with machines, workspaces, worktree groups, tabs, and
  agent rows with status. Main area shows the active tab's panes laid out
  from the BSP tree. Composer and control toolbar attached to the focused
  pane.
- Agent status visuals: distinct treatment for `blocked` (needs a human),
  `working`, `idle`/`done`, `unknown`, and dimmed for a disconnected server.
- Overlays to port first: rename, confirm close, new workspace, new tab,
  worktree create/open/remove, notifications. Settings, help, and release
  notes later.
- Narrow layout for phones from the start, since the mobile apps come later
  and the web client will be used on phones before then.
- Native browser selection replaces copy mode.

---

## 9. Security

- Hub binds to `0.0.0.0:8787` by default (LAN/phone clients out of the box;
  override with `bind = "127.0.0.1:8787"` for loopback-only). Token auth and
  the origin check are always on; on non-loopback binds without TLS the hub
  logs a cleartext warning — use `tls_cert`/`tls_key` or a TLS reverse proxy
  (or an SSH tunnel / Tailscale) beyond a trusted network.
- WebSocket auth: a bearer token generated by the hub on first run and
  stored in its config. Pass it in the first `hello` message, not in the
  URL.
- Origin check on the WebSocket upgrade.
- Optional TLS with user-provided certificates for direct exposure.
- The hub must never log prompt text or pane content at default log level.

---

## 10. Implementation plan

Technology decision (2026-09-11):

- **Hub: Rust.** Ships as a single static binary next to `herdr`, matches
  herdr's own ecosystem, and is the part that would later speak the binary
  endpoint protocol (section 11). Use tokio, a WebSocket crate such as
  tokio-tungstenite, and an HTTP layer such as axum to serve the web client
  bundle. Generate Rust types for the herdr API from the exported JSON
  Schema, or hand-write the subset the hub uses with serde.
- **Web client: TypeScript.** Browsers run JavaScript, so the UI layer is
  TypeScript whatever the hub is written in. Generate TypeScript types for
  the hub protocol so both sides share one contract. Keep the hub protocol
  schema in this repo as the source of truth for both languages.
- **Mobile clients later** speak the same hub protocol and can be written in
  whatever the platform wants.

Repository layout to start with:

```
herdr-hub/
  SPEC.md
  hub/        Rust crate, the gateway
  web/        TypeScript web client
  protocol/   hub protocol schema shared by hub and clients
  schemas/    exported herdr API schema, pinned to a herdr version
```

Phases:

| Phase | Deliverable | Rough size, one developer |
| --- | --- | --- |
| 0 | Export schema, generate types, prove NDJSON client and `events.subscribe` against a local herdr | 1 to 2 days |
| 1 | Hub: server connection pool, snapshot plus event replay, canonical state, WebSocket with auth, hub protocol v1 | 1 to 2 weeks |
| 2 | Read-only web client: sidebar, tabs, pane grid from layouts, screen polling and rendering, agent status | 1 to 2 weeks |
| 3 | Interaction: composer with `agent.prompt`, control toolbar, focus, split, resize, zoom, rename, close, create, worktrees | 1 to 2 weeks |
| 4 | Multi-server (SSH), reconnect and dimmed cached state, notifications, mobile layout, raw terminal mode | 2 to 4 weeks |

A usable read-only dashboard with click-to-focus and a composer is a two to
three week project. TUI parity is two to three months.

Verification for phase 0, run on a machine with a running herdr:

```bash
herdr api schema --output herdr-api.schema.json
herdr api snapshot | jq '.result | keys'
printf '%s\n' '{"id":"1","method":"ping","params":{}}' | nc -U ~/.config/herdr/herdr.sock
printf '%s\n' '{"id":"2","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"},{"type":"pane.agent_status_changed","pane_id":"<a real pane id>"}]}}' | nc -U ~/.config/herdr/herdr.sock
```

(`pane.agent_status_changed` and the other tick-evaluated kinds require a
per-pane `pane_id` — see §3.4.) Implementation note: schemas for both
protocol versions in use are pinned under `schemas/` in this repo; the
running server at the time of writing is herdr 0.8.2 / protocol 20, and the
pinned contract is 0.9.0 / protocol 22 (a strict method superset), with a
protocol check and graceful degradation at connect.

---

## 11. Deferred: live terminal via the private endpoint protocol

herdr's TUI uses `herdr-client.sock`, a bincode-framed protocol that streams
server-rendered cell grids and incremental patches for the active tab, plus a
JSON snapshot of the session tree. herdr's project rules declare generation 1
of this protocol frozen and required to stay available, so a third party can
implement a decoder from the type definitions in `src/protocol/wire.rs` and
`src/protocol/endpoint.rs` and the frozen fixtures under `tests/`, without
linking to herdr.

This would replace section 6 polling with true push at cell granularity and
make raw terminal mode feel local. It is a hub-internal change: the hub
protocol to clients does not change. Do this only if polling proves to be
the bottleneck in practice.

Key facts if picked up later: framing is u32 little-endian length prefix plus
bincode payload, 2 MB cap (32 MB for graphics). Handshake is JSON inside an
`EndpointControl` envelope with kinds `endpoint.hello.v1` and
`endpoint.welcome.v1`. Codecs `shell.snapshot.v1` (JSON), `shell.surface.v1`
(bincode), `shell.input.semantic.v1` (bincode), `shell.blob.v1`. The welcome
advertises the method list and capabilities.

---

## 12. Risks and open questions

- **Hidden bridge subcommands.** `remote-api-bridge` is undocumented. If it
  changes, multi-server over SSH falls back to running a hub per machine.
  (Partially mitigated already: the hub's `forward` transport mode needs no
  herdr-side support at all, only `ssh` and the remote socket — see §3.7.)
- **API stability.** The schema carries `protocol` and `schema_version`.
  The hub should check both at connect and refuse or warn on mismatch.
  Record the tested herdr version in this repo.
- **Polling cost with many viewed panes.** Each poll is one local socket
  connection and a full text read. Keep viewed-pane counts bounded by the
  client viewport and measure before optimizing.
- **`agent.prompt` on blocked agents** is refused by design. The UI must
  make the blocked state and the toolbar obvious so users are not confused
  by a rejected send.
- **Kitty graphics** are not available through the JSON API for reading.
  Panes that show images will show placeholder text until section 11.
- **Duplicate submissions on timeout.** Enforce the read-before-retry rule
  in the hub, not just the client.
- **Windows hosts** use named pipes. Support Linux and macOS servers first.

---

## 13. Reference

- herdr repo: https://github.com/herdrdev/herdr
- Local checkout used for this analysis: `~/Repos/Github/herdr`
- Docs read: `docs/next/website/src/content/docs/socket-api.mdx`,
  `agent-automation.mdx`, `persistence-remote.mdx`,
  `connecting-machines.mdx`, `concepts.mdx`
- Exported schema in herdr repo: `docs/next/api/herdr-api.schema.json`
- herdr source files that back section 3: `src/api/server.rs`,
  `src/api/schema/*.rs`, `src/api/subscriptions.rs`, `src/session.rs`,
  `src/main.rs` (bridge subcommands), `src/remote/attach.rs`,
  `src/protocol/wire.rs`, `src/protocol/endpoint.rs`
