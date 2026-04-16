//! FICO Xpress log parser. Tested against Xpress 9.6–9.8 output.

use crate::solvers::progress::{event_from_marker, parse_gap, parse_or_dash};
use crate::{schema::*, LogParser, ParseError, Solver};
use regex::Regex;
use std::sync::OnceLock;

pub struct XpressParser;

impl LogParser for XpressParser {
    fn solver(&self) -> Solver {
        Solver::Xpress
    }

    fn sniff(&self, text: &str) -> bool {
        text.contains("FICO Xpress")
    }

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError> {
        if !self.sniff(text) {
            return Err(ParseError::WrongSolver("xpress"));
        }
        let mut log = SolverLog::new(Solver::Xpress);

        if let Some(c) = re_version().captures(text) {
            log.version = Some(c[1].to_string());
        }
        if let Some(c) = re_readprob().captures(text) {
            log.problem = Some(c[1].trim().to_string());
        }

        // Status. Xpress prints "*** Search completed ***" when the MIP B&B
        // finished normally; "*** Search unfinished ***" when maxtime hit.
        // Check infeasibility BEFORE "Search completed" — Xpress prints
        // "Problem is integer infeasible" *after* "*** Search completed ***",
        // so taking the first match would mis-classify the run as Optimal.
        if text.contains("Problem is integer infeasible")
            || text.contains("Problem is infeasible")
            || text.contains("The problem is infeasible")
        {
            log.termination.status = Status::Infeasible;
        } else if text.contains("*** Search completed ***") {
            log.termination.status = Status::Optimal;
        } else if !text.contains("MILP") && !text.contains("Final MIP")
            && !text.contains("Starting root cutting")
            && (text.contains("Dual solved problem")
                || text.contains("Optimal solution found"))
        {
            // LP-only run (no B&B). Xpress always solves an LP relaxation
            // first even on MIPs, so we restrict this branch to logs that
            // lack any MIP-specific marker ("MILP", "Final MIP", "Starting
            // root cutting"). Otherwise the MILP time-/node-limit logs would
            // false-positive as Optimal LP.
            log.termination.status = Status::Optimal;
        } else if let Some(reason) = xpress_stop_reason(text) {
            // STOPPING - MAXTIME / MAXNODE / MAXSOL / MAXMIPSOL / MIPRELSTOP /
            // MIPABSSTOP target reached. Distinguish time vs other-limit.
            log.termination.raw_reason = Some(reason.clone());
            log.termination.status = if reason.contains("MAXTIME") {
                Status::TimeLimit
            } else {
                Status::OtherLimit
            };
        } else if text.contains("*** Search unfinished ***") {
            // Fallback when no STOPPING line is present.
            log.termination.status = Status::TimeLimit;
            log.termination.raw_reason = Some("Search unfinished".into());
        }

        // Time: "Solution time / primaldual integral :     T.TTs/ ..."
        if let Some(c) = re_soltime().captures(text) {
            log.timing.wall_seconds = c[1].parse().ok();
        }
        // LP-only fallback: "  N simplex iterations in 0.00 seconds at time 0"
        if log.timing.wall_seconds.is_none() {
            if let Some(c) = re_lp_simplex_summary().captures(text) {
                log.timing.wall_seconds = c[2].parse().ok();
                if log.tree.simplex_iterations.is_none() {
                    log.tree.simplex_iterations = c[1].parse().ok();
                }
            }
        }

        // Bounds: prefer the MIP-specific "Final MIP objective"/bound lines.
        // Fall back to LP-only "Final objective" (mirrored into both bounds
        // since LP optimality is duality-tight).
        if let Some(c) = re_final_obj().captures(text) {
            log.bounds.primal = c[1].parse().ok();
        }
        if let Some(c) = re_final_bound().captures(text) {
            log.bounds.dual = c[1].parse().ok();
        }
        if log.bounds.primal.is_none() {
            // LP-only: "Final objective : -5.0e+00"
            if let Some(c) = re_lp_final_obj().captures(text) {
                let v: Option<f64> = c[1].parse().ok();
                log.bounds.primal = v;
                if log.bounds.dual.is_none() {
                    log.bounds.dual = v;
                }
            }
        }

        // "Number of solutions found / nodes:  N / M"
        if let Some(c) = re_sols_nodes().captures(text) {
            log.tree.solutions_found = c[1].parse().ok();
            log.tree.nodes_explored = c[2].parse().ok();
        }

        // Pre-presolve dims — two formats seen:
        //   (a) "Problem Statistics" multi-line block (older Xpress banner)
        //   (b) "Original problem has: 133 rows 201 cols 1923 elements" (newer)
        if let Some(c) = re_problem_stats().captures(text) {
            log.presolve.rows_before = c[1].parse().ok();
            log.presolve.cols_before = c[2].parse().ok();
            log.presolve.nonzeros_before = c[3].parse().ok();
        } else if let Some(c) = re_original_one_line().captures(text) {
            log.presolve.rows_before = c[1].parse().ok();
            log.presolve.cols_before = c[2].parse().ok();
            log.presolve.nonzeros_before = c[3].parse().ok();
        }

        // Presolved problem dims (two format variants, same pattern).
        if let Some(c) = re_presolved().captures(text) {
            log.presolve.rows_after = c[1].parse().ok();
            log.presolve.cols_after = c[2].parse().ok();
            log.presolve.nonzeros_after = c[3].parse().ok();
        }

        // Presolve finished in T seconds
        if let Some(c) = re_presolve_time().captures(text) {
            log.timing.presolve_seconds = c[1].parse().ok();
        }

        // Root LP (dual bound before branching): "Final objective : 7.185e+03"
        // appears right after the concurrent LP solve, before root cutting.
        if let Some(c) = re_root_final_obj().captures(text) {
            log.bounds.root_dual = c[1].parse().ok();
        }

        // Primal-dual integral: "Solution time / primaldual integral : 0.13s/ 21.48%"
        if let Some(c) = re_pd_integral().captures(text) {
            log.bounds.primal_dual_integral = c[1].parse::<f64>().ok().map(|v| v / 100.0);
        }

        // First heuristic solution: "*** Solution found: 10170.00000 Time: 0.01 Heuristic: e ***"
        if let Some(c) = re_first_solution_found().captures(text) {
            log.bounds.first_primal = c[1].parse().ok();
            log.bounds.first_primal_time_seconds = c[2].parse().ok();
        }

        // Cuts total: "Cuts in the matrix : 29"
        if let Some(c) = re_cuts_total().captures(text) {
            if let Ok(n) = c[1].parse::<u64>() {
                if n > 0 {
                    log.cuts.insert("total".into(), n);
                }
            }
        }

        // Work units: "Work / work units per second : 0.32 / 2.45"
        //  -> expose as metadata-only; no common schema field (yet).

        log.progress = parse_progress(text);

        // Max depth from progress table, if any depth values are populated.
        log.tree.max_depth = log.progress.depth.iter().filter_map(|d| *d).max();

        populate_other_data(text, &mut log);

        Ok(log)
    }
}

