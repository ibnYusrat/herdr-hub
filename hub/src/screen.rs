//! Screen polling: `pane.read` for viewed panes only, revision-gated, with
//! per-client full/rows delivery. herdr has no push for screen content
//! (SPEC §3.4); this is the designated middle layer.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, watch};
use tracing::debug;

use crate::clients::Registry;
use crate::hubproto::{RowChange, ScreenFrame, ScreenMode, ScreenSource};
use crate::state::{InterestMap, ServerState};
use crate::wire::HerdrClient;

/// If more than this fraction of lines changed (or the row count changed),
/// send one full frame instead of row deltas.
const FULL_REWRITE_THRESHOLD: f32 = 0.6;
/// Max pane reads per poll round (SPEC §12: keep polling cost bounded).
const MAX_READS_PER_ROUND: usize = 16;
/// How many lines of scrollback to fetch per `recent` read (cap is 1000).
const SCROLLBACK_LINES: u32 = 500;

#[derive(Clone)]
struct CachedScreen {
    revision: u64,
    lines: Vec<String>,
}

/// One computed update, delivered per client as full or rows depending on
/// what that client last received.
pub struct ScreenUpdate {
    pub pane: String,
    pub revision: u64,
    pub prev_revision: u64,
    pub rows: u32,
    pub cols: u32,
    pub source: ScreenSource,
    pub truncated: bool,
    pub lines: Vec<String>,
    pub changes: Vec<RowChange>,
}

