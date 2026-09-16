#!/usr/bin/env node
// M4 SSH gate: two servers (local + ssh) in one hub. Verifies aggregation,
// then the dim/recover cycle: killing the REMOTE throwaway session must dim
// the ssh server (server_status ≠ online) and restarting it must recover
// with a fresh snapshot and no duplicate tree entries.
// The remote session is a throwaway (`herdr --session hub-test` on the ssh
// host) — the remote default session is never touched.
// Usage: node scripts/e2e-m4-ssh.mjs --url ws://127.0.0.1:8789/ws --host <ssh-host>

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import http from "node:http";
import crypto from "node:crypto";

const args = process.argv.slice(2);
function arg(name, def) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
}
const url = arg("url", "ws://127.0.0.1:8789/ws");
const host = arg("host", "");
if (!host) {
  console.error("missing --host: the ssh host running the throwaway herdr session");
  process.exit(1);
}

let token = process.env.HERDR_HUB_TOKEN;
if (!token) {
  try {
    token = readFileSync(0, "utf8").trim();
  } catch {
    /* no stdin */
  }
}
if (!token) {
  console.error("no token: pipe it (herdr-hub token | node scripts/e2e-m4-ssh.mjs) or set HERDR_HUB_TOKEN");
  process.exit(1);
}

const ssh = (remoteCmd) =>
  execFileSync("ssh", ["-o", "BatchMode=yes", host, remoteCmd], {
    timeout: 20000,
    stdio: ["ignore", "pipe", "pipe"],
  }).toString();

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
  const frameWaiters = [];
  return {
    push(chunk) {
      buf = Buffer.concat([buf, chunk]);
      drain();
    },
    next() {
      if (queue.length) return Promise.resolve(queue.shift());
      return new Promise((resolve) => frameWaiters.push(resolve));
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
      const w = frameWaiters.shift();
      if (w) w(frame);
      else queue.push(frame);
    }
  }
}

// Single dispatch pump: one consumer of raw frames, messages routed to
// whichever recv() matches (or parked in `pending`). Competing frame readers
// would steal each other's messages — e.g. the snapshot watcher eating the
// welcome frame.
const pending = [];
const waiters = []; // {match, resolve, reject}

function dispatch(msg) {
  // Deliver to EVERY matching waiter: each recv() is an independent
  // subscription (the snapshot tracker + the step-by-step flow both want the
  // same snapshot). Messages nobody wants park in pending.
  let delivered = false;
  for (const w of [...waiters]) {
    if (w.match(msg)) {
      delivered = true;
      w.resolve(msg);
    }
  }
  if (!delivered) pending.push(msg);
}

function failAll(err) {
  for (const w of waiters.splice(0)) w.reject(err);
}

async function pump(reader) {
  for (;;) {
    const frame = await reader.next();
    if (frame.opcode === 0x8) {
      throw new Error(`connection closed (code ${frame.payload.readUInt16BE(0)})`);
    }
    if (frame.opcode !== 0x1) continue;
    dispatch(JSON.parse(frame.payload.toString()));
  }
}

