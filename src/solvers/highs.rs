//! HiGHS log parser. Tested against HiGHS 1.12–1.14 output.

use crate::solvers::progress::{parse_gap, parse_time_token};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct HighsParser;

impl LogParser for HighsParser {
    fn solver(&self) -> Solver {
        Solver::Highs
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("Running HiGHS") || text.contains("HiGHS run time")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("highs"));
        }
        let mut log = SolverLog::new(Solver::Highs);

        // Version + git hash: "Running HiGHS 1.12.0 (git hash: 62f9c446a):"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
            log.solver_git_hash = Some(c[2].to_string());
        }

        // Problem name: "LP foo has ..." or "MIP foo has ..." or "Model name : foo"
        if let Some(c) = re_problem_type().captures(text) {
            log.problem = Some(c[1].to_string());
            // Pre-presolve dims from same line
            log.presolve.rows_before = c[2].replace(',', "").parse().ok();
            log.presolve.cols_before = c[3].replace(',', "").parse().ok();
            log.presolve.nonzeros_before = c[4].replace(',', "").parse().ok();
        }
        // Also check "Model name" in solving report (more reliable for the name)
        if let Some(c) = re_model_name().captures(text) {
            log.problem = Some(c[1].trim().to_string());
        }

        // Presolve reductions: "Presolve reductions: rows N(-D); columns M(-D); nonzeros K(+/-D)"
        if let Some(c) = re_presolve_reductions().captures(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        // Status from solving report
        parse_status(text, &mut log);

        // Bounds from solving report
        if let Some(c) = re_primal_bound().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        }
        if let Some(c) = re_dual_bound().captures(text) {
            log.bounds.dual = c[1].parse().ok();
        }
        // LP-only fallback: "Objective value     :  -5.0e+00" + LP optimality
        // is duality-tight, so mirror into both bounds.
        if log.bounds.primal.is_none() {
            if let Some(c) = re_lp_obj_value().captures(text) {
                let v: Option<f64> = c[1].parse().ok();
                log.bounds.primal = v;
                if log.bounds.dual.is_none() {
                    log.bounds.dual = v;
                }
                if log.bounds.gap.is_none() && log.termination.status == Status::Optimal {
                    log.bounds.gap = Some(0.0);
                }
            }
        }
        if let Some(c) = re_gap().captures(text) {
            log.bounds.gap = c[1].parse::<f64>().ok().map(|v| v / 100.0);
        }

        // Timing: "HiGHS run time      :          0.28" or Solving report "Timing X.XX"
        if let Some(c) = re_run_time().captures(text) {
            log.timing.wall_seconds = c[1].trim().parse().ok();
        } else if let Some(c) = re_timing().captures(text) {
            log.timing.wall_seconds = c[1].trim().parse().ok();
        }

        // Nodes from solving report
        if let Some(c) = re_nodes().captures(text) {
            log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
        }

        // LP iterations — prefer the Solving report "LP iterations N" (the
        // authoritative total including strong-branching + separation +
        // heuristics), fall back to the older "Simplex iterations: N" line.
        if let Some(c) = re_lp_iterations().captures(text) {
            log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
        } else if let Some(c) = re_simplex_iters().captures(text) {
            log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
        }

        // Primal-dual integral: "P-D integral  0.0362067575694"
        if let Some(c) = re_pd_integral().captures(text) {
            log.bounds.primal_dual_integral = c[1].parse().ok();
        }

        // Max sub-MIP depth: "Max sub-MIP depth 2" — HiGHS specifically calls
        // this "sub-MIP depth" (depth of deepest sub-problem); it's the closest
        // analogue to SCIP/Gurobi `max_depth` HiGHS reports.
        if let Some(c) = re_max_depth().captures(text) {
            log.tree.max_depth = c[1].parse().ok();
        }

        // Restarts: HiGHS prints "restarting" each time the search restarts
        // after a batch of heuristic-driven LP reductions. Count occurrences.
        let restarts = text
            .lines()
            .filter(|l| l.contains("restarting") || l.starts_with("Model after restart"))
            .count();
        if restarts > 0 {
            // "restarting" and "Model after restart" appear pairwise; divide by 2 to avoid double-counting.
            log.tree.restarts = Some((restarts as u32) / 2);
        }

        // Progress table
        log.progress = parse_progress(text);

        // If solutions_found wasn't set but we have progress event rows,
        // infer count from distinct incumbents.
        if log.tree.solutions_found.is_none() && !log.progress.is_empty() {
            let mut last: Option<f64> = None;
            let mut count = 0u64;
            for i in 0..log.progress.len() {
                if let Some(p) = log.progress.primal[i] {
                    if last.map_or(true, |lp| (lp - p).abs() > 1e-9) {
                        count += 1;
                        last = Some(p);
                    }
                }
            }
            if count > 0 {
                log.tree.solutions_found = Some(count);
            }
        }

        // Solver-specific rich data.
        populate_other_data(text, &mut log);

        Ok(log)
    }
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_coefficient_ranges(text) {
        log.other_data
            .push(NamedValue::new("highs.coefficient_ranges", v));
    }
    if let Some(v) = parse_variable_types(text) {
        log.other_data
            .push(NamedValue::new("highs.variable_types_after_presolve", v));
    }
    if let Some(v) = parse_solution_quality(text) {
        log.other_data
            .push(NamedValue::new("highs.solution_quality", v));
    }
    if let Some(v) = parse_lp_iter_breakdown(text) {
        log.other_data
            .push(NamedValue::new("highs.lp_iteration_breakdown", v));
    }
}

