//! Mosek log parser. Tested against Mosek 11.0 output.

use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct MosekParser;

impl LogParser for MosekParser {
    fn solver(&self) -> Solver {
        Solver::Mosek
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("MOSEK Version") || text.contains("mosek.com")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("mosek"));
        }
        let mut log = SolverLog::new(Solver::Mosek);

        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        if let Some(c) = re_name().captures(text) {
            let name = c[1].trim();
            let name = name
                .strip_suffix(".mps.gz")
                .or_else(|| name.strip_suffix(".mps"))
                .unwrap_or(name);
            log.problem = Some(name.to_string());
        }

        if let Some(c) = re_constraints().captures(text) {
            log.presolve.rows_before = c[1].trim().replace(',', "").parse().ok();
        }
        if let Some(c) = re_variables().captures(text) {
            log.presolve.cols_before = c[1].trim().replace(',', "").parse().ok();
        }

        // Solution summary via regex — prefer Basic over Interior-point
        parse_solution_summary(text, &mut log);

        if let Some(c) = re_optimizer_time().captures(text) {
            log.timing.wall_seconds = c[1].trim().parse().ok();
        }

        if let Some(c) = re_ipm_iters().captures(text) {
            log.tree.simplex_iterations = c[1].trim().replace(',', "").parse().ok();
        }
        if let Some(c) = re_simplex_iters().captures(text) {
            let val: u64 = c[1].trim().replace(',', "").parse().unwrap_or(0);
            if val > 0 {
                log.tree.simplex_iterations = Some(log.tree.simplex_iterations.unwrap_or(0) + val);
            }
        }

        if let Some(c) = re_return_code().captures(text) {
            if log.termination.raw_reason.is_none() {
                log.termination.raw_reason = Some(format!("{} [{}]", &c[1], &c[2]));
            }
        }

        Ok(log)
    }
}

fn parse_solution_summary(text: &str, log: &mut SolverLog) {
    // Use regex to extract solution status and objectives from summary blocks.
    // Prefer "Basic solution summary" over "Interior-point solution summary".
    let re = re_summary_block();

    let mut best_match: Option<(String, String, f64, f64)> = None;
    let mut found_basic = false;

    for c in re.captures_iter(text) {
        let kind = &c[1]; // "Basic" or "Interior-point"
        let sol_status = c[2].to_string();
        let primal: f64 = c[3].parse().unwrap_or(0.0);
        let dual: f64 = c[4].parse().unwrap_or(0.0);

        if kind == "Basic" {
            best_match = Some((sol_status, kind.to_string(), primal, dual));
            found_basic = true;
        } else if !found_basic {
            best_match = Some((sol_status, kind.to_string(), primal, dual));
        }
    }

    if let Some((sol_status, _, primal, dual)) = best_match {
        log.termination.raw_reason = Some(sol_status.clone());
        match sol_status.as_str() {
            "OPTIMAL" => log.termination.status = Status::Optimal,
            "UNKNOWN" | "UNDEFINED" => log.termination.status = Status::Unknown,
            s if s.contains("INFEASIBLE") => {
                log.termination.status = Status::Infeasible;
            }
            _ => {}
        }
        log.bounds.primal = Some(primal);
        log.bounds.dual = Some(dual);
    }
}

fn re_summary_block() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"(Basic|Interior-point) solution summary\n",
            r"\s+Problem status\s*:\s*\S+\n",
            r"\s+Solution status\s*:\s*(\S+)\n",
            r"\s+Primal\.\s+obj:\s*([-\d.eE+]+)",
            r".*\n",
            r"\s+Dual\.\s+obj:\s*([-\d.eE+]+)",
        ))
        .unwrap()
    })
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"MOSEK Version\s+(\d+\.\d+\.\d+)").unwrap())
}

fn re_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Name\s+:\s+(\S+)").unwrap())
}

fn re_constraints() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Constraints\s+:\s+([\d,]+)").unwrap())
}

fn re_variables() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Scalar variables\s+:\s+([\d,]+)").unwrap())
}