async function main() {
  const { socket } = await connect(url);
  const reader = makeReader();
  socket.on("data", (c) => reader.push(c));
  const pumper = pump(reader);
  pumper.catch((e) => failAll(e));

  function recv(type, timeoutMs = 15000, filter = () => true) {
    const hit = pending.findIndex((m) => m.type === type && filter(m));
    if (hit >= 0) return Promise.resolve(pending.splice(hit, 1)[0]);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        const i = waiters.indexOf(w);
        if (i >= 0) waiters.splice(i, 1);
        reject(new Error(`timeout waiting for ${type}`));
      }, timeoutMs);
      const w = {
        // error frames reject whichever caller is waiting
        match: (m) => m.type === "error" || (m.type === type && filter(m)),
        resolve: (m) => {
          clearTimeout(timer);
          const i = waiters.indexOf(w);
          if (i >= 0) waiters.splice(i, 1);
          if (m.type === "error") {
            reject(new Error(`hub error: ${m.error?.code} ${m.error?.message}`));
          } else {
            resolve(m);
          }
        },
        reject,
      };
      waiters.push(w);
    });
  }

  // Track the latest snapshot per server so dupes can be detected later.
  // Stores the whole message (generation included).
  const latest = new Map();
  const watchSnapshots = (async () => {
    for (;;) {
      const m = await recv("snapshot", 600000);
      latest.set(m.server, m);
    }
  })();
  watchSnapshots.catch(() => {});

  // Wait for a snapshot of `server` newer than `afterGen` (default: any).
  // Reads the tracker instead of recv(): the standing watcher consumes every
  // snapshot the instant it arrives, so a later recv() would miss it.
  async function waitForSnapshot(server, timeoutMs, afterGen = -1) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const s = latest.get(server);
      if (s && s.generation > afterGen) return s;
      await new Promise((r) => setTimeout(r, 100));
    }
    throw new Error(`timeout waiting for snapshot of ${server}`);
  }

  // 1. hello → welcome: both servers, both online.
  sendFrame(socket, { type: "hello", protocol: 1, token, client: { name: "e2e-m4", version: "0" } });
  const welcome = await recv("welcome");
  const listed = welcome.servers.map((s) => `${s.id}=${s.status}`).join(" ");
  console.log("✓ welcome | servers:", listed);
  const ids = welcome.servers.map((s) => s.id);
  const remoteId = ids.find((i) => i !== "local");
  if (ids.length !== 2 || !remoteId) {
    throw new Error(`expected two servers (local + one remote), got ${ids.join(",")}`);
  }
  const offline = welcome.servers.filter((s) => s.status !== "online").map((s) => s.id);
  if (offline.length) throw new Error(`servers not online at start: ${offline.join(",")}`);

  // 2. A snapshot for each server (both trees arrive).
  for (const id of ["local", remoteId]) {
    const snap = await waitForSnapshot(id, 20000);
    console.log(`✓ snapshot ${id}: workspaces=${snap.state.workspaces.length} panes=${snap.state.panes.length}`);
  }

  // 3. Kill the REMOTE throwaway session → the ssh server must dim.
  console.log(`· stopping remote throwaway session on ${host}…`);
  ssh("herdr session stop hub-test");
  const dim = await recv("server_status", 30000, (m) => m.server === remoteId && m.status !== "online");
  console.log(`✓ dimmed: ${remoteId} = ${dim.status}${dim.detail ? ` (${dim.detail})` : ""}`);

  // The local server must NOT dim (isolation).
  // (No server_status for local is expected; a later online snapshot proves it stayed alive.)

  // 4. Restart the remote session → recover with a fresh snapshot, no dupes.
  const genBefore = latest.get(remoteId)?.generation ?? 0;
  console.log(`· restarting remote throwaway session on ${host}…`);
  ssh("setsid herdr --session hub-test server >/tmp/hub-test.log 2>&1 < /dev/null & sleep 2");
  const back = await recv("server_status", 60000, (m) => m.server === remoteId && m.status === "online");
  console.log(`✓ recovered: ${remoteId} = ${back.status}`);

  // The fresh snapshot may land before or after the online status — wait for
  // it via the tracker (generation strictly newer than the pre-kill one).
  const snap = await waitForSnapshot(remoteId, 30000, genBefore);
  const wsIds = snap.state.workspaces.map((w) => w.workspace_id);
  const dupes = wsIds.filter((id, i) => wsIds.indexOf(id) !== i);
  if (dupes.length) throw new Error(`duplicate workspaces after recovery: ${dupes.join(",")}`);
  console.log(`✓ fresh snapshot, no duplicate tree entries (${wsIds.length} workspaces)`);

  console.log("\nM4 SSH GATE GREEN");
  socket.destroy();
  process.exit(0);
}

main().catch((e) => {
  console.error("M4 SSH GATE FAILED:", e.message);
  console.error(`clean up manually: ssh ${host} 'herdr session stop hub-test && herdr session delete hub-test'`);
  process.exit(1);
});