/// Parse the "Coefficient ranges" block:
///   Coefficient ranges:
///     Matrix  [1e+00, 6e+01]
///     Cost    [5e+01, 1e+04]
///     Bound   [1e+00, 1e+00]
///     RHS     [1e+00, 5e+01]
fn parse_coefficient_ranges(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^Coefficient ranges:").unwrap());
    let m = hdr.find(text)?;
    let row = Regex::new(r"^\s+(Matrix|Cost|Bound|RHS)\s+\[([^,]+),\s*([^\]]+)\]").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(8) {
        if line.trim().is_empty() || !line.starts_with("  ") {
            break;
        }
        if let Some(c) = row.captures(line) {
            let name = c[1].to_lowercase();
            let mut inner = serde_json::Map::new();
            inner.insert("min".into(), parse_f64_or_str(c[2].trim()));
            inner.insert("max".into(), parse_f64_or_str(c[3].trim()));
            obj.insert(name, serde_json::Value::Object(inner));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// Parse the variable-type breakdown after presolve:
///   177 cols (174 binary, 3 integer, 0 implied int., 0 continuous, 0 domain fixed)
fn parse_variable_types(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(
            r"(\d+)\s+cols\s+\((\d+)\s+binary,\s*(\d+)\s+integer,\s*(\d+)\s+implied int\.,\s*(\d+)\s+continuous,\s*(\d+)\s+domain fixed\)",
        )
        .unwrap()
    });
    let c = re.captures(text)?;
    let mut obj = serde_json::Map::new();
    obj.insert("total".into(), parse_json_u64(&c[1]));
    obj.insert("binary".into(), parse_json_u64(&c[2]));
    obj.insert("integer".into(), parse_json_u64(&c[3]));
    obj.insert("implied_integer".into(), parse_json_u64(&c[4]));
    obj.insert("continuous".into(), parse_json_u64(&c[5]));
    obj.insert("domain_fixed".into(), parse_json_u64(&c[6]));
    Some(serde_json::Value::Object(obj))
}

/// Parse the Solving report "Solution status" quality numbers:
///     Solution status   feasible
///                       7615 (objective)
///                       0 (bound viol.)
///                       3.33066907388e-16 (int. viol.)
///                       0 (row viol.)
fn parse_solution_quality(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^\s+Solution status\s+\S+").unwrap());
    let m = hdr.find(text)?;
    let row = Regex::new(r"^\s+(\S+)\s+\(([a-z. ]+)\)").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(6) {
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(c) = row.captures(line) {
            let name = c[2]
                .trim()
                .replace(['.', ' '], "_")
                .replace("__", "_")
                .trim_end_matches('_')
                .to_string();
            obj.insert(name, parse_f64_or_str(&c[1]));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// Parse the LP-iteration breakdown under the "LP iterations" total:
///     LP iterations     24777
///                       21761 (strong br.)
///                       1063 (separation)
///                       695 (heuristics)
fn parse_lp_iter_breakdown(text: &str) -> Option<serde_json::Value> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = R.get_or_init(|| Regex::new(r"(?m)^\s+LP iterations\s+\d").unwrap());
    let m = hdr.find(text)?;
    let row = Regex::new(r"^\s+(\d+)\s+\(([^)]+)\)").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(6) {
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(c) = row.captures(line) {
            let name = c[2].trim().trim_end_matches('.').replace(['.', ' '], "_");
            obj.insert(name, parse_json_u64(&c[1]));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

fn parse_json_u64(s: &str) -> serde_json::Value {
    s.parse::<u64>()
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null)
}

fn parse_f64_or_str(s: &str) -> serde_json::Value {
    if let Ok(n) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.to_string())
}

fn parse_status(text: &str, log: &mut SolverLog) {
    // Solving report status line: "  Status            Optimal"
    // Also: "Model status        : Optimal"
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Status") {
            let status_str = rest.trim();
            set_status(status_str, log);
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("Model status") {
            // "Model status        : Optimal" → strip leading whitespace
            // BEFORE the colon; previously the trim ran in the wrong order
            // and left ": Optimal" as the status string.
            let status_str = rest.trim_start().trim_start_matches(':').trim();
            set_status(status_str, log);
            return;
        }
    }
}

fn set_status(s: &str, log: &mut SolverLog) {
    log.termination.raw_reason = Some(s.to_string());
    if s.starts_with("Optimal") {
        log.termination.status = Status::Optimal;
    } else if s.starts_with("Infeasible") {
        log.termination.status = Status::Infeasible;
    } else if s.starts_with("Unbounded") {
        log.termination.status = Status::Unbounded;
    } else if s.contains("Time limit") || s.contains("time limit") {
        log.termination.status = Status::TimeLimit;
    } else if s.to_lowercase().contains("iteration limit")
        || s.to_lowercase().contains("node limit")
        || s.to_lowercase().contains("solution limit")
        || s.to_lowercase().contains("objective limit")
        || s.to_lowercase().contains("interrupt")
    {
        log.termination.status = Status::OtherLimit;
    }
}

/// Parse the HiGHS B&B progress table.
/// Header: "Src  Proc. InQueue |  Leaves   Expl. | BestBound       BestSol              Gap |   Cuts   InLp Confl. | LpIters     Time"
/// Row shapes:
///   " C       0       0         0   0.00%   8659293.449101  57124437.1918     84.84%       75     21     24        76     0.0s"
///   "         0       0         0   0.00%   245121.59       inf                  inf        0      0      0         0     0.0s"
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;
    for line in text.lines() {
        if !in_table {
            if line.contains("Src  Proc. InQueue") {
                in_table = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            if !out.is_empty() {
                // Could be blank line inside table (after header), skip
                continue;
            }
            continue;
        }
        // Table ends only at the solving report / final summary. Restart
        // notices ("Model after restart has …", "… restarting") interrupt the
        // flow but don't end the table — rows continue without a new header.
        if line.starts_with("Solving report") {
            break;
        }
        // Skip restart narration + coefficient-range side-notes inside the table.
        if line.starts_with("Model after restart")
            || line.contains("restarting")
            || line.contains("inactive integer columns")
        {
            continue;
        }
        if let Some(row) = parse_row(line) {
            out.push(row);
        }
    }
    out
}

fn parse_row(line: &str) -> Option<NodeSnapshot> {
    // First 2 chars may be a source marker (e.g. " C", " L", " S", " T", " H", etc.)
    let marker_part = &line[..std::cmp::min(2, line.len())];
    let marker_char = marker_part.trim();
    let event = if marker_char.is_empty() {
        None
    } else {
        highs_event(marker_char)
    };

    let toks: Vec<&str> = line.split_whitespace().collect();
    // Minimum: Proc InQueue Leaves Expl% BestBound BestSol Gap Cuts InLp Confl LpIters Time
    // With marker: Src Proc InQueue Leaves Expl% BestBound BestSol Gap Cuts InLp Confl LpIters Time
    // = 12 or 13 tokens

    // Find time token (last token, ends with 's')
    let time_tok = toks.last()?;
    let time = parse_time_token(time_tok)?;

    // Work backwards: Time LpIters Confl InLp Cuts Gap BestSol BestBound Expl% Leaves InQueue Proc [Src]
    let n = toks.len();
    if n < 12 {
        return None;
    }

    // Determine if first token is a marker (non-numeric) or Proc (numeric)
    let offset = if toks[0].parse::<u64>().is_ok() { 0 } else { 1 };
    if n < 12 + offset {
        return None;
    }

    let mut snap = NodeSnapshot::default();
    snap.time_seconds = time;
    snap.event = event;

    // Proc = nodes processed, InQueue = open nodes
    snap.nodes_explored = toks[offset].parse().ok();
    // Leaves = offset+2, Expl% = offset+3 (skip)

    // BestBound, BestSol, Gap
    snap.dual = parse_or_dash_or_inf(toks[offset + 4]);
    snap.primal = parse_or_dash_or_inf(toks[offset + 5]);
    snap.gap = parse_gap(toks[offset + 6]);

    // LpIters = n-2
    snap.lp_iterations = toks[n - 2].replace(',', "").parse().ok();

    Some(snap)
}

fn parse_or_dash_or_inf(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "-" || t.is_empty() || t.eq_ignore_ascii_case("inf") {
        None
    } else {
        t.parse().ok()
    }
}

fn highs_event(marker: &str) -> Option<NodeEvent> {
    match marker {
        "H" => Some(NodeEvent::Heuristic),
        "C" | "F" | "I" | "J" | "L" | "R" | "Z" | "l" | "p" | "u" | "z" => {
            Some(NodeEvent::Heuristic)
        }
        "S" | "T" | "B" => Some(NodeEvent::BranchSolution),
        _ => Some(NodeEvent::Other(marker.to_string())),
    }
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Running HiGHS\s+(\d+\.\d+\.\d+)\s+\(git hash:\s*([0-9a-f]+)\)").unwrap()
    })
}

fn re_problem_type() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?:LP|MIP)\s+(\S+)\s+has\s+([\d,]+)\s+rows?;\s+([\d,]+)\s+cols?;\s+([\d,]+)\s+nonzeros").unwrap()
    })
}

