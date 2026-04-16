//! CBC (COIN-OR Branch and Cut) log parser. Tested against CBC 2.9–2.10.

use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct CbcParser;

impl LogParser for CbcParser {
    fn solver(&self) -> Solver {
        Solver::Cbc
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("Welcome to the CBC MILP Solver")
            || (text.contains("Cbc0") && text.contains("CBC"))
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("cbc"));
        }
        let mut log = SolverLog::new(Solver::Cbc);

        // Version: "Version: 2.9.8"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        // Problem name: "Problem bab5 has 4964 rows, 21600 columns and 155520 elements"
        if let Some(c) = re_problem().captures(text) {
            log.problem = Some(c[1].to_string());
            log.presolve.rows_before = c[2].replace(',', "").parse().ok();
            log.presolve.cols_before = c[3].replace(',', "").parse().ok();
            log.presolve.nonzeros_before = c[4].replace(',', "").parse().ok();
        }

        // Presolve after: "processed model has R rows, C columns (I integer ...) and N elements"
        if let Some(c) = re_processed().captures(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        // Status
        parse_status(text, &mut log);

        // Result section: "Objective value: -104286.92120000" (MIP)
        // or LP: "Optimal - objective value -5" / "Optimal objective -5"
        if let Some(c) = re_obj_value().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        } else if let Some(c) = re_lp_obj().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        }
        // "Lower bound: -111273.306"
        if let Some(c) = re_lower_bound().captures(text) {
            log.bounds.dual = c[1].parse().ok();
        }
        // "Gap: 0.06"
        if let Some(c) = re_gap().captures(text) {
            log.bounds.gap = c[1].parse().ok();
        }

        // Time: "Total time (CPU seconds):  7194.79   (Wallclock seconds):  7199.45"
        // or: "Partial search ... took N iterations and M nodes (T seconds)"
        if let Some(c) = re_total_time().captures(text) {
            log.timing.cpu_seconds = c[1].parse().ok();
            log.timing.wall_seconds = c[2].parse().ok();
        }

        // Nodes + iterations from Cbc0005I line:
        // "Cbc0005I Partial search - best objective -104286.92 (best possible -111273.31), took 20695956 iterations and 162253 nodes (7199.24 seconds)"
        if let Some(c) = re_cbc0005().captures(text) {
            log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
            log.tree.nodes_explored = c[2].replace(',', "").parse().ok();
            if log.timing.wall_seconds.is_none() {
                log.timing.wall_seconds = c[3].parse().ok();
            }
        }
        // "Enumerated nodes: 162253"
        if log.tree.nodes_explored.is_none() {
            if let Some(c) = re_enum_nodes().captures(text) {
                log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
            }
        }
        // "Total iterations: 20695956"
        if log.tree.simplex_iterations.is_none() {
            if let Some(c) = re_total_iters().captures(text) {
                log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
            }
        }

        // Cuts
        parse_cuts(text, &mut log);

        // Progress: Cbc0010I lines + Cbc0004I (incumbent)
        log.progress = parse_progress(text);

        // CBC doesn't print a separate "Best bound" line on optimal runs —
        // but optimality means primal == dual by definition. Mirror primal
        // into dual so cross-solver tools can treat the field uniformly.
        if log.termination.status == Status::Optimal && log.bounds.dual.is_none() {
            log.bounds.dual = log.bounds.primal;
            log.bounds.gap = Some(0.0);
        }

        // Max depth: "Maximum depth 10"
        if let Some(c) = re_max_depth().captures(text) {
            log.tree.max_depth = c[1].parse().ok();
        }

        // Root LP after cuts: "Cuts at root node changed objective from 7155 to 7432.56"
        if let Some(c) = re_root_dual().captures(text) {
            log.bounds.root_dual = c[2].parse().ok();
        }

        // First feasible solution from feasibility pump:
        // "Integer solution of 8115 found by feasibility pump after 0 iterations and 0 nodes (0.03 seconds)"
        if let Some(c) = re_first_feasible().captures(text) {
            log.bounds.first_primal = c[1].parse().ok();
            log.bounds.first_primal_time_seconds = c[2].parse().ok();
        }

        populate_other_data(text, &mut log);

        Ok(log)
    }
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_cut_details(text) {
        log.other_data.push(NamedValue::new("cbc.cut_generators", v));
    }
    if let Some(v) = parse_strong_branching(text) {
        log.other_data.push(NamedValue::new("cbc.strong_branching", v));
    }
    if let Some(v) = parse_root_lp(text) {
        log.other_data.push(NamedValue::new("cbc.root_lp", v));
    }
    if let Some(v) = parse_continuous_obj(text) {
        log.other_data.push(NamedValue::new("cbc.continuous_objective", v));
    }
}

