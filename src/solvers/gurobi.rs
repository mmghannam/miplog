//! Gurobi log parser. Tested against Gurobi 11 and 13 output.

use crate::solvers::progress::{event_from_marker, parse_gap, parse_or_dash, parse_time_token};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct GurobiParser;

impl LogParser for GurobiParser {
    fn solver(&self) -> Solver {
        Solver::Gurobi
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("Gurobi Optimizer") || text.contains("gurobi_cl")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("gurobi"));
        }
        let mut log = SolverLog::new(Solver::Gurobi);

        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }

        // Status
        if text.contains("Optimal solution found") {
            log.termination.status = Status::Optimal;
        } else if text.contains("Model is infeasible and unbounded")
            || text.contains("Model is infeasible or unbounded")
        {
            log.termination.status = Status::InfeasibleOrUnbounded;
        } else if text.contains("Model is infeasible") {
            log.termination.status = Status::Infeasible;
        } else if text.contains("Model is unbounded") {
            log.termination.status = Status::Unbounded;
        } else if text.contains("Time limit reached") {
            log.termination.status = Status::TimeLimit;
            log.termination.raw_reason = Some("Time limit reached".into());
        } else if text.contains("Out of memory") {
            log.termination.status = Status::MemoryLimit;
        }

        // Time + nodes: "Explored N nodes (M simplex iterations) in T seconds"
        if let Some(c) = re_explored().captures(text) {
            log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
            log.tree.simplex_iterations = c[2].replace(',', "").parse().ok();
            log.timing.wall_seconds = c[3].parse().ok();
        }

        // Bounds: "Best objective X, best bound Y, gap Z%"
        if let Some(c) = re_best().captures(text) {
            log.bounds.primal = parse_opt_f64(&c[1]);
            log.bounds.dual = parse_opt_f64(&c[2]);
            log.bounds.gap = parse_opt_f64(&c[3]).map(|p| p / 100.0);
        }

        // Solutions found: "Solution count N:"
        if let Some(c) = re_solcount().captures(text) {
            log.tree.solutions_found = c[1].parse().ok();
        }

        log.progress = parse_progress(text);

        // Pre-presolve dims: "Optimize a model with R rows, C columns and N nonzeros"
        if let Some(c) = re_original().captures(text) {
            log.presolve.rows_before = c[1].replace(',', "").parse().ok();
            log.presolve.cols_before = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_before = c[3].replace(',', "").parse().ok();
        }

        // Presolve dims: "Presolved: R rows, C columns, N nonzeros"
        if let Some(c) = re_presolved().captures(text) {
            log.presolve.rows_after = c[1].replace(',', "").parse().ok();
            log.presolve.cols_after = c[2].replace(',', "").parse().ok();
            log.presolve.nonzeros_after = c[3].replace(',', "").parse().ok();
        }

        Ok(log)
    }
}

fn re_original() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Optimize a model with ([\d,]+) rows?, ([\d,]+) columns? and ([\d,]+) nonzeros")
            .unwrap()
    })
}

/// Parse the Gurobi B&B progress table. Two row shapes:
///
/// 1. Standard (10 fields):
///    `   Expl  Unexpl  Obj       Depth  IntInf  Incumbent  BestBd  Gap   It/Node  Time`
/// 2. Incumbent update (8 fields with a leading marker):
///    `M  Expl  Unexpl                            Incumbent  BestBd  Gap   It/Node  Time`
///    where M ∈ {H, *, h}. Obj/Depth/IntInf are blank on these lines.
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    let mut in_table = false;
    for line in text.lines() {
        // Table starts after the header. Both variants contain "Incumbent" and "BestBd".
        if !in_table {
            if line.contains("Incumbent") && line.contains("BestBd") {
                in_table = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            // A blank line may separate the header from the first row, or end the table.
            if !out.is_empty() {
                break;
            }
            continue;
        }
        if let Some(row) = parse_row(line) {
            out.push(row);
        } else if line.starts_with("Cutting planes:")
            || line.starts_with("Cutting Planes:")
            || line.starts_with("Explored ")
            || line.contains("Time limit reached")
            || line.starts_with("Optimal solution")
        {
            break;
        }
    }
    out
}

fn parse_row(line: &str) -> Option<NodeSnapshot> {
    let marker = line.chars().next()?;
    let (event, body) = if matches!(marker, 'H' | '*' | 'h') {
        (event_from_marker(marker), &line[1..])
    } else {
        (None, line)
    };
    let toks: Vec<&str> = body.split_whitespace().collect();
    // Minimum column count: 5 for incumbent rows (Expl Unexpl Incumbent BestBd Gap [ItN] Time)
    // but typical shapes are 7 or 10.
    let mut snap = NodeSnapshot::default();
    match toks.len() {
        // Standard row: Expl Unexpl Obj Depth IntInf Incumbent BestBd Gap It/Node Time
        10 => {
            snap.nodes_explored = toks[0].parse().ok();
            // toks[2] Obj, toks[3] Depth, toks[4] IntInf — skip Obj (per-node LP obj)
            snap.depth = toks[3].parse().ok();
            snap.primal = parse_or_dash(toks[5]);
            snap.dual = parse_or_dash(toks[6]);
            snap.gap = parse_gap(toks[7]);
            snap.lp_iterations = toks[8].parse().ok();
            snap.time_seconds = parse_time_token(toks[9])?;
        }
        // Incumbent update: Expl Unexpl Incumbent BestBd Gap It/Node Time
        7 => {
            snap.nodes_explored = toks[0].parse().ok();
            snap.primal = parse_or_dash(toks[2]);
            snap.dual = parse_or_dash(toks[3]);
            snap.gap = parse_gap(toks[4]);
            snap.lp_iterations = toks[5].parse().ok();
            snap.time_seconds = parse_time_token(toks[6])?;
        }
        _ => return None,
    }
    snap.event = event;
    // Sanity — if neither nodes nor time parsed, this isn't a progress row.
    if snap.nodes_explored.is_none() {
        return None;
    }
    Some(snap)
}

fn parse_opt_f64(s: &str) -> Option<f64> {
    if s == "-" {
        None
    } else {
        s.parse().ok()
    }
}

fn re_version() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Gurobi(?:\s+Optimizer)?\s+(?:version\s+)?(\d+\.\d+(?:\.\d+)?)").unwrap()
    })
}
fn re_explored() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Explored\s+([\d,]+)\s+nodes?\s+\(([\d,]+)\s+simplex iterations\)\s+in\s+([\d.]+)\s+seconds").unwrap()
    })
}
fn re_best() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"Best objective\s+(-|[\d.eE+\-]+)\s*,\s*best bound\s+(-|[\d.eE+\-]+)\s*,\s*gap\s+(-|[\d.]+)%?",
        )
        .unwrap()
    })
}
fn re_solcount() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solution count\s+(\d+)").unwrap())
}
fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Presolved:\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns,\s+([\d,]+)\s+nonzeros")
            .unwrap()
    })
}
