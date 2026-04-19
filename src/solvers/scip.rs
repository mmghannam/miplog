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

        // Extended bounds / tree fields.
        parse_root_and_solution(text, &mut log);
        parse_tree_details(text, &mut log);

        // Rich solver-specific data under `other_data`.
        populate_other_data(text, &mut log);

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
    let hdr = HDR.get_or_init(|| Regex::new(r"(?m)^Separators\s*:\s+ExecTime.*?Applied").unwrap());
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

    Some(NodeSnapshot {
        time_seconds,
        nodes_explored: node,
        primal,
        dual,
        gap,
        depth,
        lp_iterations: lp_iter,
        event,
    })
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

/* ---------- extended fields (Root Node / Solution / B&B Tree) ---------- */

fn parse_root_and_solution(text: &str, log: &mut SolverLog) {
    // "Root Node" section:
    //   First LP value   : +7.11500000000000e+03
    //   First LP Iters   :        136
    //   First LP Time    :       0.00
    //   Final Dual Bound : +7.37159226382289e+03
    //   Final Root Iters :        868
    //   Root LP Estimate : +7.70514845437900e+03
    if let Some(c) = re_kv_f("Final Dual Bound", true).captures(text) {
        log.bounds.root_dual = parse_opt_f64(&c[1]);
    }

    // "Solution" section:
    //   First Solution   : +1.20850000000000e+04   (in run 1, after 1 nodes, 0.01 seconds, depth 26, found by <locks>)
    let first_sol_re = Regex::new(
        r"First Solution\s*:\s*(\+?-?[\d.eE+\-]+|-)\s*\(in run \d+, after \d+ nodes?, ([\d.]+) seconds",
    )
    .unwrap();
    if let Some(c) = first_sol_re.captures(text) {
        log.bounds.first_primal = parse_opt_f64(&c[1]);
        log.bounds.first_primal_time_seconds = c[2].parse().ok();
    }

    // Primal-dual integral is in the "Integrals" section:
    //   primal-dual      :       5.29      12.61
    // (first column is total, second is avg %)
    let pdi_re = Regex::new(r"(?m)^\s*primal-dual\s*:\s*([\d.eE+\-]+)").unwrap();
    if let Some(c) = pdi_re.captures(text) {
        log.bounds.primal_dual_integral = c[1].parse().ok();
    }
}

fn parse_tree_details(text: &str, log: &mut SolverLog) {
    // B&B Tree section contains a multi-line block:
    //   number of runs   :          1
    //   nodes            :         29 (15 internal, 14 leaves)
    //   ...
    //   max depth        :          7
    if let Some(c) = Regex::new(r"number of runs\s*:\s*(\d+)")
        .unwrap()
        .captures(text)
    {
        log.tree.restarts = c[1].parse().ok();
    }
    if let Some(c) = Regex::new(r"(?m)^\s*max depth\s*:\s*(\d+)")
        .unwrap()
        .captures(text)
    {
        log.tree.max_depth = c[1].parse().ok();
    }
}

/// Regex for "key : <float>" style SCIP summary rows. When `allow_plus_sign`
/// is true, the capture can start with `+` (the SCIP convention for positive
/// numbers in bound reports).
fn re_kv_f(key: &str, allow_plus_sign: bool) -> Regex {
    let val = if allow_plus_sign {
        r"(\+?-?[\d.eE+\-]+|-)"
    } else {
        r"([\d.eE+\-]+)"
    };
    Regex::new(&format!(r"{}\s*:\s*{}", regex::escape(key), val)).unwrap()
}

