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

        Ok(log)
    }
}

fn parse_status(text: &str, log: &mut SolverLog) {
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
    let mut snap = NodeSnapshot::default();
    snap.time_seconds = time;
    snap.event = event;
    snap.nodes_explored = toks[0].replace(',', "").parse().ok();
    // toks[2] = LPit/n, toks[3] = IntInf — skip
    snap.dual = parse_or_dash_inf(toks[4]);
    snap.primal = parse_or_dash_inf(toks[5]);
    snap.gap = parse_gap(toks[6]);

    Some(snap)
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
