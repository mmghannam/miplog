//! COPT (Cardinal Optimizer) log parser. Tested against COPT 8.0.3 output.

use crate::solvers::progress::{event_from_marker, parse_gap, parse_time_token};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct CoptParser;

impl LogParser for CoptParser {
    fn solver(&self) -> Solver {
        Solver::Copt
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("Cardinal Optimizer") || text.contains("Exiting COPT")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("copt"));
        }
        let mut log = SolverLog::new(Solver::Copt);

        // Version: "Cardinal Optimizer v8.0.3. Build date Jan 13 2026"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        // Problem name from reading line: "Reading from '.../p_30n20b8.mps.gz'"
        if let Some(c) = re_reading().captures(text) {
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

        // Original problem dims:
        // "    576 rows, 18380 columns and 109706 non-zero elements"
        if let Some(c) = re_original().captures(text) {
            log.presolve.rows_before = c[1].replace(',', "").parse().ok();
            log.presolve.cols_before = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_before = c[3].replace(',', "").parse().ok();
        }

        // Presolved dims (take the last "The presolved problem has:" block):
        for c in re_presolved().captures_iter(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        // Status — two fields:
        // "MIP status      : solved" / "stopped (time limit reached)" / "stopped (memory exceeded)"
        // "Solution status : integer optimal ..." / "infeasible" / "integer feasible" / "unknown"
        parse_status(text, &mut log);

        // Bounds from summary:
        // "Best solution   : 302.000000000"
        // "Best bound      : 302.000000000"
        // "Best gap        : 0.0000%"
        if let Some(c) = re_best_sol().captures(text) {
            log.bounds.primal = parse_copt_bound(&c[1]);
        }
        if let Some(c) = re_best_bound().captures(text) {
            log.bounds.dual = parse_copt_bound(&c[1]);
        }
        if let Some(c) = re_best_gap().captures(text) {
            log.bounds.gap = c[1].parse::<f64>().ok().map(|v| v / 100.0);
        }

        // Time / nodes:
        // "Solve time      : 2.83"
        // "Solve node      : 1"
        if let Some(c) = re_solve_time().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }
        if let Some(c) = re_solve_node().captures(text) {
            log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
        }

        // Progress table
        log.progress = parse_progress(text);

        // Derive Extended fields from progress + log text.

        // First incumbent: first row with an event (H = heuristic) and a primal.
        for i in 0..log.progress.len() {
            if log.progress.event[i].is_some() {
                if let Some(p) = log.progress.primal[i] {
                    log.bounds.first_primal = Some(p);
                    log.bounds.first_primal_time_seconds = Some(log.progress.time_seconds[i]);
                    break;
                }
            }
        }

        // Root LP dual bound: first non-zero BestBound in the progress table.
        for i in 0..log.progress.len() {
            if let Some(d) = log.progress.dual[i] {
                if d != 0.0 {
                    log.bounds.root_dual = Some(d);
                    break;
                }
            }
        }

        // Solutions found: count event rows in progress (each H is an incumbent).
        let sols = log.progress.event.iter().filter(|e| e.is_some()).count() as u64;
        if sols > 0 && log.tree.solutions_found.is_none() {
            log.tree.solutions_found = Some(sols);
        }

        populate_other_data(text, &mut log);

        Ok(log)
    }
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_machine(text) {
        log.other_data.push(NamedValue::new("copt.machine", v));
    }
    if let Some(v) = parse_run_config(text) {
        log.other_data.push(NamedValue::new("copt.run_config", v));
    }
    if let Some(c) = re_fingerprint().captures(text) {
        log.other_data.push(NamedValue::new(
            "copt.model_fingerprint",
            serde_json::Value::String(c[1].to_string()),
        ));
    }
    let (before, after) = parse_variable_types(text);
    if let Some(v) = before {
        log.other_data
            .push(NamedValue::new("copt.variable_types_before_presolve", v));
    }
    if let Some(v) = after {
        log.other_data
            .push(NamedValue::new("copt.variable_types_after_presolve", v));
    }
    if let Some(v) = parse_coefficient_ranges(text) {
        log.other_data
            .push(NamedValue::new("copt.coefficient_ranges", v));
    }
    if let Some(v) = parse_violations(text) {
        log.other_data
            .push(NamedValue::new("copt.solution_quality", v));
    }
}