/* ---------- rich solver-specific data under `other_data` ---------- */

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_root_node_block(text) {
        log.other_data.push(NamedValue::new("scip.root_node", v));
    }
    if let Some(v) = parse_tree_block(text) {
        log.other_data.push(NamedValue::new("scip.tree", v));
    }
    if let Some(v) = parse_solution_attribution(text) {
        log.other_data
            .push(NamedValue::new("scip.solution_attribution", v));
    }
    if let Some(v) = parse_named_table(
        text,
        "Primal Heuristics",
        &["exec_time", "setup_time", "calls", "found", "best"],
    ) {
        log.other_data.push(NamedValue::new("scip.heuristics", v));
    }
    if let Some(v) = parse_named_table(
        text,
        "Separators",
        &[
            "exec_time",
            "setup_time",
            "calls",
            "root_calls",
            "cutoffs",
            "dom_reds",
            "found_cuts",
            "via_pool_add",
            "direct_add",
            "applied",
            "via_pool_app",
            "direct_app",
            "conss",
        ],
    ) {
        log.other_data.push(NamedValue::new("scip.separators", v));
    }
    if let Some(v) = parse_named_table(
        text,
        "Branching Rules",
        &[
            "exec_time",
            "setup_time",
            "branch_lp",
            "branch_ext",
            "branch_ps",
            "cutoffs",
            "dom_reds",
            "cuts",
            "conss",
            "children",
        ],
    ) {
        log.other_data
            .push(NamedValue::new("scip.branching_rules", v));
    }
    if let Some(v) = parse_named_table(
        text,
        "LP",
        &[
            "time",
            "calls",
            "iterations",
            "iter_per_call",
            "iter_per_sec",
        ],
    ) {
        log.other_data.push(NamedValue::new("scip.lp_breakdown", v));
    }
    if let Some(v) = parse_conflict_analysis(text) {
        log.other_data
            .push(NamedValue::new("scip.conflict_analysis", v));
    }
    if let Some(v) = parse_constraints_by_type(text) {
        log.other_data
            .push(NamedValue::new("scip.constraints_by_type", v));
    }
    if let Some(v) = parse_integrals(text) {
        log.other_data.push(NamedValue::new("scip.integrals", v));
    }
}

