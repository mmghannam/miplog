//! HiGHS log parser. Tested against HiGHS 1.12 output.

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

        // LP iterations: "Simplex   iterations: N" or "LP iterations N"
        if let Some(c) = re_simplex_iters().captures(text) {
            log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
        }

        // Progress table
        log.progress = parse_progress(text);

        Ok(log)
    }
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
            let status_str = rest.trim_start_matches(':').trim();
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
    } else if s.contains("iteration limit") || s.contains("node limit") {
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
        // Table ends at "Solving report" or "Restarting" or next section
        if line.starts_with("Solving report")
            || line.starts_with("Restarting")
            || line.starts_with("Model after restart")
        {
            // After restart, a new table may begin — keep going
            if line.starts_with("Restarting") || line.starts_with("Model after restart") {
                in_table = false; // will re-enter on next header
                continue;
            }
            break;
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
