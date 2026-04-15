//! SCIP log parser. Tested against SCIP 10 + 11 output, both SoPlex and CPLEX
//! LP solvers. Handles the Mittelmann-style concatenated wrapper transparently
//! — pass one per-instance slice (see [`crate::input::split_concatenated`]) or
//! a standalone SCIP-session log.

use crate::solvers::progress::parse_gap;
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct ScipParser;

impl LogParser for ScipParser {
    fn solver(&self) -> Solver {
        Solver::Scip
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("SCIP version") || text.contains("SCIP Status")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("scip"));
        }
        let mut log = SolverLog::new(Solver::Scip);

        // Version line: "SCIP version 11.0.0 [precision: 8 byte] ... [GitHash: 4f4f68fb97-dirty]"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }
        if let Some(c) = re_githash().captures(text) {
            log.solver_git_hash = Some(c[1].to_string());
        }
        if let Some(c) = re_problem().captures(text) {
            let raw = c[1].trim();
            let stem = std::path::Path::new(raw)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(raw)
                .trim_end_matches(".gz")
                .trim_end_matches(".mps")
                .trim_end_matches(".lp")
                .to_string();
            log.problem = Some(stem);
        }

        if let Some(c) = re_status().captures(text) {
            let reason = c[1].to_string();
            log.termination.status = classify_status(&reason);
            log.termination.raw_reason = Some(reason);
        }

        if let Some(c) = re_solving_time().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }
        if let Some(c) = re_presolving_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }
        // Root LP time lives in the "Root Node" section:
        //   First LP Time    :       0.00
        if let Some(c) = re_root_lp_time().captures(text) {
            log.timing.root_relaxation_seconds = c[1].parse().ok();
        }

        if let Some(c) = re_primal().captures(text) {
            log.bounds.primal = parse_opt_f64(&c[1]);
            log.tree.solutions_found = c.get(2).and_then(|m| m.as_str().parse().ok());
        }
        if let Some(c) = re_dual().captures(text) {
            log.bounds.dual = parse_opt_f64(&c[1]);
        }
        if let Some(c) = re_gap().captures(text) {
            log.bounds.gap = parse_gap(&c[1]);
        }

        // Nodes: "Solving Nodes : 31" is the authoritative summary value.
        if let Some(c) = re_solving_nodes().captures(text) {
            log.tree.nodes_explored = c[1].parse().ok();
        }

        // Simplex iterations: sum across the LP statistics section.
        log.tree.simplex_iterations = parse_simplex_iters(text);

        // Presolve dims.
        if let Some(c) = re_orig_dims().captures(text) {
            log.presolve.cols_before = c[1].parse().ok();
            log.presolve.rows_before = c[2].parse().ok();
        }
        if let Some(c) = re_presolved_dims().captures(text) {
            log.presolve.cols_after = c[1].parse().ok();
            log.presolve.rows_after = c[2].parse().ok();
        }

        // Cuts: parse Separators section for applied counts per family.
        log.cuts = parse_cuts(text);

        // Progress table.
        log.progress = parse_progress(text);

        Ok(log)
    }
}

fn parse_opt_f64(s: &str) -> Option<f64> {
    if s == "-" {
        None
    } else {
        s.trim_start_matches('+').parse().ok()
    }
}

fn classify_status(reason: &str) -> Status {
    let r = reason.to_lowercase();
    if r.contains("optimal solution") {
        Status::Optimal
    } else if r.contains("infeasible") && r.contains("unbounded") {
        Status::InfeasibleOrUnbounded
    } else if r.contains("infeasible") {
        Status::Infeasible
    } else if r.contains("unbounded") {
        Status::Unbounded
    } else if r.contains("time limit") {
        Status::TimeLimit
    } else if r.contains("memory limit") {
        Status::MemoryLimit
    } else if r.contains("user interrupt") || r.contains("interrupt") {
        Status::UserInterrupt
    } else if r.contains("gap limit") || r.contains("node limit") || r.contains("sol limit") {
        Status::OtherLimit
    } else {
        Status::Unknown
    }
}

/// Sum simplex iterations across the LP statistics section. Gives the
/// authoritative total (primal + dual + barrier + diving + strong branching
/// + conflict analysis LPs — matching SCIP's own counter).
fn parse_simplex_iters(text: &str) -> Option<u64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let header = R.get_or_init(|| Regex::new(r"(?m)^LP\s*:\s+Time\s+Calls\s+Iterations").unwrap());
    let start = header.find(text)?.end();
    // Read subsequent lines of the form `  <name> : <time> <calls> <iters> ...`
    // until we hit a blank line or another top-level section header.
    let row = Regex::new(r"^  \S[^:]*:\s+[\d.]+\s+[\d,]+\s+([\d,]+)").unwrap();
    let mut total: u64 = 0;
    let mut any = false;
    for line in text[start..].lines().skip(1) {
        // Section boundary: blank line OR a line starting with a non-space
        // letter (top-level section header).
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(cap) = row.captures(line) {
            if let Ok(n) = cap[1].replace(',', "").parse::<u64>() {
                total += n;
                any = true;
            }
        }
    }
    any.then_some(total)
}