/// Parse the per-cut-generator detail lines:
///   Cbc0014I Cut generator 0 (Probing) - 5 row cuts ... 0 column cuts (1 active)  in 0.034 seconds
fn parse_cut_details(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(
        r"Cbc0014I Cut generator \d+ \(([A-Za-z0-9]+)\)\s*-\s*(\d+) row cuts.*?(\d+) column cuts \((\d+) active\)\s+in\s+([\d.]+)\s+seconds",
    )
    .unwrap();
    let mut arr: Vec<serde_json::Value> = Vec::new();
    for c in re.captures_iter(text) {
        let mut o = serde_json::Map::new();
        o.insert("name".into(), serde_json::Value::String(c[1].to_string()));
        o.insert("row_cuts".into(), parse_f64_json_cbc(&c[2]));
        o.insert("column_cuts".into(), parse_f64_json_cbc(&c[3]));
        o.insert("active".into(), parse_f64_json_cbc(&c[4]));
        o.insert("time_seconds".into(), parse_f64_json_cbc(&c[5]));
        arr.push(serde_json::Value::Object(o));
    }
    (!arr.is_empty()).then(|| serde_json::Value::Array(arr))
}

/// "Cbc0032I Strong branching done 1270 times (27431 iterations), fathomed 10 nodes and fixed 24 variables"
fn parse_strong_branching(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(
        r"Strong branching done (\d+) times \((\d+) iterations\), fathomed (\d+) nodes and fixed (\d+) variables",
    )
    .unwrap()
    .captures(text)?;
    let mut o = serde_json::Map::new();
    o.insert("times".into(), parse_f64_json_cbc(&c[1]));
    o.insert("iterations".into(), parse_f64_json_cbc(&c[2]));
    o.insert("fathomed_nodes".into(), parse_f64_json_cbc(&c[3]));
    o.insert("fixed_variables".into(), parse_f64_json_cbc(&c[4]));
    Some(serde_json::Value::Object(o))
}

/// "At root node, 10 cuts changed objective from 7155 to 7432.5624 in 100 passes"
fn parse_root_lp(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(
        r"At root node, (\d+) cuts changed objective from\s+([-\d.eE+]+)\s+to\s+([-\d.eE+]+)\s+in\s+(\d+)\s+passes",
    )
    .unwrap()
    .captures(text)?;
    let mut o = serde_json::Map::new();
    o.insert("cuts".into(), parse_f64_json_cbc(&c[1]));
    o.insert("objective_before".into(), parse_f64_json_cbc(&c[2]));
    o.insert("objective_after".into(), parse_f64_json_cbc(&c[3]));
    o.insert("passes".into(), parse_f64_json_cbc(&c[4]));
    Some(serde_json::Value::Object(o))
}

/// "Continuous objective value is 6875 - 0.00 seconds"
fn parse_continuous_obj(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(
        r"Continuous objective value is\s+([-\d.eE+]+)\s+-\s+([\d.]+)\s+seconds",
    )
    .unwrap()
    .captures(text)?;
    let mut o = serde_json::Map::new();
    o.insert("value".into(), parse_f64_json_cbc(&c[1]));
    o.insert("time_seconds".into(), parse_f64_json_cbc(&c[2]));
    Some(serde_json::Value::Object(o))
}

fn parse_f64_json_cbc(s: &str) -> serde_json::Value {
    if let Ok(v) = s.trim().parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.trim().to_string())
}

fn re_max_depth() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Maximum depth\s+(\d+)").unwrap())
}

fn re_root_dual() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"At root node, \d+ cuts changed objective from\s+([-\d.eE+]+)\s+to\s+([-\d.eE+]+)",
        )
        .unwrap()
    })
}

