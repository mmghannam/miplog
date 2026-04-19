//! Shared parsing utilities for B&B node-progress tables.
//!
//! Each solver has its own table format, but the data is the same. Per-solver
//! modules implement [`parse_progress`] for their format; this module holds
//! helpers that are format-agnostic.

use crate::schema::NodeEvent;

/// Best-effort parse of a Gurobi-style time token ("0s", "12.3s", "5m", "2h").
/// Returns seconds, or `None` if the token doesn't match.
pub(crate) fn parse_time_token(tok: &str) -> Option<f64> {
    let t = tok.trim();
    let (num, mul) = if let Some(n) = t.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = t.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = t.strip_suffix('h') {
        (n, 3600.0)
    } else {
        (t, 1.0)
    };
    num.parse::<f64>().ok().map(|v| v * mul)
}

/// Parse a numeric field that may be "-" (missing).
pub(crate) fn parse_or_dash(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "-" || t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

/// Parse a gap token: "4.23%", "100%", "Inf", "-".
pub(crate) fn parse_gap(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "-" || t.is_empty() || t.eq_ignore_ascii_case("inf") {
        return None;
    }
    let s = t.strip_suffix('%').unwrap_or(t).trim();
    s.parse::<f64>().ok().map(|v| v / 100.0)
}

/// Infer a [`NodeEvent`] from a single-char Gurobi/Xpress/COPT marker.
pub(crate) fn event_from_marker(marker: char) -> Option<NodeEvent> {
    match marker {
        ' ' | '\t' => None,
        'H' | 'h' => Some(NodeEvent::Heuristic),
        '*' => Some(NodeEvent::BranchSolution),
        other => Some(NodeEvent::Other(other.to_string())),
    }
}
