//! Human-readable `Display` impl for [`SolverLog`].
//!
//! Short, compact glance at the parsed log — solver identity, termination,
//! bounds, presolve reduction, a gap-over-time sparkline, and a collapsed
//! progress table. Not round-trippable; use JSON (see [`crate::output`])
//! when you need the full data preserved.

use crate::schema::*;
use std::fmt;

impl fmt::Display for SolverLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_summary(self, f, true)
    }
}

/// A view over a [`SolverLog`] that renders the summary without the progress
/// table (but keeps the `convergence:` sparkline — one line — since that's
/// a derived insight, not the raw table).
///
/// ```
/// let log = miplog::SolverLog::new(miplog::Solver::Gurobi);
/// println!("{}", log.summary_no_table());
/// ```
pub struct SummaryNoTable<'a>(pub &'a SolverLog);

impl fmt::Display for SummaryNoTable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_summary(self.0, f, false)
    }
}

impl SolverLog {
    /// Render the summary without the progress table. See [`SummaryNoTable`].
    pub fn summary_no_table(&self) -> SummaryNoTable<'_> {
        SummaryNoTable(self)
    }
}

/// Short, human-readable glance at a [`SolverLog`]. Emitted by `{}` Display.
fn fmt_summary(log: &SolverLog, f: &mut fmt::Formatter<'_>, include_table: bool) -> fmt::Result {
    // Line 1: solver identity + problem + status + wall
    write!(f, "solver: {}", log.solver.key())?;
    if let Some(v) = &log.version {
        write!(f, " {v}")?;
    }
    writeln!(f)?;
    if let Some(p) = &log.problem {
        writeln!(f, "problem: {p}")?;
    }
    let status = summary_status_word(log.termination.status);
    match log.timing.wall_seconds {
        Some(t) => writeln!(f, "status: {status} in {t:.2}s")?,
        None => writeln!(f, "status: {status}")?,
    }

    // Bounds — one label per line. For Optimal, collapse primal==dual into
    // a single `obj:`. Otherwise report primal, dual, and gap separately so
    // the tightness of a time-limited run is obvious.
    let b = &log.bounds;
    let is_optimal = matches!(log.termination.status, Status::Optimal);
    match (is_optimal, b.primal, b.dual) {
        (true, Some(p), Some(d)) if close_enough(p, d) => {
            writeln!(f, "obj: {}", trim_f(p))?;
        }
        (_, primal, dual) => {
            if let Some(p) = primal {
                writeln!(f, "primal: {}", trim_f(p))?;
            }
            if let Some(d) = dual {
                writeln!(f, "dual: {}", trim_f(d))?;
            }
            if !is_optimal {
                if let Some(g) = b.effective_gap() {
                    writeln!(f, "gap: {:.2}%", g * 100.0)?;
                }
            }
        }
    }
    if let Some(s) = log.tree.solutions_found {
        writeln!(f, "sols: {s}")?;
    }

    // Presolve reduction.
    let p = &log.presolve;
    let rows = fmt_dim_change(p.rows_before, p.rows_after);
    let cols = fmt_dim_change(p.cols_before, p.cols_after);
    match (rows, cols) {
        (Some(r), Some(c)) => writeln!(f, "presolve: {r} rows, {c} cols")?,
        (Some(r), None) => writeln!(f, "presolve: {r} rows")?,
        (None, Some(c)) => writeln!(f, "presolve: {c} cols")?,
        (None, None) => {}
    }

    // Gap convergence sparkline (labeled `convergence:` to avoid clashing
    // with the `gap:` bounds line above).
    if let Some(line) = gap_sparkline(&log.progress) {
        writeln!(f, "{line}")?;
    }

    // Progress table: first/last rows + every row with a bound change.
    if include_table && log.progress.len() >= 6 {
        writeln!(f)?;
        write_summary_table(f, &log.progress)?;
    }

    Ok(())
}

/// 20-character Unicode sparkline of gap over time, labeled with the endpoints.
/// Returns None when the table lacks enough data to sample.
fn gap_sparkline(t: &ProgressTable) -> Option<String> {
    const W: usize = 20;
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if t.is_empty() {
        return None;
    }
    // Collect (time, effective gap) for rows. Three cases:
    //   1) gap reported → use it
    //   2) gap missing but both bounds known → derive
    //   3) gap missing because dual is unbounded ("inf"/"-inf"/null) but a
    //      primal exists → treat as "infinite gap" placeholder so the
    //      trajectory starts at the top of the chart instead of dropping
    //      the row and losing the early-search shape
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let mut has_inf_marker = false;
    for i in 0..t.len() {
        let mut g = match t.gap[i] {
            Some(g) if g.is_finite() => Some(g),
            Some(_) => None,
            None => match (t.primal[i], t.dual[i]) {
                (Some(p), Some(d)) if d.is_finite() && p.is_finite() => {
                    Some((p - d).abs() / p.abs().max(1e-10))
                }
                _ => None,
            },
        };
        if g.is_none() && t.primal[i].is_some() {
            has_inf_marker = true;
            g = Some(f64::INFINITY);
        }
        if let Some(g) = g {
            pts.push((t.time_seconds[i], g));
        }
    }
    if pts.len() < 3 {
        return None;
    }
    if has_inf_marker {
        let max_finite = pts
            .iter()
            .filter_map(|(_, g)| g.is_finite().then_some(*g))
            .fold(0.0f64, f64::max)
            .max(1.0);
        for (_, g) in pts.iter_mut() {
            if !g.is_finite() {
                *g = max_finite;
            }
        }
    }
    let t_min = pts.first()?.0;
    let t_max = pts.last()?.0;
    let time_spread = (t_max - t_min).abs() > 1e-6;
    let max_gap = pts
        .iter()
        .map(|(_, g)| *g)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(1e-9);
    let mut sparks = String::with_capacity(W * 3);
    for i in 0..W {
        let (_, g) = if time_spread {
            let target = t_min + (t_max - t_min) * (i as f64 / (W - 1) as f64);
            *pts.iter()
                .min_by(|a, b| {
                    (a.0 - target)
                        .abs()
                        .partial_cmp(&(b.0 - target).abs())
                        .unwrap()
                })
                .unwrap()
        } else {
            let idx = (i * (pts.len() - 1)) / (W - 1);
            pts[idx]
        };
        let level = ((g / max_gap) * (BLOCKS.len() - 1) as f64).round() as usize;
        sparks.push(BLOCKS[level.min(BLOCKS.len() - 1)]);
    }
    Some(format!("convergence: {sparks}"))
}

