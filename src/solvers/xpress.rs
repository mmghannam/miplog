//! FICO Xpress log parser. Tested against Xpress 9.8 output.

use crate::solvers::progress::{event_from_marker, parse_gap, parse_or_dash};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct XpressParser;

impl LogParser for XpressParser {
    fn solver(&self) -> Solver {
        Solver::Xpress
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("FICO Xpress")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("xpress"));
        }
        let mut log = SolverLog::new(Solver::Xpress);

        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }
        if let Some(c) = re_readprob().captures(text) {
            log.problem = Some(c[1].trim().to_string());
        }

        // Status. Xpress prints "*** Search completed ***" when the MIP B&B
        // finished normally; "*** Search unfinished ***" when maxtime hit.
        if text.contains("*** Search completed ***") {
            log.termination.status = Status::Optimal;
        } else if text.contains("Problem is integer infeasible")
            || text.contains("Problem is infeasible")
        {
            log.termination.status = Status::Infeasible;
        } else if text.contains("*** Search unfinished ***") {
            log.termination.status = Status::TimeLimit;
            log.termination.raw_reason = Some("Search unfinished".into());
        }

        // Time: "Solution time / primaldual integral :     T.TTs/ ..."
        if let Some(c) = re_soltime().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }

        // Bounds: final MIP objective & bound
        if let Some(c) = re_final_obj().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        }
        if let Some(c) = re_final_bound().captures(text) {
            log.bounds.dual = c[1].parse().ok();
        }

        // "Number of solutions found / nodes:  N / M"
        if let Some(c) = re_sols_nodes().captures(text) {
            log.tree.solutions_found = c[1].parse().ok();
            log.tree.nodes_explored = c[2].parse().ok();
        }

        // Pre-presolve dims: "NNN (...) rows / NNN (...) structural columns / NNN (...) non-zero elements"
        if let Some(c) = re_problem_stats().captures(text) {
            log.presolve.rows_before = c[1].parse().ok();
            log.presolve.cols_before = c[2].parse().ok();
            log.presolve.nonzeros_before = c[3].parse().ok();
        }

        // Presolved problem dims
        if let Some(c) = re_presolved().captures(text) {
            log.presolve.rows_after = c[1].parse().ok();
            log.presolve.cols_after = c[2].parse().ok();
            log.presolve.nonzeros_after = c[3].parse().ok();
        }

        // Presolve finished in T seconds
        if let Some(c) = re_presolve_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }

        log.progress = parse_progress(text);

        Ok(log)
    }
}

/// Parse Xpress B&B progress rows. Header looks like:
///   `    Node     BestSoln    BestBound   Sols Active  Depth     Gap     GInf   Time`
///
/// Two row shapes:
/// * 9 fields (with incumbent & gap):
///   `[m] Node BestSoln BestBound Sols Active Depth Gap GInf Time`
/// * 7 fields (no incumbent yet):
///   `[m] Node BestBound Sols Active Depth GInf Time`
///
/// `m` is an optional single-letter marker (e.g. `a`, `b`, `F` for branching
/// / bound-strengthening events).
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;
    for line in text.lines() {
        if !in_table {
            // Header signature: contains "Node", "BestBound", "Time" on the same line.
            if line.contains("Node")
                && line.contains("BestBound")
                && line.trim_end().ends_with("Time")
            {
                in_table = true;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if !out.is_empty() {
                break;
            }
            continue;
        }
        // End-of-table / summary markers.
        if trimmed.starts_with("***")
            || trimmed.starts_with("Final MIP")
            || trimmed.starts_with("Uncrunching")
            || trimmed.starts_with("Heap usage")
        {
            break;
        }
        if let Some(row) = parse_row(line) {
            out.push(row);
        }
    }
    out
}

fn parse_row(line: &str) -> Option<NodeSnapshot> {
    // Peel an optional single-letter marker.
    let (event, body) = match line.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => (event_from_marker(c), &line[c.len_utf8()..]),
        _ => (None, line),
    };
    let toks: Vec<&str> = body.split_whitespace().collect();

    let mut snap = NodeSnapshot::default();
    match toks.len() {
        9 => {
            // Node BestSoln BestBound Sols Active Depth Gap GInf Time
            snap.nodes_explored = toks[0].parse().ok();
            snap.primal = parse_or_dash(toks[1]);
            snap.dual = parse_or_dash(toks[2]);
            // toks[3] = Sols (we track this separately if useful later)
            snap.depth = toks[5].parse().ok();
            snap.gap = parse_gap(toks[6]);
            // toks[7] = GInf
            snap.time_seconds = toks[8].parse().ok()?;
        }
        7 => {
            // Node BestBound Sols Active Depth GInf Time
            snap.nodes_explored = toks[0].parse().ok();
            snap.dual = parse_or_dash(toks[1]);
            snap.depth = toks[4].parse().ok();
            snap.time_seconds = toks[6].parse().ok()?;
        }
        _ => return None,
    }
    if snap.nodes_explored.is_none() {
        return None;
    }
    snap.event = event;
    Some(snap)
}

fn re_problem_stats() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?ms)Problem Statistics\s*\n\s*(\d+)\s+.*?rows\s*\n\s*(\d+)\s+.*?structural columns\s*\n\s*(\d+)\s+.*?non-zero elements",
        )
        .unwrap()
    })
}
fn re_presolve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolve finished in (\d+) seconds").unwrap())
}

fn re_version() -> &'static Regex {
    // Xpress banner variants seen in the wild:
    //   "FICO Xpress Solver 64bit v9.8.0 Oct 22 2025"
    //   "FICO Xpress v9.8.1, Community, solve started ..."
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"FICO Xpress(?:\s+Solver\s+\S+)?\s+v(\d+\.\d+\.\d+)").unwrap())
}
fn re_readprob() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Reading Problem\s+(\S+)").unwrap())
}
fn re_soltime() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solution time\s*/.*?:\s*([\d.]+)s").unwrap())
}
fn re_final_obj() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Final MIP objective\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_final_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Final MIP bound\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_sols_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Number of solutions found\s*/\s*nodes\s*:\s*(\d+)\s*/\s*(\d+)").unwrap()
    })
}
fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Presolved problem has:\s+(\d+)\s+rows\s+(\d+)\s+cols\s+(\d+)\s+elements")
            .unwrap()
    })
}
