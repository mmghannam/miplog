//! OptVerse (Huawei) log parser. Tested against OptVerse 2.0.1 output.

use crate::solvers::progress::{event_from_marker, parse_gap, parse_time_token};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct OptverseParser;

impl LogParser for OptverseParser {
    fn solver(&self) -> Solver {
        Solver::Optverse
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("OptVerse Optimizer") || text.contains("Optverse license")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("optverse"));
        }
        let mut log = SolverLog::new(Solver::Optverse);

        // Version: "OptVerse Optimizer version 2.0.1"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        // Problem: "Read problem /home/.../p_30n20b8.mps.gz"
        if let Some(c) = re_read_problem().captures(text) {
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

        // Original dims: "  576 rows, 18380 columns (11036 binary, 7344 integer, 0 continuous) and 109706 nonzeros"
        if let Some(c) = re_original().captures(text) {
            log.presolve.rows_before = c[1].replace(',', "").parse().ok();
            log.presolve.cols_before = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_before = c[3].replace(',', "").parse().ok();
        }

        // Presolved: "After presolve:\n  463 rows, 4613 columns (...) and 41349 nonzeros"
        if let Some(c) = re_presolved().captures(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        // Presolve time
        if let Some(c) = re_presolve_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }

        // Solve results section
        parse_status(text, &mut log);

        // Bounds
        if let Some(c) = re_best_sol().captures(text) {
            log.bounds.primal = parse_optverse_num(&c[1]);
        }
        if let Some(c) = re_best_bound().captures(text) {
            log.bounds.dual = parse_optverse_num(&c[1]);
        }
        if let Some(c) = re_gap().captures(text) {
            log.bounds.gap = c[1].parse::<f64>().ok().map(|v| v / 100.0);
        }

        // Node / LP iteration / Time
        if let Some(c) = re_node().captures(text) {
            log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
        }
        if let Some(c) = re_lp_iter().captures(text) {
            log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
        }
        if let Some(c) = re_time().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }

        // Progress table
        log.progress = parse_progress(text);

        Ok(log)
    }
}

fn parse_status(text: &str, log: &mut SolverLog) {
    // "  Status               Optimal solution found"
    // "  Status               Time limit reached"
    // "  Status               Problem is infeasible"
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Status") {
            let s = rest.trim();
            log.termination.raw_reason = Some(s.to_string());
            if s.contains("Optimal") {
                log.termination.status = Status::Optimal;
            } else if s.contains("infeasible") || s.contains("Infeasible") {
                log.termination.status = Status::Infeasible;
            } else if s.contains("unbounded") || s.contains("Unbounded") {
                log.termination.status = Status::Unbounded;
            } else if s.contains("Time limit") || s.contains("time limit") {
                log.termination.status = Status::TimeLimit;
            } else if s.contains("Memory") || s.contains("memory") {
                log.termination.status = Status::MemoryLimit;
            } else if s.contains("Node limit") || s.contains("node limit") {
                log.termination.status = Status::OtherLimit;
            }
            return;
        }
    }
}

fn parse_optverse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t == "--" || t == "-" || t.is_empty() || t.eq_ignore_ascii_case("inf") {
        None
    } else {
        t.parse().ok()
    }
}

/// OptVerse B&B progress table. Header:
/// "    Time    Solved      Open    It/Node    BestBound       BestSol       Gap"
/// Row: "     0.5s         0          0       --   0.000000e+00        --          --"
/// Incumbent: " H   1.3s         0          0       --   1.235086e+02   3.530000e+02   65.01%"
/// Also: " *   9.0s       100         15      112   1.252192e+02   3.020000e+02   58.54%"
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;

    for line in text.lines() {
        if !in_table {
            if line.contains("Time") && line.contains("Solved") && line.contains("BestBound") {
                in_table = true;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Repeated header
        if trimmed.starts_with("Time") && trimmed.contains("BestBound") {
            continue;
        }
        // End of table
        if trimmed.starts_with("Solve results")
            || trimmed.starts_with("Write best")
            || trimmed.starts_with("Status")
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
    // First 2 chars may be a marker: " H", " *", etc.
    let marker_part = line.get(..2)?;
    let marker_char = marker_part.trim();
    let event = if marker_char.is_empty() {
        None
    } else if marker_char.len() == 1 {
        event_from_marker(marker_char.chars().next()?)
    } else {
        None
    };

    let toks: Vec<&str> = line.split_whitespace().collect();
    // With marker: Marker Time Solved Open It/Node BestBound BestSol Gap = 8 tokens
    // Without: Time Solved Open It/Node BestBound BestSol Gap = 7 tokens
    if toks.len() < 7 {
        return None;
    }

    // Find time token (ends with 's')
    let (offset, time) = if toks[0].ends_with('s') {
        (0, parse_time_token(toks[0])?)
    } else if toks.len() >= 8 && toks[1].ends_with('s') {
        (1, parse_time_token(toks[1])?)
    } else {
        return None;
    };

    if toks.len() < 7 + offset {
        return None;
    }

    // toks[offset+3] = It/Node — skip.
    Some(NodeSnapshot {
        time_seconds: time,
        event,
        nodes_explored: toks[offset + 1].replace(',', "").parse().ok(),
        dual: parse_or_dash_inf(toks[offset + 4]),
        primal: parse_or_dash_inf(toks[offset + 5]),
        gap: parse_gap(toks[offset + 6]),
        ..Default::default()
    })
}

fn parse_or_dash_inf(tok: &str) -> Option<f64> {
    let t = tok.trim();
    if t == "--" || t == "-" || t.is_empty() || t.eq_ignore_ascii_case("inf") {
        None
    } else {
        t.parse().ok()
    }
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"OptVerse Optimizer version\s+(\d+\.\d+\.\d+)").unwrap())
}

