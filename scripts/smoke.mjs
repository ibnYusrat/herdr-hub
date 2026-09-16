#!/usr/bin/env node
// Hub smoke test: hello → welcome → snapshot → view → screen → request →
// (read-only against the live server). Verifies the whole stack without a
// browser. Usage: node scripts/smoke.mjs [--url ws://127.0.0.1:8787/ws]
// Reads the token from stdin (--token -), $HUBR_HUB_TOKEN, or `herdr-hub token`.

import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
function arg(name, def) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
}
const url = arg("url", "ws://127.0.0.1:8787/ws");
const insecure = args.includes("--insecure"); // accept self-signed TLS certs

let token = process.env.HERDR_HUB_TOKEN;
if (!token || token === "-") {
  try {
    token = readFileSync(0, "utf8").trim();
  } catch {
    /* no stdin */
  }
}
if (!token) {
  console.error("no token: pipe it (herdr-hub token | node scripts/smoke.mjs) or set HERDR_HUB_TOKEN");
  process.exit(1);
}

// Minimal RFC6455 client over node:http(s) — avoids a WS dependency.
import http from "node:http";
import https from "node:https";
import crypto from "node:crypto";

function connect(urlStr, { insecure = false } = {}) {
  return new Promise((resolve, reject) => {
    const u = new URL(urlStr);
    const key = crypto.randomBytes(16).toString("base64");
    const isTls = u.protocol === "wss:";
    const mod = isTls ? https : http;
    const req = mod.request({
      hostname: u.hostname,
      port: u.port || (isTls ? 443 : 80),
      path: u.pathname + u.search,
      ...(isTls && insecure ? { rejectUnauthorized: false } : {}),
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
        // masked (client→server only; server frames are unmasked)
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

async function main() {
  const { socket } = await connect(url, { insecure });
  const reader = makeReader();
  socket.on("data", (c) => reader.push(c));

  async function recv(type, timeoutMs = 15000, filter = () => true) {
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
      // tolerate interleaved frames (events, pongs, late screens)
    }
  }

  // 1. hello → welcome
  sendFrame(socket, { type: "hello", protocol: 1, token, client: { name: "smoke", version: "0" } });
  const welcome = await recv("welcome");
  console.log("✓ welcome:", welcome.hub.name, welcome.hub.version, "| servers:", welcome.servers.map((s) => `${s.id}=${s.status}`).join(" "));
  const server = welcome.servers.find((s) => s.status === "online")?.id ?? welcome.servers[0]?.id;
  if (!server) throw new Error("no servers configured");

  // 2. snapshot
  const snap = await recv("snapshot");
  console.log("✓ snapshot:", snap.server, "gen", snap.generation, "| workspaces:", snap.state.workspaces.length, "panes:", snap.state.panes.length);

  // 3. view (focused pane first, then any other) → screen frame. The live
  // session churns: a pane can close between snapshot and view, so try the
  // next candidate instead of failing.
  const candidates = [
    snap.state.focused.pane,
    ...snap.state.panes.map((p) => p.pane_id),
  ].filter((p, i, a) => p && a.indexOf(p) === i);
  let screened = false;
  for (const pane of candidates.slice(0, 4)) {
    sendFrame(socket, {
      type: "view",
      server,
      active: { workspace: snap.state.focused.workspace, tab: snap.state.focused.tab },
      panes: [{ pane, focused: true }],
      scrollback: [],
    });
    try {
      const screen = await recv("screen", 8000, (m) => m.pane === pane);
      const shown = screen.mode === "full" ? screen.lines.length : screen.changes.length;
      console.log("✓ screen:", screen.pane, screen.mode, `${screen.rows}x${screen.cols}`, "rev", screen.revision, `(${shown} ${screen.mode === "full" ? "lines" : "changes"})`);
      screened = true;
      break;
    } catch {
      console.log(`- pane ${pane} yielded no screen (churn?); trying next`);
    }
  }
  if (!screened && candidates.length) throw new Error("no pane produced a screen frame");

  // 4. request (read-only-ish: tab.focus on the already-focused tab is a no-op)
  sendFrame(socket, { type: "request", id: "smoke1", server, action: "tab.focus", params: { tab_id: snap.state.focused.tab } });
  const resp = await recv("response");
  if (resp.id !== "smoke1") throw new Error("response id mismatch");
  console.log("✓ request:", resp.ok ? "ok" : `error ${resp.error?.code}`);

  // 5. ping → pong
  sendFrame(socket, { type: "ping" });
  await recv("pong");
  console.log("✓ ping/pong");

  console.log("\nALL GREEN");
  socket.destroy();
  process.exit(0);
}

main().catch((e) => {
  console.error("SMOKE FAILED:", e.message);
  process.exit(1);
});
