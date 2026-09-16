// Unit tests for the pure apply layer — same fixture shapes as the Rust
// state tests (hub/src/state.rs), run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyEvent, applySnapshot, emptyState } from "./apply.ts";
import type { PaneInfo } from "./types.ts";

test("snapshot installs wholesale", () => {
  const state = applySnapshot({
    workspaces: [{ workspace_id: "w1", number: 1, label: "A", focused: true, pane_count: 1, tab_count: 1, active_tab_id: "w1:t1", agent_status: "idle" }],
    tabs: [{ tab_id: "w1:t1", workspace_id: "w1", number: 1, label: "1", focused: true, pane_count: 1, agent_status: "idle" }],
    panes: [{ pane_id: "w1:p1", terminal_id: "t1", workspace_id: "w1", tab_id: "w1:t1", focused: true, agent_status: "idle", revision: 1 }],
    layouts: [],
    agents: [],
    focused: { workspace: "w1", tab: "w1:t1", pane: "w1:p1" },
  });
  assert.equal(state.workspaces["w1"].label, "A");
  assert.equal(state.focused.pane, "w1:p1");
});

const wsRec = (workspace_id: string, tab_id: string | null = null) => ({
  workspace_id, number: 1, label: workspace_id, focused: false,
  pane_count: 1, tab_count: 1, active_tab_id: tab_id, agent_status: "idle",
});
const tabRec = (tab_id: string, workspace_id: string) => ({
  tab_id, workspace_id, number: 1, label: "1", focused: false,
  pane_count: 1, agent_status: "idle",
});

test("event records are nested under keys", () => {
  let state = emptyState();
  const r1 = applyEvent(state, "workspace_created", {
    type: "workspace_created",
    workspace: { workspace_id: "w2", number: 2, label: "New", focused: false, pane_count: 0, tab_count: 0, active_tab_id: null, agent_status: "unknown" },
  });
  assert.ok(r1.state.workspaces["w2"]);
  assert.equal(r1.state.workspaces["w2"].label, "New");
  state = applyEvent(r1.state, "tab_created", {
    type: "tab_created", tab: tabRec("w2:t1", "w2"),
  }).state;

  const r2 = applyEvent(state, "pane_created", {
    type: "pane_created",
    pane: { pane_id: "w2:p1", terminal_id: "t2", workspace_id: "w2", tab_id: "w2:t1", focused: true, agent_status: "idle", revision: 1, agent: "coder" },
  });
  assert.ok(r2.paneSetChanged);
  assert.equal(r2.state.panes["w2:p1"].agent, "coder");
});

test("dotted subscription events are flat", () => {
  let state = emptyState();
  state = applyEvent(state, "workspace_created", {
    type: "workspace_created", workspace: wsRec("w1", "w1:t1"),
  }).state;
  state = applyEvent(state, "tab_created", {
    type: "tab_created", tab: tabRec("w1:t1", "w1"),
  }).state;
  state = applyEvent(state, "pane_created", {
    type: "pane_created",
    pane: { pane_id: "w1:p1", terminal_id: "t1", workspace_id: "w1", tab_id: "w1:t1", focused: true, agent_status: "idle", revision: 1 },
  }).state;
  state = applyEvent(state, "pane_agent_status_changed", {
    pane_id: "w1:p1",
    workspace_id: "w1",
    agent_status: "blocked",
  }).state;
  assert.equal(state.panes["w1:p1"].agent_status, "blocked");
});

test("closures remove and flag pane set change", () => {
  let state = emptyState();
  state = applyEvent(state, "workspace_created", {
    type: "workspace_created", workspace: wsRec("w1", "w1:t1"),
  }).state;
  state = applyEvent(state, "tab_created", {
    type: "tab_created", tab: tabRec("w1:t1", "w1"),
  }).state;
  state = applyEvent(state, "pane_created", {
    type: "pane_created",
    pane: { pane_id: "w1:p1", terminal_id: "t1", workspace_id: "w1", tab_id: "w1:t1", focused: true, agent_status: "idle", revision: 1 },
  }).state;
  const r = applyEvent(state, "pane_closed", { type: "pane_closed", pane_id: "w1:p1", workspace_id: "w1" });
  assert.ok(!r.state.panes["w1:p1"]);
  assert.ok(r.paneSetChanged);
});

test("stale and unknown events are no-ops", () => {
  const state = emptyState();
  const r = applyEvent(state, "pane_closed", { type: "pane_closed", pane_id: "gone", workspace_id: "w1" });
  assert.equal(r.state, state); // same reference: nothing changed
  const r2 = applyEvent(state, "future_kind", { whatever: true });
  assert.equal(r2.state, state);
});