/// Render a compact view of the progress table.
///
/// Rule: only collapse consecutive rows that carry no new bound information.
/// A row is kept if it changes `primal`, `dual`, or carries an event; a run
/// of identical rows becomes a single "... N more rows ..." marker. First and
/// last rows are always kept so the time range is visible.
fn write_summary_table(f: &mut fmt::Formatter<'_>, t: &ProgressTable) -> fmt::Result {
    let n = t.len();
    let mut keep: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    keep.insert(0);
    keep.insert(n - 1);
    let mut last_primal: Option<f64> = None;
    let mut last_dual: Option<f64> = None;
    for i in 0..n {
        let changed_primal = t.primal[i] != last_primal;
        let changed_dual = t.dual[i] != last_dual;
        let has_event = t.event[i].is_some();
        if changed_primal || changed_dual || has_event {
            keep.insert(i);
            last_primal = t.primal[i];
            last_dual = t.dual[i];
        }
    }

    // Closing 1-row gaps: eliding a single row costs more than showing it.
    let current: Vec<usize> = keep.iter().copied().collect();
    for w in current.windows(2) {
        if w[1] == w[0] + 2 {
            keep.insert(w[0] + 1);
        }
    }

    writeln!(
        f,
        "    {:>7}  {:>8}  {:>13}  {:>13}  {:>6}  event",
        "time", "nodes", "dual", "primal", "gap",
    )?;
    let mut prev: Option<usize> = None;
    for i in keep {
        if let Some(p) = prev {
            let n = i - p - 1;
            if n > 0 {
                let s = if n == 1 { "row" } else { "rows" };
                writeln!(f, "    … same for {n} more {s} …")?;
            }
        }
        writeln!(
            f,
            "    {:>7.2}  {:>8}  {:>13}  {:>13}  {:>6}  {}",
            t.time_seconds[i],
            fmt_opt_u(t.nodes_explored[i]),
            fmt_sci(t.dual[i]),
            fmt_sci(t.primal[i]),
            t.gap[i]
                .map(|g| format!("{:.1}%", g * 100.0))
                .unwrap_or_else(|| "-".into()),
            match &t.event[i] {
                Some(NodeEvent::Heuristic) => "H",
                Some(NodeEvent::BranchSolution) => "*",
                Some(NodeEvent::Cutoff) => "cutoff",
                Some(NodeEvent::Other(s)) => s,
                None => "",
            },
        )?;
        prev = Some(i);
    }
    Ok(())
}

fn close_enough(p: f64, d: f64) -> bool {
    // Treat bounds as equal for "obj=" display if relative diff < 0.05%.
    (p - d).abs() <= 5e-4 * p.abs().max(1.0)
}

/// Render a before→after dimension pair, skipping when both are unknown and
/// omitting the arrow when only one side is known.
fn fmt_dim_change(before: Option<u64>, after: Option<u64>) -> Option<String> {
    match (before, after) {
        (Some(b), Some(a)) if b == a => Some(format!("{a}")),
        (Some(b), Some(a)) => Some(format!("{b}→{a}")),
        (Some(b), None) => Some(format!("{b}")),
        (None, Some(a)) => Some(format!("{a}")),
        (None, None) => None,
    }
}

fn summary_status_word(s: Status) -> &'static str {
    match s {
        Status::Optimal => "optimal",
        Status::Infeasible => "infeasible",
        Status::Unbounded => "unbounded",
        Status::InfeasibleOrUnbounded => "infeasible_or_unbounded",
        Status::TimeLimit => "time-limit",
        Status::MemoryLimit => "memory-limit",
        Status::OtherLimit => "limit",
        Status::UserInterrupt => "interrupted",
        Status::NumericalError => "numerical-error",
        Status::Unknown => "unknown",
    }
}

/// Fixed-width scientific notation with 6 digits of precision — matching the
/// default MIP feasibility/optimality tolerances of most solvers (≈ 1e-6).
/// Columns stay right-aligned even when objective magnitudes span orders of
/// magnitude. None renders as "-".
fn fmt_sci(v: Option<f64>) -> String {
    match v {
        None => "-".into(),
        Some(0.0) => "0".into(),
        Some(v) => format!("{v:.6e}"),
    }
}

fn trim_f(v: f64) -> String {
    // Avoid trailing zeros for clean summary: 7615 not 7615.00
    if v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.0}", v)
    } else {
        format!("{v:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn fmt_opt_u(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}