fn parse_machine(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    if let Some(c) = Regex::new(r"Cardinal Optimizer\s+\S+\s+on\s+(.+)")
        .unwrap()
        .captures(text)
    {
        obj.insert(
            "platform".into(),
            serde_json::Value::String(c[1].trim().to_string()),
        );
    }
    if let Some(c) = Regex::new(r"The CPU model is\s+(.+)")
        .unwrap()
        .captures(text)
    {
        obj.insert(
            "cpu".into(),
            serde_json::Value::String(c[1].trim().to_string()),
        );
    }
    if let Some(c) = Regex::new(
        r"Hardware has\s+(\d+)\s+physical cores?\s+and\s+(\d+)\s+logical cores?\.\s*Using instruction set\s+(\S+)",
    )
    .unwrap()
    .captures(text)
    {
        obj.insert("physical_cores".into(), parse_f64_json(&c[1]));
        obj.insert("logical_cores".into(), parse_f64_json(&c[2]));
        obj.insert("instruction_set".into(), serde_json::Value::String(c[3].to_string()));
    }
    (!obj.is_empty()).then_some(serde_json::Value::Object(obj))
}

fn parse_run_config(text: &str) -> Option<serde_json::Value> {
    let c = Regex::new(r"Starting the MIP solver with\s+(\d+)\s+threads? and\s+(\d+)\s+tasks?")
        .unwrap()
        .captures(text)?;
    let mut obj = serde_json::Map::new();
    obj.insert("threads".into(), parse_f64_json(&c[1]));
    obj.insert("tasks".into(), parse_f64_json(&c[2]));
    Some(serde_json::Value::Object(obj))
}

fn re_fingerprint() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Model fingerprint:\s+(\S+)").unwrap())
}

/// COPT prints variable-type breakdowns both before and after presolve.
/// Before:  "    201 binaries"
/// After:   "    177 binaries and 3 integers"
fn parse_variable_types(text: &str) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let mut before = None;
    // Before: immediately after "The original problem has:" block — look for
    // a line like "    201 binaries" (standalone counts).
    static ORIG_HDR: OnceLock<Regex> = OnceLock::new();
    static BEFORE_LINE: OnceLock<Regex> = OnceLock::new();
    static AFTER_HDR: OnceLock<Regex> = OnceLock::new();
    static AFTER_LINE: OnceLock<Regex> = OnceLock::new();
    let orig_hdr = ORIG_HDR.get_or_init(|| Regex::new(r"(?m)^The original problem has:").unwrap());
    let before_line = BEFORE_LINE
        .get_or_init(|| Regex::new(r"^\s+(\d+)\s+(binaries|integers|continuous)").unwrap());
    if let Some(m) = orig_hdr.find(text) {
        for line in text[m.end()..].lines().take(6) {
            if let Some(c) = before_line.captures(line) {
                let mut obj = serde_json::Map::new();
                obj.insert(c[2].to_string(), parse_f64_json(&c[1]));
                before = Some(serde_json::Value::Object(obj));
                break;
            }
        }
    }
    let after_hdr =
        AFTER_HDR.get_or_init(|| Regex::new(r"(?m)^The presolved problem has:").unwrap());
    let after_line =
        AFTER_LINE.get_or_init(|| Regex::new(r"(\d+)\s+(binaries|integers|continuous)").unwrap());
    let mut after = None;
    if let Some(m) = after_hdr.find(text) {
        for line in text[m.end()..].lines().take(6) {
            let mut obj = serde_json::Map::new();
            let mut found = false;
            for c in after_line.captures_iter(line) {
                obj.insert(c[2].to_string(), parse_f64_json(&c[1]));
                found = true;
            }
            if found {
                after = Some(serde_json::Value::Object(obj));
                break;
            }
        }
    }
    (before, after)
}