const paneRec = (pane_id: string, workspace_id: string, tab_id: string): PaneInfo => ({
  pane_id, terminal_id: "term", workspace_id, tab_id, focused: false,
  agent_status: "idle", revision: 1, agent: null,
});

test("zombie pane_updated does not resurrect a removed pane", () => {
  let state = emptyState();
  state = applyEvent(state, "workspace_created", {
    type: "workspace_created", workspace: wsRec("w1", "w1:t1"),
  }).state;
  state = applyEvent(state, "tab_created", {
    type: "tab_created", tab: tabRec("w1:t1", "w1"),
  }).state;
  state = applyEvent(state, "pane_created", {
    type: "pane_created", pane: paneRec("w1:p1", "w1", "w1:t1"),
  }).state;
  state = applyEvent(state, "pane_closed", {
    type: "pane_closed", pane_id: "w1:p1", workspace_id: "w1",
  }).state;
  // herdr keeps probing panes of closed workspaces (verified live on 0.8.2).
  const r = applyEvent(state, "pane_updated", {
    type: "pane_updated", pane: { ...paneRec("w1:p1", "w1", "w1:t1"), revision: 7 },
  });
  assert.equal(r.state, state); // same reference: inert
  assert.ok(!r.state.panes["w1:p1"]);
});

test("phantom panes never enter the tree", () => {
  // Snapshot flicker: orphan panes whose workspace/tab are absent.
  const snap = applySnapshot({
    workspaces: [],
    tabs: [],
    panes: [paneRec("w1:p1", "w1", "w1:t1"), paneRec("w1:p2", "w1", "w1:t1")],
    layouts: [{ workspace_id: "w1", tab_id: "w1:t1", zoomed: false, area: { x: 0, y: 0, width: 80, height: 24 }, focused_pane_id: "w1:p1", panes: [], splits: [] }],
    agents: [],
    focused: { workspace: null, tab: null, pane: null },
  });
  assert.equal(Object.keys(snap.panes).length, 0);
  assert.equal(Object.keys(snap.layouts).length, 0);

  // Event-stream flicker: pane_created with no parent workspace/tab.
  const before = emptyState();
  const r = applyEvent(before, "pane_created", {
    type: "pane_created", pane: paneRec("w1:p1", "w1", "w1:t1"),
  });
  assert.equal(r.state, before); // same reference: inert
  assert.ok(!r.paneSetChanged);
});

test("workspace_closed and tab_closed cascade", () => {
  let state = emptyState();
  state = applyEvent(state, "workspace_created", {
    type: "workspace_created",
    workspace: { workspace_id: "w1", number: 1, label: "A", focused: true, pane_count: 1, tab_count: 1, active_tab_id: "w1:t1", agent_status: "idle" },
  }).state;
  state = applyEvent(state, "tab_created", {
    type: "tab_created",
    tab: { tab_id: "w1:t1", workspace_id: "w1", number: 1, label: "1", focused: true, pane_count: 1, agent_status: "idle" },
  }).state;
  state = applyEvent(state, "pane_created", {
    type: "pane_created", pane: paneRec("w1:p1", "w1", "w1:t1"),
  }).state;
  state = applyEvent(state, "layout_updated", {
    type: "layout_updated",
    layout: { workspace_id: "w1", tab_id: "w1:t1", zoomed: false, area: { x: 0, y: 0, width: 80, height: 24 }, focused_pane_id: "w1:p1", panes: [], splits: [] },
  }).state;

  const r = applyEvent(state, "workspace_closed", { type: "workspace_closed", workspace_id: "w1" });
  assert.ok(!r.state.workspaces["w1"]);
  assert.ok(!r.state.tabs["w1:t1"]);
  assert.ok(!r.state.panes["w1:p1"]);
  assert.ok(!r.state.layouts["w1:t1"]);

  // tab_closed cascades its panes too.
  state = r.state;
  state = applyEvent(state, "workspace_created", {
    type: "workspace_created",
    workspace: { workspace_id: "w2", number: 2, label: "B", focused: false, pane_count: 1, tab_count: 1, active_tab_id: "w2:t1", agent_status: "idle" },
  }).state;
  state = applyEvent(state, "tab_created", {
    type: "tab_created",
    tab: { tab_id: "w2:t1", workspace_id: "w2", number: 1, label: "1", focused: false, pane_count: 1, agent_status: "idle" },
  }).state;
  state = applyEvent(state, "pane_created", {
    type: "pane_created", pane: paneRec("w2:p1", "w2", "w2:t1"),
  }).state;
  const r2 = applyEvent(state, "tab_closed", { type: "tab_closed", tab_id: "w2:t1" });
  assert.ok(r2.paneSetChanged);
  assert.ok(!r2.state.panes["w2:p1"]);
});