fn re_first_feasible() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"Integer solution of\s+([-\d.eE+]+)\s+found by feasibility pump after \d+ iterations and \d+ nodes \(([\d.]+)\s+seconds\)",
        )
        .unwrap()
    })
}

fn parse_status(text: &str, log: &mut SolverLog) {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Result - ") {
            log.termination.raw_reason = Some(rest.to_string());
            if rest.contains("Optimal") {
                log.termination.status = Status::Optimal;
            } else if rest.contains("Infeasible") || rest.contains("infeasible") {
                log.termination.status = Status::Infeasible;
            } else if rest.contains("Unbounded") || rest.contains("unbounded") {
                log.termination.status = Status::Unbounded;
            } else if rest.contains("time limit") || rest.contains("Stopped on time") {
                log.termination.status = Status::TimeLimit;
            } else if rest.contains("node limit") || rest.contains("iteration limit") {
                log.termination.status = Status::OtherLimit;
            }
            return;
        }
    }
    // Fallback: "Cbc0001I Search completed"
    if text.contains("Search completed") {
        log.termination.status = Status::Optimal;
        log.termination.raw_reason = Some("Search completed".into());
        return;
    }
    // LP-only termination: "Optimal - objective value X" / "Optimal objective X"
    if text.contains("Optimal - objective value") || text.contains("Optimal objective ") {
        log.termination.status = Status::Optimal;
        log.termination.raw_reason = Some("Optimal".into());
        return;
    }
    // Infeasibility detected in presolve (no "Result -" line emitted):
    //   "Problem is infeasible - 0.00 seconds"
    if text.contains("Problem is infeasible") {
        log.termination.status = Status::Infeasible;
        log.termination.raw_reason = Some("Problem is infeasible".into());
    }
}

fn parse_cuts(text: &str, log: &mut SolverLog) {
    // "Probing was tried 42091 times and created 33082 cuts ..."
    let re = re_cut_line();
    for c in re.captures_iter(text) {
        let name = c[1].to_string();
        let count: u64 = c[2].replace(',', "").parse().unwrap_or(0);
        if count > 0 {
            log.cuts.insert(name, count);
        }
    }
}

/// Parse CBC progress from Cbc0010I and Cbc0004I lines.
///
/// Cbc0010I: "After N nodes, M on tree, OBJ best solution, best possible DUAL (T seconds)"
/// Cbc0004I: "Integer solution of OBJ found after I iterations and N nodes (T seconds)"
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();

    for line in text.lines() {
        if let Some(c) = re_cbc0010().captures(line) {
            let mut snap = NodeSnapshot::default();
            snap.nodes_explored = c[1].replace(',', "").parse().ok();
            snap.primal = parse_obj(&c[3]);
            snap.dual = c[4].parse().ok();
            snap.time_seconds = c[5].parse().unwrap_or(0.0);
            out.push(snap);
        } else if let Some(c) = re_cbc0004().captures(line) {
            let mut snap = NodeSnapshot::default();
            snap.primal = c[1].parse().ok();
            snap.lp_iterations = c[2].replace(',', "").parse().ok();
            snap.nodes_explored = c[3].replace(',', "").parse().ok();
            snap.time_seconds = c[4].parse().unwrap_or(0.0);
            snap.event = Some(NodeEvent::Heuristic);
            out.push(snap);
        }
    }
    out
}

/// Parse an objective value that might be "1e+50" (meaning no solution).
fn parse_obj(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    if v.abs() > 1e+40 {
        None // sentinel for "no solution"
    } else {
        Some(v)
    }
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Version:\s*(\d+\.\d+\.\d+)").unwrap())
}

fn re_problem() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Problem\s+(\S+)\s+has\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\s+and\s+([\d,]+)\s+elements")
            .unwrap()
    })
}

fn re_processed() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"processed model has\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\b.*?\band\s+([\d,]+)\s+elements")
            .unwrap()
    })
}

fn re_obj_value() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Objective value:\s+([-\d.eE+]+)").unwrap())
}
fn re_lp_obj() -> &'static Regex {
    // CBC LP-only termination: "Optimal - objective value X" / "Optimal objective X"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Optimal(?:\s+-\s+| )objective(?:\s+value)?\s+([-\d.eE+]+)").unwrap())
}