/// "Range of matrix coefficients: [1e+00,2e+01]" + three similar lines.
fn parse_coefficient_ranges(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(r"Range of (matrix|rhs|bound|cost) coefficients:\s*\[([^,]+),([^\]]+)\]")
        .unwrap();
    let mut obj = serde_json::Map::new();
    for c in re.captures_iter(text) {
        let name = c[1].to_string();
        let mut inner = serde_json::Map::new();
        inner.insert("min".into(), parse_f64_json(c[2].trim()));
        inner.insert("max".into(), parse_f64_json(c[3].trim()));
        obj.insert(name, serde_json::Value::Object(inner));
    }
    (!obj.is_empty()).then_some(serde_json::Value::Object(obj))
}

fn parse_violations(text: &str) -> Option<serde_json::Value> {
    let hdr = Regex::new(r"(?m)^Violations\s*:").unwrap();
    let m = hdr.find(text)?;
    let row =
        Regex::new(r"^\s+(bounds|rows|integrality)\s*:\s+([\d.eE+\-]+)\s+([\d.eE+\-]+)?").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(4) {
        if line.trim().is_empty() {
            break;
        }
        if let Some(c) = row.captures(line) {
            let mut inner = serde_json::Map::new();
            inner.insert("absolute".into(), parse_f64_json(&c[1]));
            if let Some(rel) = c.get(3) {
                inner.insert("relative".into(), parse_f64_json(rel.as_str()));
            }
            // Wait — c[1] is the name, not the value. Fix order.
            let name = c[1].to_string();
            let mut inner2 = serde_json::Map::new();
            inner2.insert("absolute".into(), parse_f64_json(&c[2]));
            if let Some(rel) = c.get(3) {
                inner2.insert("relative".into(), parse_f64_json(rel.as_str()));
            }
            obj.insert(name, serde_json::Value::Object(inner2));
        }
    }
    (!obj.is_empty()).then_some(serde_json::Value::Object(obj))
}

fn parse_f64_json(s: &str) -> serde_json::Value {
    if let Ok(v) = s.trim().parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.trim().to_string())
}

fn parse_status(text: &str, log: &mut SolverLog) {
    // LP-only output:
    //   "Solving finished"
    //   "Status: Optimal  Objective: -5.0e+00  Iterations: 1  Time: 0.00s"
    if let Some(c) = regex::Regex::new(
        r"(?m)^Status:\s+(\S+)\s+Objective:\s+([-\d.eE+]+)(?:\s+Iterations:\s+(\d+))?(?:\s+Time:\s+([\d.]+))?",
    )
    .unwrap()
    .captures(text)
    {
        let s = &c[1];
        log.termination.raw_reason = Some(s.to_string());
        log.termination.status = match s {
            "Optimal" => Status::Optimal,
            "Infeasible" => Status::Infeasible,
            "Unbounded" => Status::Unbounded,
            _ => Status::Unknown,
        };
        if log.termination.status == Status::Optimal {
            let v: Option<f64> = c[2].parse().ok();
            log.bounds.primal = v;
            log.bounds.dual = v;
            log.bounds.gap = Some(0.0);
        }
        if let Some(iters) = c.get(3) {
            log.tree.simplex_iterations = iters.as_str().parse().ok();
        }
        if let Some(t) = c.get(4) {
            log.timing.wall_seconds = t.as_str().parse().ok();
        }
        return;
    }

    // Check "Solution status" first (more specific), then "MIP status"
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Solution status :") {
            let s = rest.trim();
            if s.starts_with("integer optimal") {
                log.termination.status = Status::Optimal;
                log.termination.raw_reason = Some(s.to_string());
                return;
            } else if s == "infeasible" {
                log.termination.status = Status::Infeasible;
                log.termination.raw_reason = Some(s.to_string());
                return;
            } else if s.starts_with("integer feasible") {
                // Feasible but not proven optimal — check MIP status for reason
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("MIP status") {
            let s = rest.trim_start_matches(':').trim();
            if log.termination.status != Status::Unknown {
                continue; // already set from Solution status
            }
            log.termination.raw_reason = Some(s.to_string());
            if s.contains("time limit") {
                log.termination.status = Status::TimeLimit;
            } else if s.contains("memory") {
                log.termination.status = Status::MemoryLimit;
            } else if s.contains("node limit") || s.contains("iteration limit") {
                log.termination.status = Status::OtherLimit;
            } else if s == "solved" {
                // "solved" + "infeasible" solution status already handled above
                // "solved" alone with integer feasible = Optimal
                log.termination.status = Status::Optimal;
            }
        }
    }
    // Second pass for MIP status if we only got "integer feasible"
    if log.termination.status == Status::Unknown {
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("MIP status") {
                let s = rest.trim_start_matches(':').trim();
                log.termination.raw_reason = Some(s.to_string());
                if s.contains("time limit") {
                    log.termination.status = Status::TimeLimit;
                } else if s.contains("memory") {
                    log.termination.status = Status::MemoryLimit;
                }
                return;
            }
        }
    }
}