pub fn spawn_poller(
    server_id: String,
    client: Arc<HerdrClient>,
    state: Arc<ArcSwap<ServerState>>,
    mut interest_rx: watch::Receiver<InterestMap>,
    mut hints_rx: mpsc::Receiver<(String, u64)>,
    registry: Arc<Registry>,
    poll_ms: u64,
) {
    tokio::spawn(async move {
        let poll_ms = poll_ms.clamp(50, 1000);
        let ssh = client.transport_kind() == crate::wire::TransportKind::Ssh;
        // herdr's own subscription tick is 100 ms; SSH doubles the intervals.
        let mult = if ssh { 2 } else { 1 };
        let mut tick = tokio::time::interval(Duration::from_millis(poll_ms));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cache: HashMap<(String, ScreenSource), CachedScreen> = HashMap::new();
        let mut last_poll: HashMap<String, Instant> = HashMap::new();
        let mut round: u64 = 0;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    round += 1;
                    let interest = interest_rx.borrow().clone();
                    prune_cache(&mut cache, &interest);
                    poll_round(
                        &server_id, &client, &state, &interest, &mut cache,
                        &mut last_poll, &registry, round, mult,
                    ).await;
                }
                changed = interest_rx.changed() => {
                    if changed.is_err() { break; }
                }
                hint = hints_rx.recv() => {
                    match hint {
                        Some((pane, _rev)) => {
                            // Immediate poll hint, rate-limited to half a tick.
                            let interest = interest_rx.borrow().clone();
                            let ready = last_poll
                                .get(&pane)
                                .map(|t| t.elapsed() >= Duration::from_millis(poll_ms / 2))
                                .unwrap_or(true);
                            if ready && interest.panes.contains_key(&pane) {
                                poll_one(
                                    &server_id, &client, &state, &pane, ScreenSource::Visible,
                                    &mut cache, &mut last_poll, &registry,
                                ).await;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });
}

fn prune_cache(cache: &mut HashMap<(String, ScreenSource), CachedScreen>, interest: &InterestMap) {
    cache.retain(|(pane, _), _| {
        interest.panes.contains_key(pane)
            || interest.scrollback.contains(pane)
    });
}

async fn poll_round(
    server_id: &str,
    client: &Arc<HerdrClient>,
    state: &Arc<ArcSwap<ServerState>>,
    interest: &InterestMap,
    cache: &mut HashMap<(String, ScreenSource), CachedScreen>,
    last_poll: &mut HashMap<String, Instant>,
    registry: &Arc<Registry>,
    round: u64,
    mult: u64,
) {
    // Due visible panes: focused viewers → every round; others → every 5th,
    // phase-spread so bursts of panes don't fire together.
    let mut due: Vec<(String, bool)> = interest
        .panes
        .iter()
        .filter(|(_, i)| i.viewers > 0)
        .map(|(pane, i)| {
            let focused = i.focused_viewers > 0;
            let phase = simple_hash(pane) % 5;
            let due_now = focused || (round / mult) % 5 == phase as u64;
            (pane.clone(), due_now)
        })
        .filter(|(_, due_now)| *due_now)
        .collect();
    // Focused panes first.
    due.sort_by_key(|(pane, _)| {
        let focused = interest
            .panes
            .get(pane)
            .map(|i| i.focused_viewers == 0)
            .unwrap_or(true);
        focused
    });
    due.truncate(MAX_READS_PER_ROUND);

    let mut reads = 0;
    for (pane, _) in due {
        if reads >= MAX_READS_PER_ROUND {
            break;
        }
        reads += 1;
        poll_one(server_id, client, state, &pane, ScreenSource::Visible, cache, last_poll, registry)
            .await;
    }

    // Scrollback: slower, `recent` plain-text reads for overlay panes.
    if (round / mult) % 10 == 0 {
        let mut scroll: Vec<String> = interest.scrollback.iter().cloned().collect();
        scroll.sort();
        scroll.truncate(4);
        for pane in scroll {
            poll_one(server_id, client, state, &pane, ScreenSource::Recent, cache, last_poll, registry)
                .await;
        }
    }
    let _ = state; // state used inside poll_one
}

async fn poll_one(
    server_id: &str,
    client: &Arc<HerdrClient>,
    state: &Arc<ArcSwap<ServerState>>,
    pane: &str,
    source: ScreenSource,
    cache: &mut HashMap<(String, ScreenSource), CachedScreen>,
    last_poll: &mut HashMap<String, Instant>,
    registry: &Arc<Registry>,
) {
    last_poll.insert(pane.to_string(), Instant::now());

    // Skip panes the server no longer knows (closing race).
    let state_now = state.load_full();
    if !state_now.panes.contains_key(pane) {
        return;
    }

    // herdr captures alternate-screen history by scrolling the app while
    // it is idle (wheel-event harvest + restore, SPEC §3.5): a `recent`
    // read on an idle agent pane makes the user's agent viewport scroll
    // up and back down on EVERY poll. Agent panes therefore feed the
    // scrollback stream from `visible` — the client's window diff treats
    // both the same way — in ANSI so the transcript can render the
    // agent's own colors (diff red/green, dim hints, ...). Non-agent
    // panes keep the real `recent` scrollback.
    let has_agent = state_now
        .panes
        .get(pane)
        .map(|p| p.agent.is_some())
        .unwrap_or(false);
    let (wire_source, wire_format, lines_cap) = match source {
        ScreenSource::Visible => ("visible", "ansi", None),
        ScreenSource::Recent if has_agent => ("visible", "ansi", None),
        ScreenSource::Recent => ("recent", "text", Some(SCROLLBACK_LINES)),
    };

    let read = match client
        .pane_read(pane, wire_source, wire_format, lines_cap)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // not_found is normal during pane churn; anything else is worth a log.
            if e.herdr_code() != Some("not_found") {
                debug!(server = server_id, pane, error = %e, "pane.read failed");
            }
            return;
        }
    };

    let key = (pane.to_string(), source);
    let prev = cache.get(&key);
    let lines = split_lines(&read.text);
    // Revision gate, with a content-equality fallback: 0.8.2 has been
    // observed not bumping the revision on output (and returning
    // per-(source,format) revisions), so text equality is the final word
    // (SPEC §6 change detection).
    if let Some(p) = prev {
        if unchanged(p, read.revision, &lines) {
            return;
        }
    }
    let cols = match source {
        ScreenSource::Visible => layout_cols(&state_now, pane).unwrap_or_else(|| max_width(&lines)),
        ScreenSource::Recent => max_width(&lines),
    };
    let rows = lines.len() as u32;

    let (prev_revision, changes, send_full) = match prev {
        Some(p) if p.lines.len() == lines.len() && !lines.is_empty() => {
            let changed_lines = diff_rows(&p.lines, &lines);
            let fraction = changed_lines.len() as f32 / lines.len().max(1) as f32;
            let full = fraction >= FULL_REWRITE_THRESHOLD;
            (p.revision, changed_lines, full)
        }
        _ => (0, Vec::new(), true),
    };

    if send_full || !changes.is_empty() {
        let update = ScreenUpdate {
            pane: pane.to_string(),
            revision: read.revision,
            prev_revision,
            rows,
            cols,
            source,
            truncated: read.truncated,
            lines: lines.clone(),
            changes: if send_full { Vec::new() } else { changes },
        };
        registry.deliver_screen(server_id, source, &update);
    }

    cache.insert(key, CachedScreen {
        revision: read.revision,
        lines,
    });
}

/// Split herdr's `\r\n`-joined text into lines, keeping ANSI intact.
fn split_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .map(|l| l.to_string())
        .collect()
}

