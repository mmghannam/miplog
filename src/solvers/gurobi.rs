//! Gurobi log parser. Tested against Gurobi 11–13 output.

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
        if text.contains("Optimal solution found")
            // LP-only completion: Gurobi prints "Optimal objective X" without
            // the MIP "Optimal solution found" phrase.
            || (text.contains("Optimal objective") && !text.contains("Solution count"))
        {
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
        } else if text.contains("Node limit reached")
            || text.contains("Solution limit reached")
            || text.contains("Iteration limit reached")
            || text.contains("Work limit reached")
        {
            log.termination.status = Status::OtherLimit;
            log.termination.raw_reason = Some(
                ["Node", "Solution", "Iteration", "Work"]
                    .iter()
                    .find(|w| text.contains(&format!("{w} limit reached")))
                    .map(|w| format!("{w} limit reached"))
                    .unwrap_or_else(|| "limit reached".into()),
            );
        }

        // Time + nodes: "Explored N nodes (M simplex iterations) in T seconds"
        if let Some(c) = re_explored().captures(text) {
            log.tree.nodes_explored = c[1].replace(',', "").parse().ok();
            log.tree.simplex_iterations = c[2].replace(',', "").parse().ok();
            log.timing.wall_seconds = c[3].parse().ok();
        }
        // LP-only fallback: "Solved in 1 iterations and 0.00 seconds"
        if log.timing.wall_seconds.is_none() {
            if let Some(c) = re_lp_solved().captures(text) {
                log.tree.simplex_iterations = c[1].replace(',', "").parse().ok();
                log.timing.wall_seconds = c[2].parse().ok();
            }
        }

        // Bounds: "Best objective X, best bound Y, gap Z%" (MIP)
        if let Some(c) = re_best().captures(text) {
            log.bounds.primal = parse_opt_f64(&c[1]);
            log.bounds.dual = parse_opt_f64(&c[2]);
            log.bounds.gap = parse_opt_f64(&c[3]).map(|p| p / 100.0);
        } else if let Some(c) = re_lp_optimal_obj().captures(text) {
            // LP-only: "Optimal objective -5.000000000e+00" — no dual line,
            // but LP optimality is duality-tight so mirror into both.
            let v: Option<f64> = c[1].parse().ok();
            log.bounds.primal = v;
            log.bounds.dual = v;
            log.bounds.gap = Some(0.0);
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

        // Presolve time: "Presolve time: 0.00s"
        if let Some(c) = re_presolve_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }

        // Root relaxation: "Root relaxation: objective 7.155000e+03, 114 iterations, 0.00 seconds"
        if let Some(c) = re_root_relaxation().captures(text) {
            log.bounds.root_dual = c[1].parse().ok();
            log.timing.root_relaxation_seconds = c[2].parse().ok();
        }

        // First heuristic solution before B&B: "Found heuristic solution: objective X"
        if let Some(c) = re_first_heuristic().captures(text) {
            log.bounds.first_primal = c[1].parse().ok();
            // Gurobi doesn't stamp these with a time; leave `first_primal_time_seconds` None.
        }

        // Max depth reached: take max over progress Depth column (Gurobi
        // doesn't report this separately).
        let max_depth = log.progress.depth.iter().filter_map(|d| *d).max();
        log.tree.max_depth = max_depth;

        // Cuts: "Cutting planes: Gomory 9, Cover 2, MIR 24, ..."
        log.cuts = parse_cuts(text);

        // Rich solver-specific data.
        populate_other_data(text, &mut log);

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
fn re_lp_solved() -> &'static Regex {
    // LP-only termination summary:
    // "Solved in 1 iterations and 0.00 seconds (0.00 work units)"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Solved in\s+([\d,]+)\s+iterations? and\s+([\d.]+)\s+seconds").unwrap()
    })
}
fn re_lp_optimal_obj() -> &'static Regex {
    // LP-only completion. Gurobi prints "Optimal objective <value>"
    // (no comma, no "best bound") instead of the MIP "Best objective ..." line.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Optimal objective\s+([-\d.eE+]+)").unwrap())
}
fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Presolved:\s+([\d,]+)\s+rows,\s+([\d,]+)\s+columns,\s+([\d,]+)\s+nonzeros")
            .unwrap()
    })
}