fn parse_root_node_block(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^Root Node\s*:").unwrap());
    let m = hdr.find(text)?;
    let mut obj = serde_json::Map::new();
    static ROW_RE: OnceLock<Regex> = OnceLock::new();
    let row_re =
        ROW_RE.get_or_init(|| Regex::new(r"^\s+([A-Za-z][A-Za-z ]+?)\s*:\s+(\S+)").unwrap());
    for line in text[m.end()..].lines().skip(1).take(10) {
        if line.trim().is_empty() {
            continue;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(c) = row_re.captures(line) {
            let k = c[1].trim().to_lowercase().replace(' ', "_");
            obj.insert(k, parse_json_scalar(&c[2]));
        }
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

fn parse_tree_block(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^B&B Tree\s*:").unwrap());
    let m = hdr.find(text)?;
    let mut obj = serde_json::Map::new();
    static ROW_RE: OnceLock<Regex> = OnceLock::new();
    let row_re =
        ROW_RE.get_or_init(|| Regex::new(r"^\s+([A-Za-z][A-Za-z. ]+?)\s*:\s+(\S.*)$").unwrap());
    for line in text[m.end()..].lines().skip(1).take(20) {
        if line.trim().is_empty() {
            continue;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        // Format: "  name    :   value [extra]"
        if let Some(c) = row_re.captures(line) {
            let k = c[1].trim().to_lowercase().replace([' ', '.'], "_");
            let raw = c[2].trim();
            // Extract leading number; stash rest as additional info.
            let first = raw.split_whitespace().next().unwrap_or(raw);
            obj.insert(k, parse_json_scalar(first));
        }
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

/// Extract "First Solution" / "Primal Bound" attribution from the Solution section:
///   First Solution  : +1.20850000000000e+04   (in run 1, after 1 nodes, 0.01 seconds, depth 26, found by <locks>)
///   Primal Bound    : +7.61500000000000e+03   (in run 1, after 28 nodes, 0.43 seconds, depth 6, found by <relaxation>)
fn parse_solution_attribution(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    let attr_re = Regex::new(
        r"\s*:\s*\+?-?[\d.eE+\-]+\s*\(in run (\d+), after (\d+) nodes?, ([\d.]+) seconds, depth (\d+), found by <([^>]+)>\)",
    )
    .unwrap();
    for (kind, label) in [("First Solution", "first"), ("Primal Bound", "best")] {
        let pat = format!(r"{}{}", regex::escape(kind), attr_re.as_str());
        if let Ok(re) = Regex::new(&pat) {
            if let Some(c) = re.captures(text) {
                let mut inner = serde_json::Map::new();
                inner.insert("run".into(), parse_json_scalar(&c[1]));
                inner.insert("nodes".into(), parse_json_scalar(&c[2]));
                inner.insert("time_seconds".into(), parse_json_scalar(&c[3]));
                inner.insert("depth".into(), parse_json_scalar(&c[4]));
                inner.insert(
                    "heuristic".into(),
                    serde_json::Value::String(c[5].to_string()),
                );
                obj.insert(label.into(), serde_json::Value::Object(inner));
            }
        }
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

/// Parse a SCIP-style named-row table (e.g. "Primal Heuristics", "Separators",
/// "Branching Rules", "LP"). Returns a list of `{name, ...column-keyed-values}`.
/// Skips sub-rows starting with `>` (they roll up into the parent).
fn parse_named_table(text: &str, section: &str, columns: &[&str]) -> Option<serde_json::Value> {
    let hdr_re = Regex::new(&format!(r"(?m)^{}\s*:", regex::escape(section))).unwrap();
    let m = hdr_re.find(text)?;
    let row_re = Regex::new(r"^\s+([A-Za-z][A-Za-z0-9_ /()>.-]*?)\s*:\s+(.*)$").unwrap();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for line in text[m.end()..].lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if line.trim_start().starts_with('>') {
            continue;
        }
        let Some(c) = row_re.captures(line) else {
            continue;
        };
        let name = c[1].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = c[2].split_whitespace().collect();
        let mut obj = serde_json::Map::new();
        obj.insert("name".into(), serde_json::Value::String(name));
        for (col, tok) in columns.iter().zip(tokens.iter()) {
            obj.insert((*col).into(), parse_json_scalar(tok));
        }
        rows.push(serde_json::Value::Object(obj));
    }
    if rows.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(rows))
    }
}

fn parse_conflict_analysis(text: &str) -> Option<serde_json::Value> {
    // "Conflict Analysis  :       Time      Calls    Success    DomReds  Conflicts   Literals ..."
    parse_named_table(
        text,
        "Conflict Analysis",
        &[
            "time",
            "calls",
            "success",
            "dom_reds",
            "conflicts",
            "literals",
        ],
    )
}

fn parse_constraints_by_type(text: &str) -> Option<serde_json::Value> {
    // Lines right after the "presolved problem has" line:
    //   "     81 constraints of type <knapsack>"
    //   "     26 constraints of type <setppc>"
    let re = Regex::new(r"(?m)^\s*(\d+) constraints? of type <([^>]+)>").unwrap();
    let mut obj = serde_json::Map::new();
    for cap in re.captures_iter(text) {
        let n: u64 = cap[1].parse().ok()?;
        let name = cap[2].to_string();
        obj.insert(name, serde_json::Value::from(n));
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

fn parse_integrals(text: &str) -> Option<serde_json::Value> {
    // "Integrals          :      Total       Avg%"
    //   "primal-dual      :       5.29      12.61"
    //   "primal-ref       :          -          - (not evaluated)"
    //   "dual-ref         :          -          - (not evaluated)"
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^Integrals\s*:").unwrap());
    let m = hdr.find(text)?;
    let row_re = Regex::new(r"^\s+([a-z-]+)\s*:\s+(\S+)\s+(\S+)").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(5) {
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(c) = row_re.captures(line) {
            let mut inner = serde_json::Map::new();
            inner.insert("total".into(), parse_json_scalar(&c[2]));
            inner.insert("avg_pct".into(), parse_json_scalar(&c[3]));
            obj.insert(c[1].replace('-', "_"), serde_json::Value::Object(inner));
        }
    }
    if obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(obj))
    }
}

fn parse_json_scalar(tok: &str) -> serde_json::Value {
    let s = tok.trim_matches(|c: char| c == ',' || c == '%');
    if s == "-" || s.is_empty() {
        return serde_json::Value::Null;
    }
    if let Ok(n) = s.parse::<i64>() {
        return serde_json::Value::from(n);
    }
    if let Ok(n) = s.trim_start_matches('+').parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(tok.to_string())
}
