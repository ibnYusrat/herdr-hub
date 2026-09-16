// Unit tests for the transcript window-diff (run with `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { advance, attachRecentStream, transcript, MAX_LINES } from "./transcript.ts";
import { screens } from "./screens.ts";
import { paneModes } from "./footer.ts";

const w = (arr: string[]) => arr;

test("seed: first window becomes the transcript", () => {
  const st = advance(null, w(["a", "b"]));
  assert.deepEqual(st.lines, ["a", "b"]);
  assert.equal(st.headTruncated, false);
});

test("pure append within the window", () => {
  let st = advance(null, w(["a", "b"]));
  st = advance(st, w(["a", "b", "c"]));
  assert.deepEqual(st.lines, ["a", "b", "c"]);
});

test("no new output is a no-op (same object)", () => {
  const st = advance(null, w(["a", "b"]));
  assert.equal(advance(st, w(["a", "b"])), st);
});

test("empty window after content leaves the transcript alone", () => {
  const st = advance(null, w(["a", "b"]));
  assert.equal(advance(st, w([])), st);
});

test("window slide (older lines fall off the 500-line cap)", () => {
  // Buffer grew past the cap: the window start moved by one line.
  let st = advance(null, w(["a", "b", "c"]));
  st = advance(st, w(["b", "c", "d"]));
  assert.deepEqual(st.lines, ["a", "b", "c", "d"]);
});

test("slide by more than the overlap still appends only new lines", () => {
  let st = advance(null, w(["a", "b", "c", "d"]));
  // Window slid 2: ["c","d"] survived, ["e","f"] are new.
  st = advance(st, w(["c", "d", "e", "f"]));
  assert.deepEqual(st.lines, ["a", "b", "c", "d", "e", "f"]);
});

test("complete rewrite keeps history and adds a separator", () => {
  let st = advance(null, w(["old", "stuff"]));
  st = advance(st, w(["totally", "new"]));
  assert.deepEqual(st.lines, ["old", "stuff", "", "⋯ history reset ⋯", "", "totally", "new"]);
});

test("memory cap drops the head and flags it", () => {
  const big = Array.from({ length: MAX_LINES }, (_, i) => `l${i}`);
  let st = advance(null, big);
  st = advance(st, big.concat(["tail"]));
  assert.equal(st.lines.length, MAX_LINES);
  assert.equal(st.lines[st.lines.length - 1], "tail");
  assert.equal(st.headTruncated, true);
});

test("color-only restyle is not duplicated (alignment sees visible text)", () => {
  let st = advance(null, w(["\x1b[31m- removed\x1b[0m", "ctx"]));
  // Same visible lines, different styling: nothing new to append.
  st = advance(st, w(["\x1b[32m- removed\x1b[0m", "ctx"]));
  assert.deepEqual(st.lines, ["\x1b[31m- removed\x1b[0m", "ctx"]);
});

test("ansi lines are kept raw for rendering and appended verbatim", () => {
  let st = advance(null, w(["\x1b[32m+ added\x1b[0m"]));
  st = advance(st, w(["\x1b[32m+ added\x1b[0m", "\x1b[31m- gone\x1b[0m"]));
  assert.deepEqual(st.lines, ["\x1b[32m+ added\x1b[0m", "\x1b[31m- gone\x1b[0m"]);
});

test("plain-to-ansi restyle of the same text does not duplicate", () => {
  let st = advance(null, w(["same text"]));
  st = advance(st, w(["\x1b[1msame text\x1b[0m"]));
  assert.equal(st.lines.length, 1);
});

test("the console footer never enters the transcript (feed wiring)", () => {
  const srv = "s-footer";
  const pane = "w9:p1";
  const E = "\x1b";
  const rule = `${E}[0m${E}[38;2;136;136;136m${"─".repeat(61)}${E}[0m`;
  const inputRow = `${E}[0m${E}[38;2;153;153;153m❯ ${E}[0m`;
  const hint = `  ${E}[0m${E}[38;2;255;193;7m⏵⏵ auto mode on${E}[0m${E}[38;2;153;153;153m (shift+tab to cycle) · esc to interrupt · ← for agents${E}[0m`;
  const frame = (revision: number, lines: string[]) =>
    screens.apply({
      type: "screen",
      server: srv,
      pane,
      revision,
      rows: lines.length,
      cols: 80,
      mode: "full",
      lines,
      source: "recent",
    });

  frame(1, ["output a", "", rule, inputRow, rule, hint]);
  const unsub = attachRecentStream(srv, pane); // replays the stored snapshot
  frame(2, ["output a", "", "output b", rule, inputRow, rule, hint]);
  unsub();

  assert.deepEqual(transcript.get(srv, pane)?.lines, ["output a", "", "output b"]);

  const modes: (string | null)[] = [];
  const unsubMode = paneModes.subscribe(srv, pane, (m) => modes.push(m?.label ?? null));
  unsubMode();
  assert.equal(modes[0], "⏵⏵ auto mode on");
});
