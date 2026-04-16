//! CPLEX log parser. Tested against CPLEX 12.7.1 and 12.8.0 output.

use crate::solvers::progress::{parse_gap, parse_or_dash};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct CplexParser;

impl LogParser for CplexParser {
    fn solver(&self) -> Solver {
        Solver::Cplex
    }

    fn sniff(&self, text: &str) -> bool {
        (text.contains("CPLEX") || text.contains("CPXPARAM"))
            && (text.contains("Interactive Optimizer")
                || text.contains("CPXPARAM")
                || text.contains("MIP - "))
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("cplex"));
        }
        let mut log = SolverLog::new(Solver::Cplex);

        // Version: "CPLEX(R) Interactive Optimizer 12.8.0.0"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        // Problem name: "Problem 'instances/miplib2010/bab5.mps.gz' read."
        if let Some(c) = re_problem().captures(text) {
            // Extract just the filename stem
            let path = &c[1];
            let name = path.rsplit('/').next().unwrap_or(path);
            let name = name
                .strip_suffix(".mps.gz")
                .or_else(|| name.strip_suffix(".mps"))
                .or_else(|| name.strip_suffix(".lp.gz"))
                .or_else(|| name.strip_suffix(".lp"))
                .unwrap_or(name);
            log.problem = Some(name.to_string());
        }

        // Pre-presolve dims: problem statement doesn't have them in one line
        // Original: "R rows, C columns and N nonzeros" not always present.
        // But reduced/presolved: "Reduced MIP has R rows, C columns, and N nonzeros."
        // We treat the problem read line as "before" and Reduced as "after".

        // Presolve after: "Reduced MIP has R rows, C columns, and N nonzeros."
        // May appear multiple times; take the last one before the progress table.
        for c in re_reduced().captures_iter(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        // Status: "MIP - Integer optimal solution:", "MIP - Integer infeasible.", "MIP - Time limit"
        parse_status(text, &mut log);

        // Objective from status line: "Objective = -1.0641184010e+05"
        if let Some(c) = re_objective().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        }

        // Solution time / iterations / nodes:
        // "Solution time = 1551.53 sec.  Iterations = 4932561  Nodes = 51737"
        // CPLEX prints "Solution time = 0.00" for trivially-fast problems; fall
        // back to the last "Elapsed time" line for a finer-grained measurement.
        if let Some(c) = re_solution_time().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
            log.tree.simplex_iterations = c[2].replace(',', "").parse().ok();
            log.tree.nodes_explored = c[3].replace(',', "").parse().ok();
        }
        if log.timing.wall_seconds.unwrap_or(0.0) == 0.0 {
            // Prefer the authoritative "Total (root+branch&cut) = T sec."
            // summary that CPLEX prints at the end of MIP runs.
            if let Some(c) = re_total_time().captures(text) {
                log.timing.wall_seconds = c[1].parse().ok();
            }
            // Last-resort fallback to the most recent "Elapsed time" marker
            // (for trivially fast solves where neither summary line shows
            // the real value).
            if log.timing.wall_seconds.unwrap_or(0.0) == 0.0 {
                if let Some(t) = re_elapsed()
                    .captures_iter(text)
                    .last()
                    .and_then(|c| c[1].parse::<f64>().ok())
                {
                    if t > 0.0 {
                        log.timing.wall_seconds = Some(t);
                    }
                }
            }
        }

        // Root relaxation: "Root relaxation solution time = X sec."
        if let Some(c) = re_root_time().captures(text) {
            log.timing.root_relaxation_seconds = c[1].parse().ok();
        }

        // Solution pool: "Solution pool: N solutions saved."
        if let Some(c) = re_sol_pool().captures(text) {
            log.tree.solutions_found = c[1].parse().ok();
        }

        // Cuts
        parse_cuts(text, &mut log);

        // Progress table
        log.progress = parse_progress(text);

        // Best bound from last progress row or from summary
        // CPLEX doesn't have a separate "Best bound" summary line like other solvers;
        // we take it from the progress table's last row.
        if log.bounds.dual.is_none() && !log.progress.is_empty() {
            log.bounds.dual = *log.progress.dual.last().unwrap_or(&None);
        }
        // Gap from last progress row
        if log.bounds.gap.is_none() && !log.progress.is_empty() {
            log.bounds.gap = *log.progress.gap.last().unwrap_or(&None);
        }

        // Root LP dual bound: the first progress row's Best Bound is the
        // LP relaxation bound at the root (before cuts). CPLEX reports it
        // in the "Objective" column for the first few rows.
        for i in 0..log.progress.len() {
            if let Some(d) = log.progress.dual[i] {
                if d != 0.0 {
                    log.bounds.root_dual = Some(d);
                    break;
                }
            }
        }

        // First incumbent: "Found incumbent of value X after T sec."
        if let Some(c) = re_first_incumbent().captures(text) {
            log.bounds.first_primal = c[1].parse().ok();
            log.bounds.first_primal_time_seconds = c[2].parse().ok();
        }

        // Presolve time: "Presolve time = 0.00 sec. (2.20 ticks)"
        // Sum across the multiple presolve-time lines (CPLEX prints one per
        // presolve phase); gives total presolve wall time.
        let mut total_presolve = 0.0f64;
        let mut any = false;
        for c in re_presolve_time().captures_iter(text) {
            if let Ok(t) = c[1].parse::<f64>() {
                total_presolve += t;
                any = true;
            }
        }
        if any {
            log.timing.presolve_seconds = Some(total_presolve);
        }

        populate_other_data(text, &mut log);

        Ok(log)
    }
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_search_config(text) {
        log.other_data.push(NamedValue::new("cplex.search_config", v));
    }
    if let Some(v) = parse_integer_breakdown(text) {
        log.other_data.push(NamedValue::new("cplex.variable_types_after_presolve", v));
    }
    if let Some(v) = parse_presolve_details(text) {
        log.other_data.push(NamedValue::new("cplex.presolve_details", v));
    }
    if let Some(v) = parse_probing(text) {
        log.other_data.push(NamedValue::new("cplex.probing", v));
    }
    if let Some(v) = parse_clique_table(text) {
        log.other_data.push(NamedValue::new("cplex.clique_table_members", v));
    }
    if let Some(v) = parse_heuristic_solutions(text) {
        log.other_data.push(NamedValue::new("cplex.incumbents", v));
    }
    if let Some(v) = parse_ticks(text) {
        log.other_data.push(NamedValue::new("cplex.deterministic_ticks", v));
    }
    if let Some(v) = parse_timing_breakdown(text) {
        log.other_data.push(NamedValue::new("cplex.timing_breakdown", v));
    }
}