fn parse_copt_bound(s: &str) -> Option<f64> {
    let t = s.trim();
    if t == "+inf" || t == "-inf" || t == "inf" || t == "--" {
        None
    } else {
        t.parse().ok()
    }
}

/// COPT B&B progress table. Header:
/// "     Nodes    Active  LPit/n  IntInf     BestBound  BestSolution     Gap   Time"
/// Row: "         0         1      --       0  1.510000e+02            --     Inf  0.67s"
/// Incumbent: "H        0         1      --       0  1.510000e+02  9.060000e+02  83.33%  0.71s"
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;

    for line in text.lines() {
        if !in_table {
            if line.contains("Nodes") && line.contains("BestBound") && line.contains("BestSolution")
            {
                in_table = true;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Repeated header
        if trimmed.starts_with("Nodes") && trimmed.contains("BestBound") {
            continue;
        }
        // End of table
        if trimmed.starts_with("Best solution")
            || trimmed.starts_with("Best bound")
            || trimmed.starts_with("Solve ")
            || trimmed.starts_with("MIP status")
            || trimmed.starts_with("Solution status")
            || trimmed.starts_with("Violations")
            || trimmed.starts_with("Writing")
            || trimmed.starts_with("Exiting")
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
    let first = line.chars().next()?;
    let (event, body) = if first.is_alphabetic() {
        (event_from_marker(first), &line[1..])
    } else {
        (None, line)
    };

    let toks: Vec<&str> = body.split_whitespace().collect();
    // Nodes Active LPit/n IntInf BestBound BestSolution Gap Time
    // = 8 tokens
    if toks.len() < 8 {
        return None;
    }

    let time = parse_time_token(toks[toks.len() - 1])?;
    // toks[2] = LPit/n, toks[3] = IntInf — skip
    Some(NodeSnapshot {
        time_seconds: time,
        event,
        nodes_explored: toks[0].replace(',', "").parse().ok(),
        dual: parse_or_dash_inf(toks[4]),
        primal: parse_or_dash_inf(toks[5]),
        gap: parse_gap(toks[6]),
        ..Default::default()
    })
}

fn parse_or_dash_inf(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "--"
        || t == "-"
        || t.is_empty()
        || t.eq_ignore_ascii_case("inf")
        || t == "+inf"
        || t == "-inf"
    {
        None
    } else {
        t.parse().ok()
    }
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Cardinal Optimizer\s+v(\d+\.\d+\.\d+)").unwrap())
}

fn re_reading() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Reading from '([^']+)'").unwrap())
}