fn re_lower_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Lower bound:\s+([-\d.eE+]+)").unwrap())
}

fn re_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^Gap:\s+([\d.]+)").unwrap())
}

fn re_total_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Total time \(CPU seconds\):\s+([\d.]+)\s+\(Wallclock seconds\):\s+([\d.]+)")
            .unwrap()
    })
}

fn re_cbc0005() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Cbc0005I.*took\s+([\d,]+)\s+iterations\s+and\s+([\d,]+)\s+nodes\s+\(([\d.]+)\s+seconds\)")
            .unwrap()
    })
}

fn re_enum_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Enumerated nodes:\s+([\d,]+)").unwrap())
}

fn re_total_iters() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Total iterations:\s+([\d,]+)").unwrap())
}

fn re_cut_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(\w+)\s+was tried\s+\d+\s+times and created\s+([\d,]+)\s+cuts").unwrap()
    })
}

fn re_cbc0010() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Cbc0010I After\s+([\d,]+)\s+nodes,\s+([\d,]+)\s+on tree,\s+([-\d.eE+]+)\s+best solution,\s+best possible\s+([-\d.eE+]+)\s+\(([\d.]+)\s+seconds\)")
            .unwrap()
    })
}

fn re_cbc0004() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Cbc0004I Integer solution of\s+([-\d.eE+]+)\s+found after\s+([\d,]+)\s+iterations and\s+([\d,]+)\s+nodes\s+\(([\d.]+)\s+seconds\)")
            .unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_cbc() {
        let p = CbcParser;
        assert!(p.sniff("Welcome to the CBC MILP Solver\nVersion: 2.9.8"));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_cbc_log() {
        let text = r#"Welcome to the CBC MILP Solver
Version: 2.9.8
Build Date: Jun 10 2016

Problem bab5 has 4964 rows, 21600 columns and 155520 elements
Cgl0004I processed model has 4509 rows, 21151 columns (21151 integer (21151 of which binary)) and 163311 elements
Cbc0010I After 0 nodes, 1 on tree, 1e+50 best solution, best possible -112145.45 (12.35 seconds)
Cbc0004I Integer solution of -95115.013 found after 266658 iterations and 2044 nodes (119.43 seconds)
Cbc0010I After 100 nodes, 61 on tree, 1e+50 best solution, best possible -112145.45 (23.02 seconds)

Probing was tried 42091 times and created 33082 cuts of which 0 were active
Gomory was tried 39729 times and created 13630 cuts of which 0 were active

Result - Stopped on time limit

Objective value:                -104286.92120000
Lower bound:                    -111273.306
Gap:                            0.06
Enumerated nodes:               162253
Total iterations:               20695956
Time (CPU seconds):             7194.70
Time (Wallclock seconds):       7199.35

Total time (CPU seconds):       7194.79   (Wallclock seconds):       7199.45
"#;
        let log = CbcParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Cbc);
        assert_eq!(log.version.as_deref(), Some("2.9.8"));
        assert_eq!(log.problem.as_deref(), Some("bab5"));
        assert_eq!(log.termination.status, Status::TimeLimit);
        assert!((log.bounds.primal.unwrap() - (-104286.9212)).abs() < 0.01);
        assert!((log.bounds.dual.unwrap() - (-111273.306)).abs() < 0.01);
        assert!((log.bounds.gap.unwrap() - 0.06).abs() < 0.001);
        assert!((log.timing.wall_seconds.unwrap() - 7199.45).abs() < 0.01);
        assert_eq!(log.tree.nodes_explored, Some(162253));
        assert_eq!(log.tree.simplex_iterations, Some(20695956));
        assert_eq!(log.presolve.rows_before, Some(4964));
        assert_eq!(log.presolve.rows_after, Some(4509));
        assert_eq!(log.progress.len(), 3); // 2 Cbc0010I + 1 Cbc0004I
        assert_eq!(*log.cuts.get("Probing").unwrap_or(&0), 33082);
        assert_eq!(*log.cuts.get("Gomory").unwrap_or(&0), 13630);
    }
}