/// Search config: MIP emphasis, search method, parallel mode, threads.
fn parse_search_config(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    if let Some(c) = Regex::new(r"MIP emphasis:\s+(.+?)\.").unwrap().captures(text) {
        obj.insert("mip_emphasis".into(), serde_json::Value::String(c[1].trim().to_string()));
    }
    if let Some(c) = Regex::new(r"MIP search method:\s+(.+?)\.").unwrap().captures(text) {
        obj.insert("search_method".into(), serde_json::Value::String(c[1].trim().to_string()));
    }
    if let Some(c) = Regex::new(
        r"Parallel mode:\s+([^,]+),\s+using up to (\d+) threads?",
    )
    .unwrap()
    .captures(text)
    {
        obj.insert("parallel_mode".into(), serde_json::Value::String(c[1].trim().to_string()));
        obj.insert("threads".into(), parse_f64_json(&c[2]));
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// "Reduced MIP has 183 binaries, 3 generals, 0 SOSs, and 0 indicators."
fn parse_integer_breakdown(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(
        r"Reduced MIP has (\d+) binaries,\s+(\d+) generals,\s+(\d+) SOSs,\s+and\s+(\d+) indicators",
    )
    .unwrap();
    let c = re.captures_iter(text).last()?;
    let mut obj = serde_json::Map::new();
    obj.insert("binaries".into(), parse_f64_json(&c[1]));
    obj.insert("generals".into(), parse_f64_json(&c[2]));
    obj.insert("sos".into(), parse_f64_json(&c[3]));
    obj.insert("indicators".into(), parse_f64_json(&c[4]));
    Some(serde_json::Value::Object(obj))
}

fn parse_presolve_details(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    let mut eliminated_rows = 0u64;
    let mut eliminated_cols = 0u64;
    for c in Regex::new(r"MIP Presolve eliminated (\d+) rows? and (\d+) columns?")
        .unwrap()
        .captures_iter(text)
    {
        eliminated_rows += c[1].parse::<u64>().unwrap_or(0);
        eliminated_cols += c[2].parse::<u64>().unwrap_or(0);
    }
    if eliminated_rows > 0 || eliminated_cols > 0 {
        obj.insert("eliminated_rows".into(), serde_json::Value::from(eliminated_rows));
        obj.insert("eliminated_cols".into(), serde_json::Value::from(eliminated_cols));
    }
    if let Some(c) = Regex::new(r"MIP Presolve modified (\d+) coefficients?")
        .unwrap()
        .captures_iter(text)
        .last()
    {
        obj.insert("modified_coefficients".into(), parse_f64_json(&c[1]));
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

fn parse_probing(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    if let Some(c) = Regex::new(r"Probing time\s*=\s*([\d.]+)\s+sec")
        .unwrap()
        .captures_iter(text)
        .last()
    {
        obj.insert("time_seconds".into(), parse_f64_json(&c[1]));
    }
    if let Some(c) = Regex::new(r"Probing changed sense of (\d+) constraints?")
        .unwrap()
        .captures(text)
    {
        obj.insert("constraints_sense_changed".into(), parse_f64_json(&c[1]));
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

fn parse_clique_table(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(r"Clique table members:\s+(\d+)").unwrap().captures(text)?;
    Some(parse_f64_json(&c[1]))
}

fn parse_heuristic_solutions(text: &str) -> Option<serde_json::Value> {
    // "Found incumbent of value X after T sec."
    let re = Regex::new(
        r"Found incumbent of value\s+([\d.eE+\-]+)\s+after\s+([\d.]+)\s+sec",
    )
    .unwrap();
    let mut arr: Vec<serde_json::Value> = Vec::new();
    for c in re.captures_iter(text) {
        let mut o = serde_json::Map::new();
        o.insert("value".into(), parse_f64_json(&c[1]));
        o.insert("time_seconds".into(), parse_f64_json(&c[2]));
        arr.push(serde_json::Value::Object(o));
    }
    (!arr.is_empty()).then(|| serde_json::Value::Array(arr))
}

/// CPLEX's deterministic ticks — a workload counter that's hardware-independent.
/// Useful for apples-to-apples comparison across machines.
fn parse_ticks(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(r"Total \(root\+branch&cut\)\s*=\s*[\d.]+\s+sec\.\s+\(([\d.]+)\s+ticks\)")
        .unwrap()
        .captures(text)?;
    Some(parse_f64_json(&c[1]))
}

/// "Root node processing (before b&c): Real time = 0.04 sec."
/// "Parallel b&c, 14 threads: Real time = 0.00 sec."
fn parse_timing_breakdown(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    if let Some(c) = Regex::new(
        r"Root node processing \(before b&c\):\s*\n\s*Real time\s*=\s*([\d.]+)",
    )
    .unwrap()
    .captures(text)
    {
        obj.insert("root_node_time".into(), parse_f64_json(&c[1]));
    }
    if let Some(c) = Regex::new(
        r"Parallel b&c,\s+\d+\s+threads?:\s*\n\s*Real time\s*=\s*([\d.]+)",
    )
    .unwrap()
    .captures(text)
    {
        obj.insert("parallel_bc_time".into(), parse_f64_json(&c[1]));
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

fn parse_f64_json(s: &str) -> serde_json::Value {
    if let Ok(v) = s.trim().parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.trim().to_string())
}

fn re_first_incumbent() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Found incumbent of value\s+([\d.eE+\-]+)\s+after\s+([\d.]+)\s+sec")
            .unwrap()
    })
}

fn re_presolve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolve time\s*=\s*([\d.]+)\s+sec").unwrap())
}

fn parse_status(text: &str, log: &mut SolverLog) {
    // Find "MIP - ..." line (not prefixed with "CPLEX> ")
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("CPLEX>") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("MIP - ") {
            log.termination.raw_reason = Some(rest.to_string());
            if rest.contains("nteger optimal") {
                log.termination.status = Status::Optimal;
            } else if rest.contains("infeasible") || rest.contains("Infeasible") {
                log.termination.status = Status::Infeasible;
            } else if rest.contains("unbounded") || rest.contains("Unbounded") {
                log.termination.status = Status::Unbounded;
            } else if rest.contains("Time limit") || rest.contains("time limit") {
                log.termination.status = Status::TimeLimit;
            } else if rest.contains("Node limit") || rest.contains("node limit") {
                log.termination.status = Status::OtherLimit;
            }
            return;
        }
        // LP-only: "Optimal:  Objective = ..."
        if trimmed.starts_with("Optimal:") || trimmed.starts_with("LP status = optimal") {
            log.termination.status = Status::Optimal;
            log.termination.raw_reason = Some(trimmed.to_string());
        }
    }
}

fn parse_cuts(text: &str, log: &mut SolverLog) {
    // Lines like: "  Gomory fractional cuts applied:  21"
    // Or: "  Lift and project cuts applied:  25"
    let re = re_cut_line();
    for c in re.captures_iter(text) {
        let name = c[1].trim().to_string();
        let count: u64 = c[2].replace(',', "").parse().unwrap_or(0);
        if count > 0 {
            log.cuts.insert(name, count);
        }
    }
}

/// Parse CPLEX B&B progress table.
/// Header: "   Node  Left     Objective  IInf  Best Integer    Best Bound    ItCnt     Gap"
/// Standard row: "     35    33  -106238.6135   180  -106025.2642  -108376.0418    28161    2.22%"
/// Incumbent row: "*     0+    0                      -102451.6002  -108398.9052             5.80%"
/// Elapsed line:  "Elapsed time = 5.02 sec. (7373.09 ticks, tree = 0.01 MB, solutions = 3)"
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;
    let mut current_time = 0.0f64;

    for line in text.lines() {
        if !in_table {
            if line.contains("Node  Left") && line.contains("Best Integer") {
                in_table = true;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Elapsed time markers give us the current time
        if let Some(c) = re_elapsed().captures(line) {
            if let Ok(t) = c[1].parse::<f64>() {
                current_time = t;
            }
            continue;
        }

        // End of table
        if trimmed.starts_with("Clique")
            || trimmed.starts_with("Cover")
            || trimmed.starts_with("Implied")
            || trimmed.starts_with("Flow")
            || trimmed.starts_with("Mixed")
            || trimmed.starts_with("Zero-half")
            || trimmed.starts_with("Gomory")
            || trimmed.starts_with("Lift")
            || trimmed.starts_with("GUB")
            || line.contains("cuts applied")
            || trimmed.starts_with("Root node")
            || trimmed.starts_with("MIP -")
            || trimmed.starts_with("Solution pool")
            || trimmed.starts_with("Repeating presolve")
        {
            break;
        }

        if let Some(row) = parse_row(line, current_time) {
            out.push(row);
        }
    }
    out
}

fn parse_row(line: &str, current_time: f64) -> Option<NodeSnapshot> {
    let marker = line.chars().next()?;
    let (event, body) = if marker == '*' {
        (Some(NodeEvent::Heuristic), &line[1..])
    } else {
        (None, line)
    };

    let toks: Vec<&str> = body.split_whitespace().collect();

    // CPLEX rows don't have a time column per row; we use the last "Elapsed time"
    // Standard: Node Left Objective IInf BestInteger BestBound ItCnt Gap
    // = 8 tokens (standard) or 6 (incumbent update, blanking Obj/IInf) or variable with "Cuts: N"

    // Skip lines with "Cuts:" — these are root cuts
    if line.contains("Cuts:") {
        // Still parse node/bestbound for root cut rows
        // Format: "      0     0  -114235.0679   346                   Cuts: 294    17625"
        // These have BestBound in the Objective column and Cuts in the BestBound column
        // We can still extract some info but it's tricky — skip for now.
        return None;
    }

    let has_marker = event.is_some();
    let mut snap = NodeSnapshot::default();
    snap.event = event;
    snap.time_seconds = current_time;

    // Parse node number — might have "+" suffix (e.g. "0+")
    let node_tok = toks.first()?;
    let node_str = node_tok.strip_suffix('+').unwrap_or(node_tok);
    snap.nodes_explored = node_str.replace(',', "").parse().ok();

    // CPLEX row shapes:
    // (A) Standard:          Node Left Obj IInf BestInt BestBd ItCnt Gap  (8 tok)
    // (B) Root no-incumbent: Node Left Obj IInf BestBd ItCnt             (6 tok, no gap)
    // (C) Incumbent update (* marker): Node Left BestInt BestBd Gap      (5 tok)
    //     or with ItCnt:               Node Left BestInt BestBd ItCnt Gap (6 tok)
    // Distinguish (B) vs (C-6tok): marker rows always have event set.

    match toks.len() {
        8 => {
            // (A) Standard: Node Left Obj IInf BestInt BestBd ItCnt Gap
            snap.primal = parse_or_dash(toks[4]);
            snap.dual = parse_or_dash(toks[5]);
            snap.lp_iterations = toks[6].replace(',', "").parse().ok();
            snap.gap = parse_gap(toks[7]);
        }
        7 if !has_marker => {
            // (D) Cutoff / terminating row: Node Left Obj|cutoff BestInt BestBd ItCnt Gap
            // (IInf omitted because the node was cut off — no LP solve).
            snap.primal = parse_or_dash(toks[3]);
            snap.dual = parse_or_dash(toks[4]);
            snap.lp_iterations = toks[5].replace(',', "").parse().ok();
            snap.gap = parse_gap(toks[6]);
        }
        6 if has_marker => {
            // (C) Incumbent update with ItCnt: Node Left BestInt BestBd ItCnt Gap
            snap.primal = parse_or_dash(toks[2]);
            snap.dual = parse_or_dash(toks[3]);
            snap.lp_iterations = toks[4].replace(',', "").parse().ok();
            snap.gap = parse_gap(toks[5]);
        }
        6 => {
            // (B) Root row without incumbent: Node Left Obj IInf BestBd ItCnt
            snap.dual = parse_or_dash(toks[4]);
            snap.lp_iterations = toks[5].replace(',', "").parse().ok();
        }
        5 if has_marker => {
            // (C) Incumbent update: Node Left BestInt BestBd Gap
            snap.primal = parse_or_dash(toks[2]);
            snap.dual = parse_or_dash(toks[3]);
            snap.gap = parse_gap(toks[4]);
        }
        5 => {
            // Root row variant: Node Left Obj IInf BestBd (no ItCnt)
            snap.dual = parse_or_dash(toks[4]);
        }
        4 if has_marker => {
            snap.primal = parse_or_dash(toks[2]);
            snap.dual = parse_or_dash(toks[3]);
        }
        _ => return None,
    }

    Some(snap)
}

fn re_version() -> &'static Regex {
    // CPLEX prints the version in two common forms:
    //   (a) "Welcome to IBM(R) ILOG(R) CPLEX(R) Interactive Optimizer 22.1.2.0"
    //       — emitted by the `cplex` CLI at startup.
    //   (b) "Version identifier: 22.1.2.0 | 2026-03-02 | af0ce9b93"
    //       — emitted when invoked through the C/Python API (no banner).
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?:CPLEX[^I]*Interactive Optimizer\s+|Version identifier:\s+)(\d+\.\d+\.\d+(?:\.\d+)?)",
        )
        .unwrap()
    })
}

