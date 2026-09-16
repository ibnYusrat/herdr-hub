// Unit tests for console-footer stripping (run with `npm test`). Fixtures
// are byte-accurate to panes captured from a live herdr session (idle and
// working agent states, full-width panes).
import { test } from "node:test";
import assert from "node:assert/strict";
import { splitFooter, paneModes } from "./footer.ts";

const E = "\x1b";
const dim = `${E}[0m${E}[38;2;136;136;136m`;
const gray = `${E}[38;2;153;153;153m`;

// Box rules: a long dash run, the top one carrying the workspace title.
const rule = (title?: string) =>
  `${dim}${"─".repeat(61)}${title ? ` ${title} ` : ""}${"─".repeat(2)}${E}[0m`;

// Working state (captured: auto mode, styled input row, esc-to-interrupt).
const WORKING_FOOTER = [
  rule("implement-herdr-hub"),
  `${E}[0m${E}[38;2;153;153;153m❯ ${E}[0m`,
  rule(),
  `  ${E}[0m${E}[38;2;255;193;7m⏵⏵ auto mode on${E}[0m${gray} (shift+tab to cycle) · esc to interrupt · ← for agents${E}[0m`,
];

// Idle state (captured: plain input row, bypass mode with extra segments).
const IDLE_FOOTER = [
  rule("example workspace"),
  "❯ ",
  rule(),
  `  ${E}[0m${E}[38;2;255;107;128m⏵⏵ bypass permissions on${E}[0m${E}[38;2;153;153;153m · ${E}[0m${E}[38;2;0;204;204m1 shell${E}[0m${E}[38;2;153;153;153m · ← for agents · ↓ to manage${E}[0m`,
];

const CONTENT = [
  `${E}[0m${E}[38;2;153;153;153m● ${E}[0mReading web/src/components/PaneGrid.tsx${E}[38;2;153;153;153m · 4s${E}[0m`,
  "",
];

test("working-state footer (4 lines) is stripped and the mode parsed", () => {
  const { body, footer, mode } = splitFooter([...CONTENT, ...WORKING_FOOTER]);
  assert.deepEqual(body, CONTENT);
  assert.equal(footer!.length, 4);
  assert.deepEqual(mode, { label: "⏵⏵ auto mode on", kind: "auto" });
});

test("idle-state footer with plain input row and extra hint segments", () => {
  const { body, mode } = splitFooter([...CONTENT, ...IDLE_FOOTER]);
  assert.deepEqual(body, CONTENT);
  assert.deepEqual(mode, { label: "⏵⏵ bypass permissions on", kind: "bypass" });
});

test("content without a hint line is returned untouched", () => {
  const lines = ["some output", "", "  trailing blank above"];
  const { body, footer, mode } = splitFooter(lines);
  assert.equal(body, lines); // same reference — nothing stripped
  assert.equal(footer, null);
  assert.equal(mode, null);
});

test("a bare dash rule at the bottom is NOT a footer without the hint line", () => {
  const lines = ["text", rule(), "❯ "];
  const { body, footer } = splitFooter(lines);
  assert.equal(body, lines);
  assert.equal(footer, null);
});

test("hint line alone (narrow pane, box collapsed) still strips", () => {
  const { body, footer, mode } = splitFooter(["text", WORKING_FOOTER[3]]);
  assert.deepEqual(body, ["text"]);
  assert.equal(footer!.length, 1);
  assert.equal(mode!.kind, "auto");
});

test("hint + bottom rule without an input row strips two lines", () => {
  const { body, footer, mode } = splitFooter(["text", rule(), WORKING_FOOTER[3]]);
  assert.deepEqual(body, ["text"]);
  assert.equal(footer!.length, 2);
  assert.equal(mode!.kind, "auto");
});

test("truncated hint is caught by the mode glyph anchor", () => {
  const { body, footer } = splitFooter(["text", `  ${E}[0m${E}[38;2;255;193;7m⏵⏵ auto mode on${E}[0m`]);
  assert.deepEqual(body, ["text"]);
  assert.equal(footer!.length, 1);
});

test("plain-pane scrollback (shell output) is untouched", () => {
  const lines = ["$ cargo build", "   Compiling hub v0.1.0", "    Finished dev profile", "$ "];
  const { body, footer } = splitFooter(lines);
  assert.equal(body, lines);
  assert.equal(footer, null);
});

test("empty input is fine", () => {
  const { body, footer, mode } = splitFooter([]);
  assert.deepEqual(body, []);
  assert.equal(footer, null);
  assert.equal(mode, null);
});

test("footer-only screen still yields the mode", () => {
  const { body, mode } = splitFooter(WORKING_FOOTER);
  assert.deepEqual(body, []);
  assert.equal(mode!.label, "⏵⏵ auto mode on");
});

test("paneModes store notifies on change, not on repeats", () => {
  const seen: (string | null)[] = [];
  const unsub = paneModes.subscribe("s", "w1:p1", (m) => seen.push(m?.label ?? null));
  paneModes.note("s", "w1:p1", { label: "⏵⏵ auto mode on", kind: "auto" });
  paneModes.note("s", "w1:p1", { label: "⏵⏵ auto mode on", kind: "auto" }); // no-op
  paneModes.note("s", "w1:p1", null);
  unsub();
  paneModes.note("s", "w1:p1", { label: "x", kind: "other" }); // after unsub
  assert.deepEqual(seen, [null, "⏵⏵ auto mode on", null]);
});
