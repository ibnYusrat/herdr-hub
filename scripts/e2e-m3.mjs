#!/usr/bin/env node
// M3 gate: end-to-end MUTATING test — must run ONLY against a throwaway
// herdr session (herdr --session hub-test), never the default session.
// Flow: workspace.create → pane.split → send text + enter → screen shows
// the echo → pane.rename → allowlist rejects a forbidden action →
// pane.close → tab.close → workspace.close → gone.
// Usage: node scripts/e2e-m3.mjs --url ws://127.0.0.1:8788/ws

import { readFileSync } from "node:fs";
import http from "node:http";
import crypto from "node:crypto";

const args = process.argv.slice(2);
function arg(name, def) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
}
const url = arg("url", "ws://127.0.0.1:8788/ws");

let token = process.env.HERDR_HUB_TOKEN;
if (!token) {
  try {
    token = readFileSync(0, "utf8").trim();
  } catch {
    /* no stdin */
  }
}
if (!token) {
  console.error("no token: pipe it (herdr-hub token | node scripts/e2e-m3.mjs) or set HERDR_HUB_TOKEN");
  process.exit(1);
}

// --- minimal RFC6455 client (same as smoke.mjs) -------------------------

function connect(urlStr) {
  return new Promise((resolve, reject) => {
    const u = new URL(urlStr);
    const key = crypto.randomBytes(16).toString("base64");
    const req = http.request({
      hostname: u.hostname,
      port: u.port || 80,
      path: u.pathname + u.search,
      headers: {
        Connection: "Upgrade",
        Upgrade: "websocket",
        "Sec-WebSocket-Key": key,
        "Sec-WebSocket-Version": "13",
      },
    });
    req.on("upgrade", (res, socket, head) => resolve({ socket, head }));
    req.on("error", reject);
    req.end();
  });
}

function sendFrame(socket, obj) {
  const payload = Buffer.from(JSON.stringify(obj));
  const len = payload.length;
  const mask = crypto.randomBytes(4);
  const masked = Buffer.allocUnsafe(len);
  for (let i = 0; i < len; i++) masked[i] = payload[i] ^ mask[i & 3];
  let header;
  if (len < 126) {
    header = Buffer.from([0x81, 0x80 | len]);
  } else if (len < 65536) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  socket.write(Buffer.concat([header, mask, masked]));
}

function makeReader() {
  let buf = Buffer.alloc(0);
  const queue = [];
  const waiters = [];
  return {
    push(chunk) {
      buf = Buffer.concat([buf, chunk]);
      drain();
    },
    next() {
      if (queue.length) return Promise.resolve(queue.shift());
      return new Promise((resolve) => waiters.push(resolve));
    },
  };
  function drain() {
    while (true) {
      if (buf.length < 2) return;
      const opcode = buf[0] & 0x0f;
      let len = buf[1] & 0x7f;
      let off = 2;
      if (len === 126) {
        if (buf.length < 4) return;
        len = buf.readUInt16BE(2);
        off = 4;
      } else if (len === 127) {
        if (buf.length < 10) return;
        len = Number(buf.readBigUInt64BE(2));
        off = 10;
      }
      if (buf[1] & 0x80) {
        if (buf.length < off + 4 + len) return;
        off += 4;
      }
      if (buf.length < off + len) return;
      const payload = buf.subarray(off, off + len);
      buf = buf.subarray(off + len);
      const frame = { opcode, payload };
      const w = waiters.shift();
      if (w) w(frame);
      else queue.push(frame);
    }
  }
}

// --- hub session helper --------------------------------------------------

let seq = 0;
function session(socket, reader) {
  async function recv(type, timeoutMs = 10000, filter = () => true) {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error(`timeout waiting for ${type}`);
      const frame = await Promise.race([
        reader.next(),
        new Promise((_, rej) => setTimeout(() => rej(new Error(`timeout waiting for ${type}`)), remaining)),
      ]);
      if (frame.opcode === 0x8) throw new Error(`connection closed (code ${frame.payload.readUInt16BE(0)})`);
      if (frame.opcode !== 0x1) continue;
      const msg = JSON.parse(frame.payload.toString());
      if (msg.type === "error") throw new Error(`hub error: ${msg.error?.code} ${msg.error?.message}`);
      if (msg.type === type && filter(msg)) return msg;
    }
  }

  async function request(action, params, expectOk = true) {
    const id = `e2e-${++seq}`;
    sendFrame(socket, { type: "request", id, server, action, params });
    const resp = await recv("response", 10000, (m) => m.id === id);
    if (expectOk && !resp.ok) {
      throw new Error(`${action} failed: ${resp.error?.code} ${resp.error?.message}`);
    }
    return resp;
  }

  const server = "hub-test";
  return { recv, request };
}

/** id extraction that tolerates nested (`data.pane.pane_id`) and flat envelopes */
function idOf(data, kind) {
  if (!data) return null;
  if (kind.startsWith("workspace")) return data.workspace?.workspace_id ?? data.workspace_id ?? null;
  if (kind.startsWith("tab")) return data.tab?.tab_id ?? data.tab_id ?? null;
  if (kind.startsWith("pane")) return data.pane?.pane_id ?? data.pane_id ?? null;
  return null;
}