fn re_problem() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Problem '([^']+)' read\.").unwrap())
}

fn re_reduced() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"Reduced MIP has\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns,\s+and\s+([\d,]+)\s+nonzeros",
        )
        .unwrap()
    })
}

fn re_objective() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"MIP - [^:]+:\s+Objective\s*=\s*([-\d.eE+]+)").unwrap())
}

fn re_solution_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Solution time\s*=\s*([\d.]+)\s+sec\.\s+Iterations\s*=\s*([\d,]+)\s+Nodes\s*=\s*([\d,]+)")
            .unwrap()
    })
}

fn re_root_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Root relaxation solution time\s*=\s*([\d.]+)\s+sec").unwrap())
}

fn re_sol_pool() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solution pool:\s+(\d+)\s+solutions? saved").unwrap())
}

fn re_cut_line() -> &'static Regex {
    // CPLEX prints these cut lines unindented (or with leading whitespace in
    // some versions) — use `\s*` so both variants match.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s*(.+?)\s+cuts applied:\s+([\d,]+)").unwrap())
}

fn re_elapsed() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Elapsed time\s*=\s*([\d.]+)\s+sec").unwrap())
}

fn re_total_time() -> &'static Regex {
    // "Total (root+branch&cut) = 2.00 sec. (3325.32 ticks)" — the canonical
    // CPLEX MIP wall time at termination.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Total\s*\(root\+branch&cut\)\s*=\s*([\d.]+)\s+sec").unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_cplex() {
        let p = CplexParser;
        assert!(p.sniff(
            "Welcome to IBM(R) ILOG(R) CPLEX(R) Interactive Optimizer 12.8.0.0\nCPXPARAM_TimeLimit"
        ));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_cplex_log() {
        let text = r#"
Welcome to IBM(R) ILOG(R) CPLEX(R) Interactive Optimizer 12.8.0.0
  with Simplex, Mixed Integer & Barrier Optimizers

CPLEX> CPXPARAM_TimeLimit                               7200
Problem 'instances/miplib2010/bab5.mps.gz' read.
Read time = 0.04 sec. (12.97 ticks)
Reduced MIP has 4665 rows, 21379 columns, and 91629 nonzeros.
Root relaxation solution time = 0.63 sec. (910.97 ticks)

        Nodes                                         Cuts/
   Node  Left     Objective  IInf  Best Integer    Best Bound    ItCnt     Gap

*     0+    0                      -102451.6002  -108398.9052             5.80%
*     0+    0                      -105884.5712  -108398.9052             2.37%
      0     2  -108398.9052   401  -106025.2642  -108398.9052    24130    2.24%
Elapsed time = 5.02 sec. (7373.09 ticks, tree = 0.01 MB, solutions = 3)
     35    33  -106238.6135   180  -106025.2642  -108376.0418    28161    2.22%

  Gomory fractional cuts applied:  21
  Lift and project cuts applied:  25

Solution pool: 10 solutions saved.

MIP - Integer optimal solution:  Objective = -1.0641184010e+05
Solution time = 1551.53 sec.  Iterations = 4932561  Nodes = 51737
"#;
        let log = CplexParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Cplex);
        assert_eq!(log.version.as_deref(), Some("12.8.0.0"));
        assert_eq!(log.problem.as_deref(), Some("bab5"));
        assert_eq!(log.termination.status, Status::Optimal);
        assert!((log.bounds.primal.unwrap() - (-1.064118401e+05)).abs() < 1.0);
        assert!((log.timing.wall_seconds.unwrap() - 1551.53).abs() < 0.01);
        assert_eq!(log.tree.nodes_explored, Some(51737));
        assert_eq!(log.tree.simplex_iterations, Some(4932561));
        assert_eq!(log.tree.solutions_found, Some(10));
        assert_eq!(log.presolve.rows_after, Some(4665));
        assert_eq!(log.progress.len(), 4); // 2 incumbent + 1 standard + 1 after elapsed
        eprintln!("cuts: {:?}", log.cuts);
        assert_eq!(*log.cuts.get("Gomory fractional").unwrap_or(&0), 21);
    }
}