fn re_original() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\s+and\s+([\d,]+)\s+non-zero")
            .unwrap()
    })
}

fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"The presolved problem has:\n\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\s+and\s+([\d,]+)\s+non-zero")
            .unwrap()
    })
}

fn re_best_sol() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Best solution\s*:\s*([-+\d.eE+inf]+)").unwrap())
}

fn re_best_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Best bound\s*:\s*([-+\d.eE+inf]+)").unwrap())
}

fn re_best_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Best gap\s*:\s*([\d.]+)%").unwrap())
}

fn re_solve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solve time\s*:\s*([\d.]+)").unwrap())
}

fn re_solve_node() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solve node\s*:\s*([\d,]+)").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_copt() {
        let p = CoptParser;
        assert!(p.sniff("Cardinal Optimizer v8.0.3. Build date Jan 13 2026"));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_copt_log() {
        let text = r#"Cardinal Optimizer v8.0.3. Build date Jan 13 2026
Copyright Cardinal Operations 2026. All Rights Reserved

Reading from '/home/beck/miplib2017/modified/p_30n20b8.mps.gz'

The original problem has:
    576 rows, 18380 columns and 109706 non-zero elements
    11036 binaries and 7344 integers

The presolved problem has:
    377 rows, 4113 columns and 34546 non-zero elements
    4065 binaries and 48 integers

     Nodes    Active  LPit/n  IntInf     BestBound  BestSolution     Gap   Time
         0         1      --       0  1.510000e+02            --     Inf  0.67s
H        0         1      --       0  1.510000e+02  9.060000e+02  83.33%  0.71s
         1         1   14858     144  3.020000e+02  3.020000e+02  0.000%  2.82s

Best solution   : 302.000000000
Best bound      : 302.000000000
Best gap        : 0.0000%
Solve time      : 2.83
Solve node      : 1
MIP status      : solved
Solution status : integer optimal (relative gap limit 0)
"#;
        let log = CoptParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Copt);
        assert_eq!(log.version.as_deref(), Some("8.0.3"));
        assert_eq!(log.problem.as_deref(), Some("p_30n20b8"));
        assert_eq!(log.termination.status, Status::Optimal);
        assert!((log.bounds.primal.unwrap() - 302.0).abs() < 0.01);
        assert!((log.bounds.dual.unwrap() - 302.0).abs() < 0.01);
        assert!((log.timing.wall_seconds.unwrap() - 2.83).abs() < 0.01);
        assert_eq!(log.tree.nodes_explored, Some(1));
        assert_eq!(log.presolve.rows_before, Some(576));
        assert_eq!(log.presolve.cols_before, Some(18380));
        assert_eq!(log.presolve.rows_after, Some(377));
        assert_eq!(log.progress.len(), 3);
    }

    #[test]
    fn parse_copt_infeasible() {
        let text = r#"Cardinal Optimizer v8.0.3.
Reading from 'test.mps'

The original problem has:
    10 rows, 20 columns and 50 non-zero elements

Best solution   : +inf
Best bound      : +inf
Best gap        : 0.0000%
Solve time      : 7.24
Solve node      : 63
MIP status      : solved
Solution status : infeasible
"#;
        let log = CoptParser.parse(text).unwrap();
        assert_eq!(log.termination.status, Status::Infeasible);
        assert!(log.bounds.primal.is_none());
    }

    #[test]
    fn parse_copt_timelimit() {
        let text = r#"Cardinal Optimizer v8.0.3.

Best solution   : 212.000000000
Best bound      : 206.355726603
Best gap        : 2.6624%
Solve time      : 7200.07
Solve node      : 56948817
MIP status      : stopped (time limit reached)
Solution status : integer feasible
"#;
        let log = CoptParser.parse(text).unwrap();
        assert_eq!(log.termination.status, Status::TimeLimit);
        assert!((log.bounds.primal.unwrap() - 212.0).abs() < 0.01);
    }
}