fn re_optimizer_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s+Optimizer\s+-\s+time:\s*([\d.]+)").unwrap())
}

fn re_ipm_iters() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Interior-point\s+-\s+iterations\s*:\s*([\d,]+)").unwrap())
}

fn re_simplex_iters() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Simplex\s+-\s+iterations\s*:\s*([\d,]+)").unwrap())
}

fn re_return_code() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Return code\s+-\s+(\d+)\s+\[(\w+)\]").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_mosek() {
        let p = MosekParser;
        assert!(p.sniff("MOSEK Version 11.0.13 (Build date: 2025-3-17)"));
        assert!(!p.sniff("Gurobi Optimizer version 11"));
    }

    #[test]
    fn parse_mosek_lp() {
        let text = "
MOSEK Version 11.0.13 (Build date: 2025-3-17 10:00:42)
Copyright (c) MOSEK ApS, Denmark WWW: mosek.com
Platform: Linux/64-X86

Problem
  Name                   : a2864-99blp.mps.gz
  Objective sense        : minimize
  Type                   : LO (linear optimization problem)
  Constraints            : 22117
  Scalar variables       : 200787

Optimizer terminated. Time: 1307.17

Interior-point solution summary
  Problem status  : PRIMAL_AND_DUAL_FEASIBLE
  Solution status : OPTIMAL
  Primal.  obj: -2.8296047432e+02   nrm: 2e+01    Viol.  con: 6e-10    var: 0e+00
  Dual.    obj: -2.8296047432e+02   nrm: 2e+01    Viol.  con: 0e+00    var: 5e-11

Basic solution summary
  Problem status  : PRIMAL_AND_DUAL_FEASIBLE
  Solution status : OPTIMAL
  Primal.  obj: -2.8296047431e+02   nrm: 2e+01    Viol.  con: 2e-11    var: 0e+00
  Dual.    obj: -2.8296047431e+02   nrm: 2e+01    Viol.  con: 7e-15    var: 8e-15

Optimizer summary
  Optimizer                 -                        time: 1307.17
    Interior-point          - iterations : 11        time: 1307.11
    Simplex                 - iterations : 0         time: 0.00
    Mixed integer           - relaxations: 0         time: 0.00

Return code - 0  [MSK_RES_OK]
";
        let log = MosekParser.parse(text).unwrap();
        assert_eq!(log.solver, Solver::Mosek);
        assert_eq!(log.version.as_deref(), Some("11.0.13"));
        assert_eq!(log.problem.as_deref(), Some("a2864-99blp"));
        assert_eq!(log.termination.status, Status::Optimal);
        assert!((log.bounds.primal.unwrap() - (-282.96047431)).abs() < 0.001);
        assert!((log.bounds.dual.unwrap() - (-282.96047431)).abs() < 0.001);
        assert!((log.timing.wall_seconds.unwrap() - 1307.17).abs() < 0.01);
        assert_eq!(log.tree.simplex_iterations, Some(11));
        assert_eq!(log.presolve.rows_before, Some(22117));
        assert_eq!(log.presolve.cols_before, Some(200787));
    }

    #[test]
    fn parse_mosek_unknown() {
        let text = "
MOSEK Version 11.0.13

Problem
  Name                   : test
  Constraints            : 100
  Scalar variables       : 200

Interior-point solution summary
  Problem status  : UNKNOWN
  Solution status : UNKNOWN
  Primal.  obj: 1.3973568392e+16    nrm: 4e+10    Viol.  con: 6e+02    var: 1e+04
  Dual.    obj: -5.6480379543e+14   nrm: 4e+08    Viol.  con: 0e+00    var: 9e+04

Optimizer summary
  Optimizer                 -                        time: 15485.75
    Interior-point          - iterations : 25        time: 15484.56

Return code - 100006  [MSK_RES_TRM_STALL]
";
        let log = MosekParser.parse(text).unwrap();
        assert_eq!(log.termination.status, Status::Unknown);
        assert!((log.timing.wall_seconds.unwrap() - 15485.75).abs() < 0.01);
    }
}