fn re_presolve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolve time:\s+([\d.]+)s").unwrap())
}

fn re_root_relaxation() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"Root relaxation:\s+objective\s+([-\d.eE+]+),\s+[\d,]+\s+iterations,\s+([\d.]+)\s+seconds",
        )
        .unwrap()
    })
}

fn re_first_heuristic() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Found heuristic solution:\s+objective\s+([-\d.eE+]+)").unwrap())
}

/// Parse "Cutting planes:" block into a family → count map.
/// Block shape:
///   Cutting planes:
///     Gomory: 9
///     Cover: 2
///     MIR: 24
fn parse_cuts(text: &str) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    let hdr_re = Regex::new(r"(?m)^Cutting [Pp]lanes:").unwrap();
    let Some(m) = hdr_re.find(text) else {
        return out;
    };
    let row_re = Regex::new(r"^\s+([A-Za-z][A-Za-z0-9 \-]*?):\s+(\d+)").unwrap();
    for line in text[m.end()..].lines().skip(1) {
        if line.trim().is_empty() {
            break;
        }
        let c0 = line.chars().next().unwrap_or(' ');
        if !c0.is_whitespace() {
            break;
        }
        if let Some(c) = row_re.captures(line) {
            let name = c[1].trim().to_string();
            if let Ok(n) = c[2].parse::<u64>() {
                if n > 0 {
                    out.insert(name, n);
                }
            }
        }
    }
    out
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_coefficient_ranges(text) {
        log.other_data
            .push(NamedValue::new("gurobi.coefficient_ranges", v));
    }
    let (before, after) = parse_variable_types(text);
    if let Some(v) = before {
        log.other_data
            .push(NamedValue::new("gurobi.variable_types_before_presolve", v));
    }
    if let Some(v) = after {
        log.other_data
            .push(NamedValue::new("gurobi.variable_types_after_presolve", v));
    }
    if let Some(v) = parse_cpu_info(text) {
        log.other_data.push(NamedValue::new("gurobi.machine", v));
    }
    if let Some(v) = parse_heuristic_solutions(text) {
        log.other_data
            .push(NamedValue::new("gurobi.pre_bb_heuristic_solutions", v));
    }
    if let Some(v) = parse_solution_pool(text) {
        log.other_data
            .push(NamedValue::new("gurobi.solution_pool", v));
    }
    if let Some(c) = re_fingerprint().captures(text) {
        log.other_data.push(NamedValue::new(
            "gurobi.model_fingerprint",
            serde_json::Value::String(c[1].to_string()),
        ));
    }
    if let Some(c) = re_optimal_tolerance().captures(text) {
        if let Ok(v) = c[1].parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(v) {
                log.other_data.push(NamedValue::new(
                    "gurobi.optimality_tolerance",
                    serde_json::Value::Number(n),
                ));
            }
        }
    }
    if let Some(c) = re_work_units().captures(text) {
        if let Ok(v) = c[1].parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(v) {
                log.other_data.push(NamedValue::new(
                    "gurobi.work_units",
                    serde_json::Value::Number(n),
                ));
            }
        }
    }
}