fn re_read_problem() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Read problem\s+(\S+)").unwrap())
}

fn re_original() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\b.*?\band\s+([\d,]+)\s+nonzeros")
            .unwrap()
    })
}

fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"After presolve:\n\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns\b.*?\band\s+([\d,]+)\s+nonzeros")
            .unwrap()
    })
}

fn re_presolve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolve time:\s*([\d.]+)s").unwrap())
}

fn re_best_sol() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Best solution\s+([-+\d.eE]+)").unwrap())
}

fn re_best_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Best bound\s+([-+\d.eE]+)").unwrap())
}

fn re_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Gap\s+([\d.]+)%").unwrap())
}

fn re_node() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Node\s+([\d,]+)").unwrap())
}

fn re_lp_iter() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+LP iteration\s+([\d,]+)").unwrap())
}

fn re_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Time\s+([\d.]+)").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_optverse() {
        let p = OptverseParser;
        assert!(p.sniff("OptVerse Optimizer version 2.0.1"));
        assert!(p.sniff("Optverse license - expires"));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_optverse_log() {
        let text = r#"Optverse license - expires in 2026-07-23
OptVerse Optimizer version 2.0.1
Copyright (c) Huawei Technologies Co., Ltd. 2022-2025. All rights reserved.

Read problem /home/beck/miplib2017/modified/p_30n20b8.mps.gz
Read time: 0.04s

Optimize a(n) MILP model
  576 rows, 18380 columns (11036 binary, 7344 integer, 0 continuous) and 109706 nonzeros
  Model fingerprint: 0x8a10608d7e44d8ce

Presolve problem
Presolve time: 0.53s
After presolve:
  463 rows, 4613 columns (4551 binary, 62 integer, 0 continuous) and 41349 nonzeros

Start parallel solving, using up to 12 threads

    Time    Solved      Open    It/Node    BestBound       BestSol       Gap  
     0.5s         0          0       --   0.000000e+00        --          --  
 H   1.3s         0          0       --   1.235086e+02   3.530000e+02   65.01%
 *   9.0s       100         15      112   1.252192e+02   3.020000e+02   58.54%
    53.0s     17374          0      129   3.020000e+02   3.020000e+02    0.00%

Solve results
  Status               Optimal solution found
  Best solution        3.020000000000e+02
  Best bound           3.020000000000e+02
  Gap                  0.0000%
  Node                 17374
  LP iteration         2250377
  Time                 52.98
"#;
        let log = OptverseParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Optverse);
        assert_eq!(log.version.as_deref(), Some("2.0.1"));
        assert_eq!(log.problem.as_deref(), Some("p_30n20b8"));
        assert_eq!(log.termination.status, Status::Optimal);
        assert!((log.bounds.primal.unwrap() - 302.0).abs() < 0.01);
        assert!((log.bounds.dual.unwrap() - 302.0).abs() < 0.01);
        assert!((log.timing.wall_seconds.unwrap() - 52.98).abs() < 0.01);
        assert_eq!(log.tree.nodes_explored, Some(17374));
        assert_eq!(log.tree.simplex_iterations, Some(2250377));
        assert_eq!(log.presolve.rows_before, Some(576));
        assert_eq!(log.presolve.rows_after, Some(463));
        assert!((log.timing.presolve_seconds.unwrap() - 0.53).abs() < 0.01);
        assert_eq!(log.progress.len(), 4);
        // Check event parsing
        let rows: Vec<_> = log.progress.iter().collect();
        assert!(rows[1].event.is_some()); // H
        assert!(rows[2].event.is_some()); // *
    }
}