async function main() {
  const { socket } = await connect(url);
  const reader = makeReader();
  socket.on("data", (c) => reader.push(c));
  const { recv, request } = session(socket, reader);

  // 1. hello → welcome
  sendFrame(socket, { type: "hello", protocol: 1, token, client: { name: "e2e-m3", version: "0" } });
  const welcome = await recv("welcome");
  const online = welcome.servers.filter((s) => s.status === "online").map((s) => s.id);
  console.log("✓ welcome | servers:", welcome.servers.map((s) => `${s.id}=${s.status}`).join(" "));
  if (!online.includes("hub-test")) throw new Error("hub-test server not online — refusing to mutate anything else");

  async function waitEvent(kind, matchId, timeoutMs = 10000) {
    return recv("event", timeoutMs, (m) => m.event === kind && (!matchId || idOf(m.data, kind) === matchId));
  }

  // 2. workspace.create (label, focus)
  const marker = `M3GATE-${crypto.randomBytes(3).toString("hex")}`;
  const created = await request("workspace.create", { label: `m3gate-${marker}`, focus: true });
  const wsId = created.result?.workspace?.workspace_id ?? created.result?.workspace_id;
  if (!wsId) throw new Error(`workspace.create returned no workspace_id: ${JSON.stringify(created.result)}`);
  await waitEvent("workspace_created", wsId);
  console.log(`✓ workspace.create → ${wsId} (${created.result?.label ?? created.result?.number})`);

  // The new workspace arrives with its first tab + a shell pane. Wait for a
  // pane in that workspace: pane_created events carry the record.
  const paneEv = await recv("event", 10000, (m) => {
    if (m.event !== "pane_created" && m.event !== "pane_updated") return false;
    const p = m.data?.pane ?? m.data;
    return p?.workspace_id === wsId;
  });
  const firstPane = paneEv.data?.pane ?? paneEv.data;
  console.log(`✓ workspace has pane ${firstPane.pane_id} (tab ${firstPane.tab_id})`);

  // 3. pane.split right, focus the new pane
  const split = await request("pane.split", { direction: "right", target_pane_id: firstPane.pane_id, focus: true });
  const splitPaneId = split.result?.pane?.pane_id ?? split.result?.pane_id;
  if (!splitPaneId) throw new Error(`pane.split returned no pane_id: ${JSON.stringify(split.result)}`);
  await waitEvent("pane_created", splitPaneId);
  console.log(`✓ pane.split → ${splitPaneId}`);

  // 4. send a line and see the echo on screen
  sendFrame(socket, {
    type: "view",
    server: "hub-test",
    active: { workspace: wsId, tab: firstPane.tab_id },
    panes: [{ pane: splitPaneId, focused: true }],
    scrollback: [],
  });
  await request("pane.send_text", { pane_id: splitPaneId, text: `echo ${marker}` });
  await request("pane.send_keys", { pane_id: splitPaneId, keys: ["enter"] });
  console.log(`✓ sent: echo ${marker} + enter`);

  const deadline = Date.now() + 10000;
  let echoed = false;
  while (!echoed && Date.now() < deadline) {
    const screen = await recv("screen", Math.max(1, deadline - Date.now()), (m) => m.pane === splitPaneId);
    const lines =
      screen.mode === "full" ? screen.lines : screen.changes.map((c) => c.text);
    // the command line shows `echo MARKER`; the output is the bare marker on
    // its own line — require that, not just any occurrence
    if (lines.some((l) => l.trim() === marker)) echoed = true;
  }
  if (!echoed) throw new Error(`screen never showed the bare echo output of ${marker}`);
  console.log(`✓ screen shows echo output (${marker})`);

  // 5. pane.rename
  await request("pane.rename", { pane_id: splitPaneId, label: "m3-renamed" });
  await waitEvent("pane_updated", splitPaneId);
  console.log("✓ pane.rename → m3-renamed");

  // 6. allowlist: a non-allowlisted herdr method must be refused by the hub
  const evil = await request("session.snapshot", {}, false);
  if (evil.ok || evil.error?.code !== "hub.action_not_allowed") {
    throw new Error(`forbidden action not rejected: ${JSON.stringify(evil)}`);
  }
  console.log(`✓ allowlist rejects forbidden action (${evil.error.code})`);

  // 7. pane.close
  await request("pane.close", { pane_id: splitPaneId });
  await waitEvent("pane_closed", splitPaneId);
  console.log(`✓ pane.close ${splitPaneId}`);

  // 8. tab.create (a second tab, so closing the first doesn't auto-close
  //    the workspace — herdr removes workspaces that run out of tabs)
  const tab = await request("tab.create", { workspace_id: wsId, label: "second", focus: false });
  const tabId2 = tab.result?.tab?.tab_id ?? tab.result?.tab_id;
  if (!tabId2) throw new Error(`tab.create returned no tab_id: ${JSON.stringify(tab.result)}`);
  await waitEvent("tab_created", tabId2);
  console.log(`✓ tab.create → ${tabId2}`);

  // 9. tab.close (the first tab; the workspace keeps the second)
  await request("tab.close", { tab_id: firstPane.tab_id });
  await waitEvent("tab_closed", firstPane.tab_id);
  console.log(`✓ tab.close ${firstPane.tab_id}`);

  // 10. workspace.close (close_group: false — no worktree group here)
  await request("workspace.close", { workspace_id: wsId, close_group: false });
  await waitEvent("workspace_closed", wsId);
  console.log(`✓ workspace.close ${wsId}`);

  console.log("\nM3 GATE GREEN");
  socket.destroy();
  process.exit(0);
}

main().catch((e) => {
  console.error("M3 GATE FAILED:", e.message);
  console.error("clean up manually if a m3gate workspace lingers in the hub-test session");
  process.exit(1);
});