/// "Coefficient statistics:" block with 4 named ranges.
fn parse_coefficient_ranges(text: &str) -> Option<serde_json::Value> {
    let hdr = Regex::new(r"(?m)^Coefficient statistics:").unwrap();
    let m = hdr.find(text)?;
    let row =
        Regex::new(r"^\s+(Matrix|Objective|Bounds|RHS) range\s+\[([^,]+),\s*([^\]]+)\]").unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(8) {
        if line.trim().is_empty() {
            break;
        }
        if !line.starts_with("  ") {
            break;
        }
        if let Some(c) = row.captures(line) {
            let name = c[1].to_lowercase();
            let mut inner = serde_json::Map::new();
            inner.insert("min".into(), parse_f64_json(c[2].trim()));
            inner.insert("max".into(), parse_f64_json(c[3].trim()));
            obj.insert(name, serde_json::Value::Object(inner));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// "Variable types:" lines (Gurobi prints two — before and after presolve).
fn parse_variable_types(text: &str) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let re =
        Regex::new(r"Variable types:\s+(\d+)\s+continuous,\s+(\d+)\s+integer\s+\((\d+)\s+binary\)")
            .unwrap();
    let mut caps: Vec<_> = re.captures_iter(text).collect();
    let before = caps.first().map(|c| {
        let mut o = serde_json::Map::new();
        o.insert(
            "continuous".into(),
            serde_json::Value::from(c[1].parse::<u64>().unwrap_or(0)),
        );
        o.insert(
            "integer".into(),
            serde_json::Value::from(c[2].parse::<u64>().unwrap_or(0)),
        );
        o.insert(
            "binary".into(),
            serde_json::Value::from(c[3].parse::<u64>().unwrap_or(0)),
        );
        serde_json::Value::Object(o)
    });
    let after = if caps.len() >= 2 {
        let c = caps.pop().unwrap();
        let mut o = serde_json::Map::new();
        o.insert(
            "continuous".into(),
            serde_json::Value::from(c[1].parse::<u64>().unwrap_or(0)),
        );
        o.insert(
            "integer".into(),
            serde_json::Value::from(c[2].parse::<u64>().unwrap_or(0)),
        );
        o.insert(
            "binary".into(),
            serde_json::Value::from(c[3].parse::<u64>().unwrap_or(0)),
        );
        Some(serde_json::Value::Object(o))
    } else {
        None
    };
    (before, after)
}

fn parse_cpu_info(text: &str) -> Option<serde_json::Value> {
    let cpu_re = Regex::new(r"CPU model:\s+(.+)").unwrap();
    let thr_re = Regex::new(
        r"Thread count:\s+(\d+)\s+physical cores?,\s+(\d+)\s+logical processors?, using up to (\d+) threads?",
    )
    .unwrap();
    let mut obj = serde_json::Map::new();
    if let Some(c) = cpu_re.captures(text) {
        obj.insert(
            "cpu".into(),
            serde_json::Value::String(c[1].trim().to_string()),
        );
    }
    if let Some(c) = thr_re.captures(text) {
        obj.insert(
            "physical_cores".into(),
            serde_json::Value::from(c[1].parse::<u64>().unwrap_or(0)),
        );
        obj.insert(
            "logical_processors".into(),
            serde_json::Value::from(c[2].parse::<u64>().unwrap_or(0)),
        );
        obj.insert(
            "threads_used".into(),
            serde_json::Value::from(c[3].parse::<u64>().unwrap_or(0)),
        );
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// "Found heuristic solution: objective N" — pre-B&B incumbents. Captures in order.
fn parse_heuristic_solutions(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(r"Found heuristic solution:\s+objective\s+([-\d.eE+]+)").unwrap();
    let mut arr: Vec<serde_json::Value> = Vec::new();
    for c in re.captures_iter(text) {
        if let Ok(v) = c[1].parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(v) {
                arr.push(serde_json::Value::Number(n));
            }
        }
    }
    (!arr.is_empty()).then(|| serde_json::Value::Array(arr))
}

/// "Solution count 10: 7615 7865 8055 ... 8920" — top-K solutions in the pool.
fn parse_solution_pool(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(r"Solution count (\d+):\s+(.+)").unwrap();
    let c = re.captures(text)?;
    let count: u64 = c[1].parse().ok()?;
    let vals: Vec<serde_json::Value> = c[2]
        .split_whitespace()
        .filter(|t| *t != "...")
        .filter_map(|t| {
            let v: f64 = t.parse().ok()?;
            serde_json::Number::from_f64(v).map(serde_json::Value::Number)
        })
        .collect();
    let mut obj = serde_json::Map::new();
    obj.insert("count".into(), serde_json::Value::from(count));
    obj.insert("top_values".into(), serde_json::Value::Array(vals));
    Some(serde_json::Value::Object(obj))
}

fn re_fingerprint() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Model fingerprint:\s+(\S+)").unwrap())
}

fn re_optimal_tolerance() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Optimal solution found \(tolerance\s+([0-9.eE+\-]+)\)").unwrap())
}

fn re_work_units() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\(([\d.]+)\s+work units\)").unwrap())
}

fn parse_f64_json(s: &str) -> serde_json::Value {
    if let Ok(v) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.to_string())
}