/// Parse the Separators section and return per-family applied counts.
/// Sub-rows starting with `>` are sub-categories of the line above and are
/// skipped (their counts already roll up into the top-level row).
fn parse_cuts(text: &str) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    static HDR: OnceLock<Regex> = OnceLock::new();
    let hdr = HDR.get_or_init(|| {
        Regex::new(r"(?m)^Separators\s*:\s+ExecTime.*?Applied").unwrap()
    });
    let Some(m) = hdr.find(text) else {
        return out;
    };
    let row = Regex::new(
        // <name> : ExecTime SetupTime Calls RootCalls Cutoffs DomReds FoundCuts ViaPoolAdd DirectAdd Applied ...
        r"^\s+([A-Za-z][A-Za-z0-9_]*)\s+:\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+[\d.-]+\s+(\d+)",
    )
    .unwrap();
    for line in text[m.end()..].lines().skip(1) {
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        // Skip `>` sub-rows (their counts are a drilldown of the family above).
        if line.trim_start().starts_with('>') {
            continue;
        }
        // Skip the "cut pool" row — meta row, not a cut family.
        if line.trim_start().starts_with("cut pool") {
            continue;
        }
        if let Some(cap) = row.captures(line) {
            let name = cap[1].to_string();
            if let Ok(n) = cap[2].parse::<u64>() {
                if n > 0 {
                    out.insert(name, n);
                }
            }
        }
    }
    out
}

/// Parse SCIP's tabular progress output.
///
/// Columns (18):
///   `time | node | left | LP iter | LP it/n | mem/heur | mdpt | vars | cons | rows | cuts | sepa | confs | strbr | dualbound | primalbound | gap | compl.`
///
/// Rows have an optional single-char marker at column 0 (blank, `p`, `r`, `*`,
/// `L`, `i`, `h`, `R`, `I`, `d`, `b`, `s`, `o` — see SCIP docs). The first cell
/// combines the marker with the time ("p 0.0s", "  0.1s").
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;
    for line in text.lines() {
        // Table (re-)starts at every header line; SCIP reprints it every ~15 rows.
        if line.contains("| node") && line.contains("LP iter") && line.contains("dualbound") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        // Termination markers / summary lines end the table.
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("SCIP Status")
            || trimmed.starts_with("Solving Time")
            || trimmed.starts_with("Solving Nodes")
            || trimmed.starts_with("Primal Bound")
            || trimmed.starts_with("Dual Bound")
        {
            break;
        }
        if !line.contains('|') {
            continue;
        }
        if let Some(row) = parse_row(line) {
            out.push(row);
        }
    }
    out
}

fn parse_row(line: &str) -> Option<NodeSnapshot> {
    // Split on pipe. Expect 18 cells (17 separators).
    let cells: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
    if cells.len() < 17 {
        return None;
    }
    // Cell 0: "<marker>?<time>s" — e.g. "p 0.0s", "0.1s", "* 0.3s".
    let (event, time_str) = split_marker(cells[0]);
    let time_seconds = time_str.trim_end_matches('s').parse::<f64>().ok()?;

    let node = cells[1].parse().ok();
    let lp_iter = cells[3].parse().ok();
    // cell 6 = mdpt (max depth at this point)
    let depth = cells[6].parse().ok();
    let dual = parse_exp(cells[14]);
    let primal = parse_exp(cells[15]);
    let gap = parse_gap(cells[16]);

    let mut snap = NodeSnapshot::default();
    snap.time_seconds = time_seconds;
    snap.nodes_explored = node;
    snap.primal = primal;
    snap.dual = dual;
    snap.gap = gap;
    snap.depth = depth;
    snap.lp_iterations = lp_iter;
    snap.event = event;
    Some(snap)
}

/// Split a cell like `"p 0.0s"` or `"  0.1s"` or `"* 0.3s"` into an optional
/// event marker and the raw time token.
fn split_marker(cell: &str) -> (Option<NodeEvent>, &str) {
    // The cell is already trimmed. A leading non-digit, non-space char is the
    // marker; the rest (after optional space) is the time.
    let mut chars = cell.chars();
    let Some(first) = chars.next() else {
        return (None, cell);
    };
    if first.is_ascii_digit() || first == '.' {
        return (None, cell);
    }
    let rest = chars.as_str().trim_start();
    let event = match first {
        '*' => Some(NodeEvent::BranchSolution),
        'R' | 'r' | 'L' | 'p' | 'h' | 'H' | 'i' | 'I' | 'd' | 'b' | 'o' | 's' => {
            Some(NodeEvent::Heuristic)
        }
        other => Some(NodeEvent::Other(other.to_string())),
    };
    (event, rest)
}

fn parse_exp(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "-" || t.is_empty() {
        return None;
    }
    t.trim_start_matches('+').parse().ok()
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"SCIP version (\d+\.\d+(?:\.\d+)?)").unwrap())
}
fn re_githash() -> &'static Regex {
    // Allow hex + suffixes like `-dirty` or `-unstable`. Capture everything
    // up to the closing `]`.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"SCIP version[^\n]*\[GitHash:\s*([^\]]+?)\s*\]").unwrap())
}
fn re_problem() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"read problem <([^>]+)>").unwrap())
}
fn re_status() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"SCIP Status\s*:\s*[^\[\n]*\[([^\]]+)\]").unwrap())
}
fn re_solving_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solving Time \(sec\)\s*:\s*([\d.]+)").unwrap())
}
fn re_presolving_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolving Time\s*:\s*([\d.]+)").unwrap())
}
fn re_root_lp_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"First LP Time\s*:\s*([\d.]+)").unwrap())
}
fn re_primal() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Primal Bound\s*:\s*(\+?-?[\d.eE+\-]+|-)\s*(?:\((\d+) solutions\))?").unwrap()
    })
}
fn re_dual() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:Final )?Dual Bound\s*:\s*(\+?-?[\d.eE+\-]+|-)").unwrap())
}
fn re_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Gap\s*:\s*([\d.]+ ?%|infinite|-)").unwrap())
}
fn re_solving_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solving Nodes\s*:\s*(\d+)").unwrap())
}
fn re_orig_dims() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"original problem has (\d+) variables[^\n]* and (\d+) constraints").unwrap()
    })
}
fn re_presolved_dims() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"presolved problem has (\d+) variables[^\n]* and (\d+) constraints").unwrap()
    })
}
