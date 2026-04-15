//! SCIP log parser (tested against SCIP 10 output, both SoPlex and CPLEX LP
//! solvers). Handles the Mittelmann-style concatenated wrapper transparently —
//! pass one per-instance slice (see [`crate::input::split_concatenated`]) or a
//! standalone SCIP-session log.

use crate::solvers::progress::{parse_gap, parse_or_dash};
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

        // Version line: "SCIP version 10.0.0 [precision: 8 byte] ... [GitHash: 0c80fdd8e9]"
        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }
        if let Some(c) = re_githash().captures(text) {
            log.solver_git_hash = Some(c[1].to_string());
        }
        // Problem name from "read problem <path>" or "read problem </path/to/X.mps.gz>"
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

        // Status: "SCIP Status : problem is solved [optimal solution found]" etc.
        // The bracketed reason is the authoritative piece.
        if let Some(c) = re_status().captures(text) {
            let reason = c[1].to_string();
            log.termination.status = classify_status(&reason);
            log.termination.raw_reason = Some(reason);
        }

        // Solving + presolving times.
        if let Some(c) = re_solving_time().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }
        if let Some(c) = re_presolving_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }

        // Primal / dual bounds + gap.
        if let Some(c) = re_primal().captures(text) {
            log.bounds.primal = parse_or_dash(&c[1]);
            log.tree.solutions_found = c.get(2).and_then(|m| m.as_str().parse().ok());
        }
        if let Some(c) = re_dual().captures(text) {
            log.bounds.dual = parse_or_dash(&c[1]);
        }
        if let Some(c) = re_gap().captures(text) {
            log.bounds.gap = parse_gap(&c[1]);
        }

        // Nodes.
        if let Some(c) = re_nodes().captures(text) {
            log.tree.nodes_explored = c[1].parse().ok();
        }

        // Presolve dims. SCIP emits both "original problem has" and
        // "presolved problem has N variables ... and M constraints". No
        // nonzero count is reported in the summary form.
        if let Some(c) = re_orig_dims().captures(text) {
            log.presolve.cols_before = c[1].parse().ok();
            log.presolve.rows_before = c[2].parse().ok();
        }
        if let Some(c) = re_presolved_dims().captures(text) {
            log.presolve.cols_after = c[1].parse().ok();
            log.presolve.rows_after = c[2].parse().ok();
        }

        Ok(log)
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

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"SCIP version (\d+\.\d+(?:\.\d+)?)").unwrap())
}
fn re_githash() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"SCIP version[^\n]*\[GitHash:\s*([0-9a-f]+)\]").unwrap())
}
fn re_problem() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"read problem <([^>]+)>").unwrap())
}
fn re_status() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // "SCIP Status        : problem is solved [optimal solution found]"
    // "SCIP Status        : solving was interrupted [time limit reached]"
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
fn re_primal() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // "Primal Bound       : +3.02000000000000e+02 (4 solutions)"
    R.get_or_init(|| {
        Regex::new(r"Primal Bound\s*:\s*(\+?-?[\d.eE+\-]+|-)\s*(?:\((\d+) solutions\))?").unwrap()
    })
}
fn re_dual() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Prefer "Final Dual Bound" when present (summary form); fall back to
    // plain "Dual Bound" otherwise. We only capture once so use alternation.
    R.get_or_init(|| Regex::new(r"(?:Final )?Dual Bound\s*:\s*(\+?-?[\d.eE+\-]+|-)").unwrap())
}
fn re_gap() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Gap\s*:\s*([\d.]+ ?%|infinite|-)").unwrap())
}
fn re_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"B&B Tree.*?nodes\s*\(total\)\s*:\s*(\d+)").unwrap())
}
fn re_orig_dims() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // "original problem has 18380 variables (...) and 576 constraints"
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