fn re_model_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Model name\s*:\s*(\S+)").unwrap())
}

fn re_presolve_reductions() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Presolve reductions:\s*rows\s+([\d,]+)\([^)]*\);\s*columns\s+([\d,]+)\([^)]*\);\s*nonzeros\s+([\d,]+)")
            .unwrap()
    })
}

fn re_primal_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Primal bound\s+([-\d.eE+]+)").unwrap())
}

fn re_lp_obj_value() -> &'static Regex {
    // LP-only termination: "Objective value     :  -5.0000000000e+00"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^Objective value\s*:\s*([-\d.eE+]+)").unwrap())
}

fn re_dual_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Dual bound\s+([-\d.eE+]+)").unwrap())
}

fn re_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Gap\s+([\d.]+)%").unwrap())
}

fn re_run_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"HiGHS run time\s*:\s*([\d.]+)").unwrap())
}

fn re_timing() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Timing\s+([\d.]+)").unwrap())
}

fn re_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Nodes\s+([\d,]+)").unwrap())
}

fn re_simplex_iters() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Simplex\s+iterations:\s*([\d,]+)").unwrap())
}

fn re_lp_iterations() -> &'static Regex {
    // Solving report: "  LP iterations     24777"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+LP iterations\s+([\d,]+)").unwrap())
}