fn populate_other_data(text: &str, log: &mut SolverLog) {
    if let Some(v) = parse_coefficient_ranges(text) {
        log.other_data.push(NamedValue::new("xpress.coefficient_ranges", v));
    }
    if let Some(v) = parse_symmetry(text) {
        log.other_data.push(NamedValue::new("xpress.symmetry", v));
    }
    if let Some(v) = parse_threads_and_memory(text) {
        log.other_data.push(NamedValue::new("xpress.run_config", v));
    }
    if let Some(v) = parse_heuristic_solutions(text) {
        log.other_data.push(NamedValue::new("xpress.pre_bb_heuristic_solutions", v));
    }
    if let Some(v) = parse_work_units(text) {
        log.other_data.push(NamedValue::new("xpress.work", v));
    }
    if let Some(v) = parse_stopping_reason(text) {
        log.other_data.push(NamedValue::new("xpress.stopping_reason", v));
    }
    if let Some(v) = parse_lp_violations(text) {
        log.other_data.push(NamedValue::new("xpress.solution_quality", v));
    }
}

/// Parse the 3-row "Coefficient range" block (original vs solved side-by-side).
fn parse_coefficient_ranges(text: &str) -> Option<serde_json::Value> {
    let hdr = Regex::new(r"(?m)^Coefficient range\s").unwrap();
    let m = hdr.find(text)?;
    // Rows: "  <Label> [min,max] : [ a, b] / [ c, d]"
    let row = Regex::new(
        r"^\s+(Coefficients|RHS and bounds|Objective)\s+\[min,max\]\s*:\s*\[\s*([^\],]+?),\s*([^\]]+?)\]\s*/\s*\[\s*([^\],]+?),\s*([^\]]+?)\]",
    )
    .unwrap();
    let mut obj = serde_json::Map::new();
    for line in text[m.end()..].lines().skip(1).take(6) {
        if line.trim().is_empty() {
            break;
        }
        if let Some(c) = row.captures(line) {
            let name = match &c[1] {
                "RHS and bounds" => "rhs_and_bounds".to_string(),
                s => s.to_lowercase(),
            };
            let mut group = serde_json::Map::new();
            let mut orig = serde_json::Map::new();
            orig.insert("min".into(), parse_f64_json(c[2].trim()));
            orig.insert("max".into(), parse_f64_json(c[3].trim()));
            let mut solved = serde_json::Map::new();
            solved.insert("min".into(), parse_f64_json(c[4].trim()));
            solved.insert("max".into(), parse_f64_json(c[5].trim()));
            group.insert("original".into(), serde_json::Value::Object(orig));
            group.insert("solved".into(), serde_json::Value::Object(solved));
            obj.insert(name, serde_json::Value::Object(group));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// Parse the "Symmetric problem" block:
///   Symmetric problem: generators: 2, support set: 178
///    Number of orbits: 52, largest orbit: 4
///    Row orbits: 39, row support: 96
fn parse_symmetry(text: &str) -> Option<serde_json::Value> {
    let hdr = Regex::new(r"(?m)^Symmetric problem:").unwrap();
    let m = hdr.find(text)?;
    let body: String = std::iter::once(&text[m.start()..m.end()])
        .chain(text[m.end()..].lines().skip(1).take(3).map(|l| &l[..]))
        .collect::<Vec<_>>()
        .join(" ");
    let mut obj = serde_json::Map::new();
    for (key, re_src) in [
        ("generators", r"generators:\s+(\d+)"),
        ("support_set", r"support set:\s+(\d+)"),
        ("orbits", r"Number of orbits:\s+(\d+)"),
        ("largest_orbit", r"largest orbit:\s+(\d+)"),
        ("row_orbits", r"Row orbits:\s+(\d+)"),
        ("row_support", r"row support:\s+(\d+)"),
    ] {
        if let Some(c) = Regex::new(re_src).unwrap().captures(&body) {
            obj.insert(key.into(), parse_f64_json(&c[1]));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

/// "Minimizing MILP p0201 using up to 14 threads and up to 24GB memory"
fn parse_threads_and_memory(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(
        r"using up to (\d+) threads? and up to (\d+)(GB|MB|KB) memory",
    )
    .unwrap();
    let c = re.captures(text)?;
    let mut obj = serde_json::Map::new();
    obj.insert("threads".into(), parse_f64_json(&c[1]));
    obj.insert("memory_limit".into(), serde_json::Value::String(format!("{}{}", &c[2], &c[3])));
    Some(serde_json::Value::Object(obj))
}

/// "*** Solution found: 10170.00000 Time: 0.01 Heuristic: e ***"
fn parse_heuristic_solutions(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(
        r"\*\*\* Solution found:\s+([\d.eE+\-]+)\s+Time:\s+([\d.]+)\s+Heuristic:\s+(\S+)\s*\*\*\*",
    )
    .unwrap();
    let mut arr: Vec<serde_json::Value> = Vec::new();
    for c in re.captures_iter(text) {
        let mut o = serde_json::Map::new();
        o.insert("value".into(), parse_f64_json(&c[1]));
        o.insert("time".into(), parse_f64_json(&c[2]));
        o.insert("heuristic".into(), serde_json::Value::String(c[3].to_string()));
        arr.push(serde_json::Value::Object(o));
    }
    (!arr.is_empty()).then(|| serde_json::Value::Array(arr))
}

/// "Work / work units per second : 0.32 / 2.45"
fn parse_work_units(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(
        r"Work\s*/\s*work units per second\s*:\s*([\d.]+)\s*/\s*([\d.]+)",
    )
    .unwrap();
    let c = re.captures(text)?;
    let mut obj = serde_json::Map::new();
    obj.insert("work".into(), parse_f64_json(&c[1]));
    obj.insert("work_units_per_second".into(), parse_f64_json(&c[2]));
    Some(serde_json::Value::Object(obj))
}

/// First "STOPPING - <REASON>" trigger keyword (MAXTIME / MAXNODE / MAXSOL /
/// MIPRELSTOP / MIPABSSTOP / etc.). Used to classify termination status.
fn xpress_stop_reason(text: &str) -> Option<String> {
    let re = Regex::new(r"STOPPING - (\S+)").unwrap();
    re.captures(text).map(|c| c[1].to_string())
}

/// "STOPPING - MIPRELSTOP target reached (MIPRELSTOP=0.0001  gap=0)."
fn parse_stopping_reason(text: &str) -> Option<serde_json::Value> {
    let re = Regex::new(r"STOPPING - ([^(\n]+)(?:\(([^)]+)\))?\.?").unwrap();
    let c = re.captures(text)?;
    let mut obj = serde_json::Map::new();
    obj.insert("reason".into(), serde_json::Value::String(c[1].trim().to_string()));
    if let Some(extra) = c.get(2) {
        obj.insert("detail".into(), serde_json::Value::String(extra.as_str().to_string()));
    }
    Some(serde_json::Value::Object(obj))
}

/// LP final violations (primal/dual/complementarity).
fn parse_lp_violations(text: &str) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    for (k, re_src) in [
        ("max_primal_violation", r"Max primal violation\s+\(abs/rel\)\s*:\s+([\d.eE+\-]+)"),
        ("max_dual_violation", r"Max dual violation\s+\(abs/rel\)\s*:\s+([\d.eE+\-]+)"),
        ("max_integer_violation", r"Max integer violation\s+\(abs\s*\)\s*:\s+([\d.eE+\-]+)"),
        ("max_complementarity_violation", r"Max complementarity viol\.\s+\(abs/rel\)\s*:\s+([\d.eE+\-]+)"),
    ] {
        if let Some(c) = Regex::new(re_src).unwrap().captures(text) {
            obj.insert(k.into(), parse_f64_json(&c[1]));
        }
    }
    (!obj.is_empty()).then(|| serde_json::Value::Object(obj))
}

fn parse_f64_json(s: &str) -> serde_json::Value {
    if let Ok(v) = s.trim().parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(s.trim().to_string())
}

/// Parse Xpress's progress tables. Xpress has two distinct progress
/// formats depending on whether B&B branching occurred:
///
/// 1. **B&B tree** table (printed when actual branching happens):
///    `Node BestSoln BestBound Sols Active Depth Gap GInf Time` (9 cols)
/// 2. **Root cutting & heuristics** table (printed when the root round
///    closes the gap without branching):
///    `Its Type BestSoln BestBound Sols Add Del Gap GInf Time` (10 cols)
///
/// Both tables can also contain "P"-marked rows which are new-incumbent
/// events that skip several columns. We handle both shapes.
fn parse_progress(text: &str) -> ProgressTable {
    let mut out = ProgressTable::default();
    #[derive(Copy, Clone, PartialEq)]
    enum Kind {
        None,
        BbTree,
        RootCutting,
    }
    let mut kind = Kind::None;

    for line in text.lines() {
        if kind == Kind::None {
            // Root-cutting header: contains "Its", "Type", "BestSoln", "BestBound", "Time".
            if line.contains("Its")
                && line.contains("BestBound")
                && line.contains("Add")
                && line.contains("Del")
            {
                kind = Kind::RootCutting;
                continue;
            }
            // B&B header.
            if line.contains("Node")
                && line.contains("BestBound")
                && line.contains("Active")
                && line.trim_end().ends_with("Time")
            {
                kind = Kind::BbTree;
                continue;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if !out.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with("***")
            || trimmed.starts_with("Final MIP")
            || trimmed.starts_with("Uncrunching")
            || trimmed.starts_with("Heap usage")
            || trimmed.starts_with("Cuts in the matrix")
            || trimmed.starts_with("STOPPING")
        {
            break;
        }
        let row = match kind {
            Kind::BbTree => parse_bb_row(line),
            Kind::RootCutting => parse_root_cutting_row(line),
            Kind::None => None,
        };
        if let Some(r) = row {
            out.push(r);
        }
    }
    out
}

fn parse_bb_row(line: &str) -> Option<NodeSnapshot> {
    // Peel an optional single-letter marker.
    let (event, body) = match line.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => (event_from_marker(c), &line[c.len_utf8()..]),
        _ => (None, line),
    };
    let toks: Vec<&str> = body.split_whitespace().collect();
    let mut snap = NodeSnapshot::default();
    match toks.len() {
        9 => {
            snap.nodes_explored = toks[0].parse().ok();
            snap.primal = parse_or_dash(toks[1]);
            snap.dual = parse_or_dash(toks[2]);
            snap.depth = toks[5].parse().ok();
            snap.gap = parse_gap(toks[6]);
            snap.time_seconds = toks[8].parse().ok()?;
        }
        7 => {
            snap.nodes_explored = toks[0].parse().ok();
            snap.dual = parse_or_dash(toks[1]);
            snap.depth = toks[4].parse().ok();
            snap.time_seconds = toks[6].parse().ok()?;
        }
        _ => return None,
    }
    if snap.nodes_explored.is_none() {
        return None;
    }
    snap.event = event;
    Some(snap)
}

/// Root cutting rows. Two shapes:
///   Standard:  "  1  K   7995.0  7265.2  2  15  0  9.13%  24  0"   (10 tok)
///   Incumbent: "P        7865.0  7265.2  3                 7.63%   0  0"   (8 tok)
fn parse_root_cutting_row(line: &str) -> Option<NodeSnapshot> {
    let first = line.chars().next()?;
    let incumbent = matches!(first, 'P');
    let event = if incumbent {
        Some(NodeEvent::BranchSolution)
    } else {
        None
    };
    let toks: Vec<&str> = line.split_whitespace().collect();
    let mut snap = NodeSnapshot::default();
    snap.event = event;
    if incumbent {
        // P BestSoln BestBound Sols Gap GInf Time  (7 tok after marker)
        if toks.len() < 7 {
            return None;
        }
        snap.primal = parse_or_dash(toks[1]);
        snap.dual = parse_or_dash(toks[2]);
        // toks[3] = Sols
        snap.gap = parse_gap(toks[4]);
        // toks[5] = GInf
        snap.time_seconds = toks[toks.len() - 1].parse().ok()?;
    } else {
        // Its Type BestSoln BestBound Sols Add Del Gap GInf Time  (10 tok)
        if toks.len() < 10 {
            return None;
        }
        // "Its" is not a node count, but it's the closest analogue — treat
        // it as iteration index via `lp_iterations` rather than `nodes_explored`.
        snap.lp_iterations = toks[0].parse().ok();
        snap.primal = parse_or_dash(toks[2]);
        snap.dual = parse_or_dash(toks[3]);
        snap.gap = parse_gap(toks[7]);
        snap.time_seconds = toks[9].parse().ok()?;
    }
    Some(snap)
}

fn re_problem_stats() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?ms)Problem Statistics\s*\n\s*(\d+)\s+.*?rows\s*\n\s*(\d+)\s+.*?structural columns\s*\n\s*(\d+)\s+.*?non-zero elements",
        )
        .unwrap()
    })
}
/// "Original problem has: 133 rows 201 cols 1923 elements" (newer Xpress)
fn re_original_one_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?ms)Original problem has:\s*\n\s*(\d+)\s+rows?\s+(\d+)\s+cols?\s+(\d+)\s+elements",
        )
        .unwrap()
    })
}
fn re_root_final_obj() -> &'static Regex {
    // The first "Final objective" is the root LP solve (before the MIP loop).
    // Distinguishing it from "Final MIP objective" is easy: the LP version
    // appears BEFORE "Starting root cutting & heuristics".
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?ms)Final objective\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_pd_integral() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Solution time\s*/\s*primaldual integral\s*:\s*[\d.]+s/\s*([\d.]+)%")
            .unwrap()
    })
}
fn re_first_solution_found() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"\*\*\* Solution found:\s+([\d.eE+\-]+)\s+Time:\s+([\d.]+)",
        )
        .unwrap()
    })
}
fn re_cuts_total() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Cuts in the matrix\s*:\s*(\d+)").unwrap())
}
fn re_presolve_time() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Presolve finished in (\d+) seconds").unwrap())
}

