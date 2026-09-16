import { test } from "node:test";
import assert from "node:assert/strict";
import { parseAnsi, stripAnsi } from "./ansi.ts";

test("plain text passes through as one unstyled segment", () => {
  assert.deepEqual(parseAnsi("hello world"), [{ text: "hello world", style: {} }]);
});

test("basic and bright foreground colors", () => {
  assert.deepEqual(parseAnsi("\x1b[31mred"), [{ text: "red", style: { fg: "#800000" } }]);
  assert.deepEqual(parseAnsi("\x1b[91mRED"), [{ text: "RED", style: { fg: "#ff0000" } }]);
});

test("reset clears accumulated style", () => {
  const segs = parseAnsi("\x1b[1;31mboldred\x1b[0mplain");
  assert.deepEqual(segs, [
    { text: "boldred", style: { bold: true, fg: "#800000" } },
    { text: "plain", style: {} },
  ]);
});

test("256-color and RGB via 38;5 / 38;2", () => {
  assert.deepEqual(parseAnsi("\x1b[38;5;196mx"), [
    { text: "x", style: { fg: "rgb(255,0,0)" } },
  ]);
  assert.deepEqual(parseAnsi("\x1b[38;2;10;20;30mx"), [
    { text: "x", style: { fg: "rgb(10,20,30)" } },
  ]);
});

test("background colors", () => {
  assert.deepEqual(parseAnsi("\x1b[41mx"), [{ text: "x", style: { bg: "#800000" } }]);
  assert.deepEqual(parseAnsi("\x1b[48;5;16mx"), [
    { text: "x", style: { bg: "rgb(0,0,0)" } },
  ]);
});

test("attributes set and clear", () => {
  const segs = parseAnsi("\x1b[1;3;4mA\x1b[22;23;24mB");
  assert.deepEqual(segs[0].style, { bold: true, italic: true, underline: true });
  assert.deepEqual(segs[1].style, {});
});

test("reverse swaps fg and bg", () => {
  const segs = parseAnsi("\x1b[31;44;7mx");
  assert.deepEqual(segs[0].style, { fg: "#000080", bg: "#800000" });
});

test("empty SGR acts as reset", () => {
  const segs = parseAnsi("\x1b[31mred\x1b[mplain");
  assert.deepEqual(segs[1].style, {});
});

test("non-SGR CSI and OSC sequences are dropped", () => {
  const segs = parseAnsi("\x1b[2J\x1b[H\x1b]0;title\x07ok\x1b K!");
  assert.deepEqual(segs, [{ text: "ok!", style: {} }]);
});

test("newlines flow through as ordinary characters", () => {
  const segs = parseAnsi("\x1b[32ma\nb");
  assert.deepEqual(segs, [{ text: "a\nb", style: { fg: "#008000" } }]);
});

test("stripAnsi removes styling but keeps text", () => {
  assert.equal(stripAnsi("\x1b[1;32m+ added\x1b[0m"), "+ added");
  assert.equal(stripAnsi("\x1b[38;2;1;2;3mrgb\x1b[0m"), "rgb");
  assert.equal(stripAnsi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link");
});

test("html-significant characters survive as plain text", () => {
  assert.deepEqual(parseAnsi("<b>&amp;"), [{ text: "<b>&amp;", style: {} }]);
});