/// Skip the update only when BOTH revision and content are unchanged: 0.8.2
/// can leave the revision at 0 across output, so revision alone must not
/// gate (the M3 gate caught exactly this).
fn unchanged(prev: &CachedScreen, revision: u64, lines: &[String]) -> bool {
    prev.revision == revision && prev.lines.as_slice() == lines
}

/// Which rows changed between two equal-length line vectors.
fn diff_rows(prev: &[String], next: &[String]) -> Vec<RowChange> {
    prev.iter()
        .zip(next.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (_, b))| RowChange {
            row: i as u32,
            text: b.clone(),
        })
        .collect()
}

/// Pane width in cells from the tab's BSP layout, when known.
fn layout_cols(state: &ServerState, pane: &str) -> Option<u32> {
    let p = state.panes.get(pane)?;
    let layout = state.layouts.get(&p.tab_id)?;
    layout
        .panes
        .iter()
        .find(|e| e.pane_id == pane)
        .map(|e| e.rect.width as u32)
}

/// Max line width ignoring ANSI escape sequences (fallback for `cols`).
fn max_width(lines: &[String]) -> u32 {
    lines
        .iter()
        .map(|l| strip_ansi_width(l))
        .max()
        .unwrap_or(0)
}

fn strip_ansi_width(s: &str) -> u32 {
    let mut width = 0u32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    // Consume until a final byte of the escape sequence.
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else if c != '\u{7f}' {
            width += 1;
        }
    }
    width
}

fn simple_hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// Build the wire frame for one client given what it last received.
pub fn frame_for(update: &ScreenUpdate, server: &str, client_has_prev: bool) -> ScreenFrame {    let mode = if client_has_prev && !update.changes.is_empty() {
        ScreenMode::Rows
    } else {
        ScreenMode::Full
    };
    ScreenFrame {
        server: server.to_string(),
        pane: update.pane.clone(),
        revision: update.revision,
        rows: update.rows,
        cols: update.cols,
        mode,
        lines: (mode == ScreenMode::Full).then(|| update.lines.clone()),
        changes: (mode == ScreenMode::Rows).then(|| update.changes.clone()),
        source: update.source,
        truncated: update.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_crlf_and_keeps_ansi() {
        let lines = split_lines("\u{1b}[32mok\u{1b}[0m\r\nplain\r\n");
        assert_eq!(lines, vec!["\u{1b}[32mok\u{1b}[0m", "plain", ""]);
    }

    #[test]
    fn diff_finds_changed_rows_only() {
        let prev = vec!["a".into(), "b".into(), "c".into()];
        let next = vec!["a".into(), "B".into(), "c".into()];
        let d = diff_rows(&prev, &next);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].row, 1);
        assert_eq!(d[0].text, "B");
    }

    #[test]
    fn stale_revision_with_new_content_still_updates() {
        let cached = CachedScreen {
            revision: 0,
            lines: vec!["❯".into()],
        };
        // Same revision (0.8.2 never bumped it), new content → NOT unchanged.
        assert!(!unchanged(&cached, 0, &["❯ echo hi".into(), "hi".into()]));
        // Same revision, same content → unchanged (the common idle case).
        assert!(unchanged(&cached, 0, &["❯".into()]));
        // Bumped revision → never unchanged, even if content looks equal.
        assert!(!unchanged(&cached, 1, &["❯".into()]));
    }

    #[test]
    fn ansi_width_ignores_escapes() {
        assert_eq!(strip_ansi_width("\u{1b}[38;2;1;2;3mab\u{1b}[0m"), 2);
        assert_eq!(strip_ansi_width("plain"), 5);
    }

    #[test]
    fn frame_mode_selection() {
        let update = ScreenUpdate {
            pane: "w1:p1".into(),
            revision: 10,
            prev_revision: 9,
            rows: 2,
            cols: 10,
            source: ScreenSource::Visible,
            truncated: false,
            lines: vec!["a".into(), "b".into()],
            changes: vec![RowChange { row: 1, text: "b".into() }],
        };
        let with_prev = frame_for(&update, "local", true);
        assert!(matches!(with_prev.mode, ScreenMode::Rows));
        let without_prev = frame_for(&update, "local", false);
        assert!(matches!(without_prev.mode, ScreenMode::Full));
        assert_eq!(without_prev.lines.unwrap().len(), 2);
    }
}
