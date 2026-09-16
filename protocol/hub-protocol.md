# herdr-hub client protocol v1

This is the contract between herdr-hub and its clients (web today, mobile
later). Clients speak only this protocol; they never see herdr's wire details.

- Transport: WebSocket, JSON text frames, one JSON object per frame.
- Every frame is discriminated by `type`.
- Protocol version: `1`, carried in `hello` and `welcome`. On mismatch the hub
  replies with an `error`-bearing `response`-style message and closes.
- **Id addressing**: herdr ids are passed through verbatim (`w1`, `w1:t1`,
  `w1:p1`). Every server-scoped frame carries a `server` field naming the
  configured server (e.g. `"local"`). The pair `(server, id)` is globally
  unique; ids alone are only unique within one server.

## Client → hub

### `hello` (first frame, required)

```json
{
  "type": "hello",
  "protocol": 1,
  "token": "<auth token>",
  "client": { "name": "web", "version": "0.1.0", "caps": ["screens", "actions", "raw_input"] }
}
```

Auth failure → the hub sends
`{"type":"response","id":null,"ok":false,"error":{"code":"hub.auth_failed","message":"..."}}`
and closes the socket. The token is never accepted via URL/query (SPEC §9).

### `view` (full replace, send on every visibility change)

```json
{
  "type": "view",
  "server": "local",
  "active": { "workspace": "w3", "tab": "w3:t1" },
  "panes": [{ "pane": "w3:p1", "focused": true }, { "pane": "w3:p2", "focused": false }],
  "scrollback": []
}
```

Declares which panes the client currently shows (drives hub-side polling
scope), which is active, and which panes have their scrollback overlay open.
Omitting a pane stops its polling. `cols`/`rows` of the client viewport are
intentionally absent: the server-rendered grid is authoritative and the client
scales it (SPEC §6).

### `request`

```json
{ "type": "request", "id": "c17", "server": "local", "action": "agent.prompt",
  "params": { "target": "w3:p1", "text": "run the tests" } }
```

`action` is a herdr method name from the allowlist below; `params` is the
verbatim herdr params object for that method. `id` is echoed in the response.

Allowlist: `workspace.create|focus|rename|close`, `worktree.create|open|remove`,
`tab.create|focus|rename|close`,
`pane.focus|zoom|rename|close|split|resize|scroll|send_text|send_keys|send_input`,
`layout.set_split_ratio`, `agent.prompt|send_keys`.

### `ping`

Hub replies `pong`. Also serves as keepalive.

## Hub → client

### `welcome`

```json
{
  "type": "welcome",
  "protocol": 1,
  "hub": { "name": "herdr-hub", "version": "0.1.0" },
  "servers": [
    { "id": "local", "label": "Local", "kind": "local", "status": "online",
      "herdr_version": "0.8.2", "protocol": 20 }
  ],
  "heartbeat_ms": 15000
}
```

Sent immediately after a successful `hello`, and again after a hub-side
reconnect. Followed (per server that is online) by a `snapshot`.

### `snapshot`

```json
{ "type": "snapshot", "server": "local", "generation": 42,
  "state": {
    "workspaces": [], "tabs": [], "panes": [], "layouts": [], "agents": [],
    "focused": { "workspace": "w3", "tab": "w3:t1", "pane": "w3:p1" }
  } }
```

Full state for one server; record shapes are herdr's verbatim
(`WorkspaceInfo`, `TabInfo`, `PaneInfo`, `PaneLayoutSnapshot`, `AgentInfo`).
Sent on join, after any hub-side resync of that server, and after a server
reconnects. Clients fully replace their copy for that server.

### `event`

```json
{ "type": "event", "server": "local", "event": "pane_agent_status_changed",
  "data": { "pane_id": "w3:p1", "workspace_id": "w3", "agent_status": "blocked" } }
```

One lifecycle delta. Kinds and payloads mirror herdr's emitted events (kinds
normalized to underscore form); apply with upsert-by-id semantics:
created/updated/layout events carry full records, moved/reordered carry whole
ordered id lists, closed/exited remove, focused is last-write-wins.

### `screen`

```json
{ "type": "screen", "server": "local", "pane": "w3:p1", "revision": 128,
  "rows": 42, "cols": 80, "mode": "full", "lines": ["[32mok[0m", "..."] }

{ "type": "screen", "server": "local", "pane": "w3:p1", "revision": 129,
  "rows": 42, "cols": 80, "mode": "rows", "changes": [{ "row": 12, "text": "..." }] }
```

ANSI content (`format: ansi`), only for panes the client declared in `view`.
`mode:"full"` replaces the whole grid; `mode:"rows"` patches lines. `scrollback`
reads arrive as separate frames with `"source":"recent"` and plain text.

### `response`

```json
{ "type": "response", "id": "c17", "server": "local", "ok": true, "result": { "type": "agent_prompted", "agent": {} } }
{ "type": "response", "id": "c18", "ok": false, "error": { "code": "agent_blocked", "message": "agent is blocked" } }
```

herdr results and errors pass through verbatim. Hub-internal failures use
`hub.*` codes: `hub.unknown_server`, `hub.action_not_allowed`,
`hub.server_offline`, `hub.prompt_in_flight`, `hub.bad_message`,
`hub.auth_failed`, `hub.protocol_unsupported`. `agent_blocked` is terminal —
neither hub nor client ever auto-retries (SPEC §7, §12).

### `server_status`

```json
{ "type": "server_status", "server": "mbp", "status": "reconnecting", "detail": "socket closed" }
```

`status`: `connecting` | `online` | `reconnecting` | `offline`. Clients dim
cached state for that server rather than discarding it (TUI parity), until a
fresh `snapshot` arrives.

### `pong`

Reply to `ping`.