fn re_version() -> &'static Regex {
    // Xpress banner variants seen in the wild:
    //   "FICO Xpress Solver 64bit v9.8.0 Oct 22 2025"
    //   "FICO Xpress v9.8.1, Community, solve started ..."
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"FICO Xpress(?:\s+Solver\s+\S+)?\s+v(\d+\.\d+\.\d+)").unwrap())
}
fn re_readprob() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Reading Problem\s+(\S+)").unwrap())
}
fn re_soltime() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Solution time\s*/.*?:\s*([\d.]+)s").unwrap())
}
fn re_final_obj() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Final MIP objective\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_lp_simplex_summary() -> &'static Regex {
    // "  85 simplex iterations in 0.00 seconds at time 0"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(\d+) simplex iterations? in ([\d.]+) seconds").unwrap()
    })
}
fn re_lp_final_obj() -> &'static Regex {
    // LP-only termination: matches "Final objective : X" but only when
    // it isn't the MIP variant (handled separately above).
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^Final objective\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_final_bound() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"Final MIP bound\s*:\s*([\d.eE+\-]+)").unwrap())
}
fn re_sols_nodes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Number of solutions found\s*/\s*nodes\s*:\s*(\d+)\s*/\s*(\d+)").unwrap()
    })
}
fn re_presolved() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"Presolved problem has:\s+(\d+)\s+rows\s+(\d+)\s+cols\s+(\d+)\s+elements")
            .unwrap()
    })
}