fn re_pd_integral() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"P-D integral\s+([\d.eE+\-]+)").unwrap())
}

fn re_max_depth() -> &'static Regex {
    // HiGHS: "Max sub-MIP depth 2"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Max sub-MIP depth\s+(\d+)").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_highs() {
        let p = HighsParser;
        assert!(p.sniff("Running HiGHS 1.12.0 (git hash: 62f9c446a): Copyright"));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_mip_log() {
        let text = r#"Running HiGHS 1.12.0 (git hash: 62f9c446a): Copyright (c) 2025 HiGHS under MIT licence terms
MIP bell5 has 91 rows; 104 cols; 266 nonzeros
Presolve reductions: rows 81(-10); columns 98(-6); nonzeros 242(-24)

Solving MIP model with:
   81 rows
   98 cols (28 binary, 28 integer, 0 implied int., 42 continuous, 0 domain fixed)
   242 nonzeros

Src: B => Branching; C => Central rounding;

        Nodes      |    B&B Tree     |            Objective Bounds              |  Dynamic Constraints |       Work
Src  Proc. InQueue |  Leaves   Expl. | BestBound       BestSol              Gap |   Cuts   InLp Confl. | LpIters     Time

         0       0         0   0.00%   245121.59       inf                  inf        0      0      0         0     0.0s
 C       0       0         0   0.00%   8659293.449101  57124437.1918     84.84%       75     21     24        76     0.0s
 L       0       0         0   0.00%   8660780.823234  8974250.01376      3.49%       80     27     24        84     0.1s
       405       0       106 100.00%   8965513.731286  8966406.49152      0.01%      108     25    358      2796     0.2s

Solving report
  Model             bell5
  Status            Optimal
  Primal bound      8966406.49152
  Dual bound        8965513.73129
  Gap               0.00996% (tolerance: 0.01%)
  Timing            0.25
  Nodes             405
  LP iterations     2796
Model name          : bell5
Model status        : Optimal
Simplex   iterations: 2796
Objective value     :  8.9664064915e+06
HiGHS run time      :          0.25
"#;
        let log = HighsParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Highs);
        assert_eq!(log.version.as_deref(), Some("1.12.0"));
        assert_eq!(log.solver_git_hash.as_deref(), Some("62f9c446a"));
        assert_eq!(log.problem.as_deref(), Some("bell5"));
        assert_eq!(log.termination.status, Status::Optimal);
        assert!((log.bounds.primal.unwrap() - 8966406.49152).abs() < 1.0);
        assert!((log.bounds.dual.unwrap() - 8965513.73129).abs() < 1.0);
        assert!((log.timing.wall_seconds.unwrap() - 0.25).abs() < 0.01);
        assert_eq!(log.tree.nodes_explored, Some(405));
        assert_eq!(log.tree.simplex_iterations, Some(2796));
        assert_eq!(log.presolve.rows_before, Some(91));
        assert_eq!(log.presolve.cols_before, Some(104));
        assert_eq!(log.presolve.rows_after, Some(81));
        assert_eq!(log.presolve.cols_after, Some(98));
        assert_eq!(log.progress.len(), 4);
    }
}
