//! Two text forms of [`SolverLog`]:
//!
//! * **Summary** — the default `Display` output. A 2–4 line, human-readable
//!   glance with only the universal vocabulary (solver/status/time/bounds/
//!   presolve). Not round-trippable.
//! * **`miplog-text` v1** — the alternate `Display` output (via `{:#}`) and
//!   what [`from_text`] parses. Full fidelity, ASCII-only, round-trippable.
//!   Grammar and stability documented in `FORMAT.md`.
//!
//! Use JSON (see [`crate::output`]) when you need the full data in a form
//! other tools can consume.

use crate::schema::*;
use std::collections::BTreeMap;
use std::fmt;

pub const MAGIC: &str = "miplog-text 1";

/* ----------------------------- serialization ----------------------------- */

impl fmt::Display for SolverLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            return fmt_text_v1(self, f);
        }
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
            Some(_) => None, // gap was reported as inf/NaN — fall through
            None => match (t.primal[i], t.dual[i]) {
                (Some(p), Some(d)) if d.is_finite() && p.is_finite() => {
                    Some((p - d).abs() / p.abs().max(1e-10))
                }
                _ => None,
            },
        };
        // Rows with a primal but unreliable dual (None / ±inf) → "infinite gap"
        // sentinel so the trajectory starts at the top of the chart instead of
        // dropping the row and losing the early-search shape.
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
        // Replace `inf` placeholders with max(observed_finite, 1.0) so they
        // render as a full bar without warping the rest of the scale.
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
    // Time-based sampling, with a fallback to index-based when all rows have
    // (near-)identical timestamps — common for sub-second solves where the
    // progress table has no meaningful time spread.
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
        "time", "nodes", "primal", "dual", "gap",
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
            fmt_sci(t.primal[i]),
            fmt_sci(t.dual[i]),
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

/// Emit `miplog-text` v1, the round-trippable full form. `{:#}` on Display.
fn fmt_text_v1(log: &SolverLog, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "{MAGIC}")?;
    writeln!(
        f,
        "solver: name={} version={} git={}",
        log.solver.key(),
        fmt_opt_str(log.version.as_deref()),
        fmt_opt_str(log.solver_git_hash.as_deref()),
    )?;
    if let Some(p) = &log.problem {
        writeln!(f, "problem: {}", quote_if_needed(p))?;
    }
    writeln!(
        f,
        "status: {} reason={}",
        status_key(log.termination.status),
        fmt_opt_str(log.termination.raw_reason.as_deref()),
    )?;
    let t = &log.timing;
    writeln!(
        f,
        "timing: wall={} cpu={} reading={} presolve={} root_relax={}",
        fmt_opt_f(t.wall_seconds),
        fmt_opt_f(t.cpu_seconds),
        fmt_opt_f(t.reading_seconds),
        fmt_opt_f(t.presolve_seconds),
        fmt_opt_f(t.root_relaxation_seconds),
    )?;
    let b = &log.bounds;
    writeln!(
        f,
        "bounds: primal={} dual={} gap={}",
        fmt_opt_f(b.primal),
        fmt_opt_f(b.dual),
        fmt_opt_f(b.gap),
    )?;
    writeln!(
        f,
        "tree: nodes={} simplex_iters={} sols={}",
        fmt_opt_u(log.tree.nodes_explored),
        fmt_opt_u(log.tree.simplex_iterations),
        fmt_opt_u(log.tree.solutions_found),
    )?;
    let p = &log.presolve;
    writeln!(
        f,
        "presolve: rows={}/{} cols={}/{} nnz={}/{}",
        fmt_opt_u(p.rows_before),
        fmt_opt_u(p.rows_after),
        fmt_opt_u(p.cols_before),
        fmt_opt_u(p.cols_after),
        fmt_opt_u(p.nonzeros_before),
        fmt_opt_u(p.nonzeros_after),
    )?;
    write!(f, "cuts:")?;
    for (k, v) in &log.cuts {
        write!(f, " {}={}", quote_if_needed(k), v)?;
    }
    writeln!(f)?;
    writeln!(
        f,
        "progress: rows={} last_time={}",
        log.progress.len(),
        fmt_opt_f(log.progress.last_time()),
    )?;
    if !log.progress.is_empty() {
        writeln!(f, "  # cols: time nodes primal dual gap depth lp event")?;
        for row in log.progress.iter() {
            writeln!(
                f,
                "  {} {} {} {} {} {} {} {}",
                fmt_f(row.time_seconds),
                fmt_opt_u(row.nodes_explored),
                fmt_opt_f(row.primal),
                fmt_opt_f(row.dual),
                fmt_opt_f(row.gap),
                fmt_opt_u32(row.depth),
                fmt_opt_u(row.lp_iterations),
                fmt_event(row.event.as_ref()),
            )?;
        }
    }
    write!(
        f,
        "parser: version={} git={}",
        log.parser.version,
        if log.parser.git_hash.is_empty() {
            "-"
        } else {
            &log.parser.git_hash
        },
    )
}

fn fmt_f(v: f64) -> String {
    // Use `{}` (Rust's default) so integers stay clean and floats stay precise.
    format!("{v}")
}
fn fmt_opt_f(v: Option<f64>) -> String {
    v.map(|x| format!("{x}")).unwrap_or_else(|| "-".into())
}
fn fmt_opt_u(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}
fn fmt_opt_u32(v: Option<u32>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}
fn fmt_opt_str(v: Option<&str>) -> String {
    match v {
        None => "-".into(),
        Some(s) => quote_if_needed(s),
    }
}
fn fmt_event(e: Option<&NodeEvent>) -> String {
    match e {
        None => "-".into(),
        Some(NodeEvent::Heuristic) => "heuristic".into(),
        Some(NodeEvent::BranchSolution) => "branch_solution".into(),
        Some(NodeEvent::Cutoff) => "cutoff".into(),
        Some(NodeEvent::Other(s)) => quote(s),
    }
}

const BAREWORD_OK: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._/:+-";

fn is_bareword(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| BAREWORD_OK.contains(&b)) && s != "-"
}

fn quote_if_needed(s: &str) -> String {
    if is_bareword(s) {
        s.into()
    } else {
        quote(s)
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn status_key(s: Status) -> &'static str {
    match s {
        Status::Optimal => "optimal",
        Status::Infeasible => "infeasible",
        Status::Unbounded => "unbounded",
        Status::InfeasibleOrUnbounded => "infeasible_or_unbounded",
        Status::TimeLimit => "time_limit",
        Status::MemoryLimit => "memory_limit",
        Status::OtherLimit => "other_limit",
        Status::UserInterrupt => "user_interrupt",
        Status::NumericalError => "numerical_error",
        Status::Unknown => "unknown",
    }
}

/* ------------------------------- parsing -------------------------------- */

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("missing magic header; expected `{MAGIC}`")]
    MissingMagic,
    #[error("unsupported format version: {0}")]
    WrongVersion(String),
    #[error("line {line}: {msg}")]
    Parse { line: usize, msg: String },
}

/// Parse the `miplog-text` v1 form back into a [`SolverLog`].
pub fn from_text(input: &str) -> Result<SolverLog, TextError> {
    let mut lines = input.lines().enumerate();

    // Magic line.
    let magic = loop {
        let (_, l) = lines.next().ok_or(TextError::MissingMagic)?;
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        break l;
    };
    if magic != MAGIC {
        if magic.starts_with("miplog-text ") {
            return Err(TextError::WrongVersion(
                magic.strip_prefix("miplog-text ").unwrap().into(),
            ));
        }
        return Err(TextError::MissingMagic);
    }

    let mut log = SolverLog::new(Solver::Gurobi); // overwritten by solver line
    let mut saw_solver = false;
    let mut parsing_progress = false;

    for (i, line_raw) in lines {
        let lineno = i + 1;
        let line = line_raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        // Indented progress row or comment.
        if let Some(body) = line.strip_prefix("  ") {
            if body.trim_start().starts_with('#') {
                continue;
            }
            if !parsing_progress {
                return Err(perr(
                    lineno,
                    "unexpected indented row (no open progress: section)",
                ));
            }
            parse_progress_row(body, lineno, &mut log.progress)?;
            continue;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }

        // End of any currently-open progress section.
        parsing_progress = false;

        let (tag, rest) = split_tag(line).ok_or_else(|| perr(lineno, "expected `tag: ...`"))?;
        match tag {
            "solver" => {
                let tokens = tokenize(rest, lineno)?;
                let name = must_get(&tokens, "name", lineno)?;
                log.solver = Solver::from_key(&name)
                    .ok_or_else(|| perr(lineno, &format!("unknown solver `{name}`")))?;
                log.version = opt_str(&tokens, "version");
                log.solver_git_hash = opt_str(&tokens, "git");
                saw_solver = true;
            }
            "problem" => log.problem = Some(unquote(rest)?),
            "status" => {
                // "status: <enum> reason=<str|->"
                let trimmed = rest.trim();
                let (status_word, after) = trimmed
                    .split_once(char::is_whitespace)
                    .unwrap_or((trimmed, ""));
                log.termination.status = status_from_key(status_word)
                    .ok_or_else(|| perr(lineno, &format!("unknown status `{status_word}`")))?;
                let tokens = tokenize(after, lineno)?;
                log.termination.raw_reason = opt_str(&tokens, "reason");
            }
            "timing" => {
                let tokens = tokenize(rest, lineno)?;
                log.timing.wall_seconds = opt_f(&tokens, "wall");
                log.timing.cpu_seconds = opt_f(&tokens, "cpu");
                log.timing.reading_seconds = opt_f(&tokens, "reading");
                log.timing.presolve_seconds = opt_f(&tokens, "presolve");
                log.timing.root_relaxation_seconds = opt_f(&tokens, "root_relax");
            }
            "bounds" => {
                let tokens = tokenize(rest, lineno)?;
                log.bounds.primal = opt_f(&tokens, "primal");
                log.bounds.dual = opt_f(&tokens, "dual");
                log.bounds.gap = opt_f(&tokens, "gap");
            }
            "tree" => {
                let tokens = tokenize(rest, lineno)?;
                log.tree.nodes_explored = opt_u(&tokens, "nodes");
                log.tree.simplex_iterations = opt_u(&tokens, "simplex_iters");
                log.tree.solutions_found = opt_u(&tokens, "sols");
            }
            "presolve" => {
                let tokens = tokenize(rest, lineno)?;
                let (rb, ra) = split_slash(tokens.get("rows").map(String::as_str), lineno)?;
                let (cb, ca) = split_slash(tokens.get("cols").map(String::as_str), lineno)?;
                let (nb, na) = split_slash(tokens.get("nnz").map(String::as_str), lineno)?;
                log.presolve.rows_before = rb;
                log.presolve.rows_after = ra;
                log.presolve.cols_before = cb;
                log.presolve.cols_after = ca;
                log.presolve.nonzeros_before = nb;
                log.presolve.nonzeros_after = na;
            }
            "cuts" => {
                let tokens = tokenize(rest, lineno)?;
                for (k, v) in tokens {
                    let n: u64 = v
                        .parse()
                        .map_err(|_| perr(lineno, &format!("cuts `{k}`: bad u64")))?;
                    log.cuts.insert(k, n);
                }
            }
            "progress" => {
                // rows=<u> last_time=<f|-> — just a summary; actual rows follow indented.
                parsing_progress = true;
                let _tokens = tokenize(rest, lineno)?;
                // We deliberately ignore the summary values — the indented rows
                // carry the truth. Kept in output for quick human scanning.
            }
            "parser" => {
                let tokens = tokenize(rest, lineno)?;
                log.parser.version = must_get(&tokens, "version", lineno)?;
                log.parser.git_hash = opt_str(&tokens, "git").unwrap_or_default();
            }
            other => return Err(perr(lineno, &format!("unknown tag `{other}`"))),
        }
    }

    if !saw_solver {
        return Err(TextError::Parse {
            line: 0,
            msg: "missing `solver:` line".into(),
        });
    }
    Ok(log)
}

fn perr(line: usize, msg: &str) -> TextError {
    TextError::Parse {
        line,
        msg: msg.into(),
    }
}

fn split_tag(line: &str) -> Option<(&str, &str)> {
    let (tag, rest) = line.split_once(':')?;
    if !tag.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return None;
    }
    Some((tag, rest.strip_prefix(' ').unwrap_or(rest)))
}

fn tokenize(s: &str, line: usize) -> Result<BTreeMap<String, String>, TextError> {
    let mut out = BTreeMap::new();
    let mut it = s.chars().peekable();
    loop {
        while matches!(it.peek(), Some(&c) if c.is_whitespace()) {
            it.next();
        }
        if it.peek().is_none() {
            break;
        }
        // Read key (quoted or bareword).
        let mut key = String::new();
        if it.peek() == Some(&'"') {
            it.next();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some('n') => key.push('\n'),
                        Some('r') => key.push('\r'),
                        Some('t') => key.push('\t'),
                        Some('"') => key.push('"'),
                        Some('\\') => key.push('\\'),
                        Some(o) => key.push(o),
                        None => return Err(perr(line, "unterminated escape in key")),
                    }
                } else if c == '"' {
                    break;
                } else {
                    key.push(c);
                }
            }
        } else {
            while let Some(&c) = it.peek() {
                if c == '=' || c.is_whitespace() {
                    break;
                }
                key.push(c);
                it.next();
            }
        }
        if it.peek() != Some(&'=') {
            return Err(perr(line, &format!("token `{key}` missing `=`")));
        }
        it.next(); // consume =
                   // Read value.
        let value = if it.peek() == Some(&'"') {
            it.next();
            let mut v = String::new();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some('n') => v.push('\n'),
                        Some('r') => v.push('\r'),
                        Some('t') => v.push('\t'),
                        Some('"') => v.push('"'),
                        Some('\\') => v.push('\\'),
                        Some(other) => v.push(other),
                        None => return Err(perr(line, "unterminated escape")),
                    }
                } else if c == '"' {
                    break;
                } else {
                    v.push(c);
                }
            }
            v
        } else {
            let mut v = String::new();
            while let Some(&c) = it.peek() {
                if c.is_whitespace() {
                    break;
                }
                v.push(c);
                it.next();
            }
            v
        };
        out.insert(key, value);
    }
    Ok(out)
}

fn must_get(map: &BTreeMap<String, String>, key: &str, line: usize) -> Result<String, TextError> {
    map.get(key)
        .cloned()
        .ok_or_else(|| perr(line, &format!("missing required key `{key}`")))
}
fn opt_str(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    match map.get(key).map(String::as_str) {
        None | Some("-") => None,
        Some(v) => Some(v.to_string()),
    }
}
fn opt_f(map: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    opt_str(map, key).and_then(|v| v.parse().ok())
}
fn opt_u(map: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    opt_str(map, key).and_then(|v| v.parse().ok())
}

fn split_slash(v: Option<&str>, line: usize) -> Result<(Option<u64>, Option<u64>), TextError> {
    let v = v.unwrap_or("-/-");
    let (a, b) = v
        .split_once('/')
        .ok_or_else(|| perr(line, &format!("expected `a/b`, got `{v}`")))?;
    let parse = |x: &str| -> Result<Option<u64>, TextError> {
        if x == "-" {
            Ok(None)
        } else {
            Ok(Some(
                x.parse()
                    .map_err(|_| perr(line, &format!("bad u64 `{x}`")))?,
            ))
        }
    };
    Ok((parse(a)?, parse(b)?))
}

fn parse_progress_row(body: &str, line: usize, out: &mut ProgressTable) -> Result<(), TextError> {
    // Quoted event may contain spaces; tokenize manually.
    let mut cols: Vec<String> = Vec::with_capacity(9);
    let mut it = body.chars().peekable();
    loop {
        while matches!(it.peek(), Some(&c) if c.is_whitespace()) {
            it.next();
        }
        if it.peek().is_none() {
            break;
        }
        if it.peek() == Some(&'"') {
            it.next();
            let mut v = String::new();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some(other) => v.push(other),
                        None => return Err(perr(line, "unterminated escape in progress row")),
                    }
                } else if c == '"' {
                    break;
                } else {
                    v.push(c);
                }
            }
            cols.push(v);
        } else {
            let mut v = String::new();
            while let Some(&c) = it.peek() {
                if c.is_whitespace() {
                    break;
                }
                v.push(c);
                it.next();
            }
            cols.push(v);
        }
    }
    if cols.len() != 8 {
        return Err(perr(
            line,
            &format!("progress row: expected 8 fields, got {}", cols.len()),
        ));
    }
    let f = |s: &str| -> Result<Option<f64>, TextError> {
        if s == "-" {
            Ok(None)
        } else {
            Ok(Some(
                s.parse()
                    .map_err(|_| perr(line, &format!("bad f64 `{s}`")))?,
            ))
        }
    };
    let u = |s: &str| -> Result<Option<u64>, TextError> {
        if s == "-" {
            Ok(None)
        } else {
            Ok(Some(
                s.parse()
                    .map_err(|_| perr(line, &format!("bad u64 `{s}`")))?,
            ))
        }
    };
    let u32p = |s: &str| -> Result<Option<u32>, TextError> {
        if s == "-" {
            Ok(None)
        } else {
            Ok(Some(
                s.parse()
                    .map_err(|_| perr(line, &format!("bad u32 `{s}`")))?,
            ))
        }
    };
    let t = f(&cols[0])?.ok_or_else(|| perr(line, "progress row: time cannot be `-`"))?;
    let event = match cols[7].as_str() {
        "-" => None,
        "heuristic" => Some(NodeEvent::Heuristic),
        "branch_solution" => Some(NodeEvent::BranchSolution),
        "cutoff" => Some(NodeEvent::Cutoff),
        other => Some(NodeEvent::Other(other.to_string())),
    };
    out.push(NodeSnapshot {
        time_seconds: t,
        nodes_explored: u(&cols[1])?,
        primal: f(&cols[2])?,
        dual: f(&cols[3])?,
        gap: f(&cols[4])?,
        depth: u32p(&cols[5])?,
        lp_iterations: u(&cols[6])?,
        event,
    });
    Ok(())
}

fn unquote(s: &str) -> Result<String, TextError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut it = inner.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(o) => out.push(o),
                    None => {
                        return Err(TextError::Parse {
                            line: 0,
                            msg: "bad escape".into(),
                        })
                    }
                }
            } else {
                out.push(c);
            }
        }
        Ok(out)
    } else {
        Ok(s.to_string())
    }
}

fn status_from_key(s: &str) -> Option<Status> {
    Some(match s {
        "optimal" => Status::Optimal,
        "infeasible" => Status::Infeasible,
        "unbounded" => Status::Unbounded,
        "infeasible_or_unbounded" => Status::InfeasibleOrUnbounded,
        "time_limit" => Status::TimeLimit,
        "memory_limit" => Status::MemoryLimit,
        "other_limit" => Status::OtherLimit,
        "user_interrupt" => Status::UserInterrupt,
        "numerical_error" => Status::NumericalError,
        "unknown" => Status::Unknown,
        _ => return None,
    })
}

impl Solver {
    fn from_key(s: &str) -> Option<Self> {
        Some(match s {
            "gurobi" => Solver::Gurobi,
            "xpress" => Solver::Xpress,
            "scip" => Solver::Scip,
            "highs" => Solver::Highs,
            "cplex" => Solver::Cplex,
            "cbc" => Solver::Cbc,
            "copt" => Solver::Copt,
            "optverse" => Solver::Optverse,
            "mosek" => Solver::Mosek,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SolverLog {
        let mut log = SolverLog::new(Solver::Scip);
        log.version = Some("10.0.0".into());
        log.solver_git_hash = Some("0c80fdd8e9".into());
        log.problem = Some("p 30n20b8".into()); // has space → forces quoting
        log.termination.status = Status::Optimal;
        log.termination.raw_reason = Some("optimal solution found".into());
        log.timing.wall_seconds = Some(448.93);
        log.timing.presolve_seconds = Some(10.16);
        log.bounds.primal = Some(302.0);
        log.bounds.dual = Some(302.0);
        log.bounds.gap = Some(0.0);
        log.tree.solutions_found = Some(4);
        log.presolve.rows_before = Some(576);
        log.presolve.rows_after = Some(487);
        log.presolve.cols_before = Some(18380);
        log.presolve.cols_after = Some(4579);
        log.cuts.insert("gomory".into(), 12);
        log.cuts.insert("mir".into(), 3);
        log.progress.push(NodeSnapshot {
            time_seconds: 0.0,
            primal: Some(553.0),
            dual: Some(302.0),
            gap: Some(0.4539),
            ..Default::default()
        });
        log.progress.push(NodeSnapshot {
            time_seconds: 0.5,
            nodes_explored: Some(38),
            primal: Some(402.0),
            dual: Some(302.0),
            gap: Some(0.249),
            event: Some(NodeEvent::Heuristic),
            ..Default::default()
        });
        log.progress.push(NodeSnapshot {
            time_seconds: 120.0,
            nodes_explored: Some(53747),
            primal: Some(302.0),
            dual: Some(302.0),
            gap: Some(0.0),
            event: Some(NodeEvent::Other("b".into())),
            ..Default::default()
        });
        log
    }

    #[test]
    fn roundtrip() {
        let orig = sample();
        let text = format!("{orig:#}");
        let back = from_text(&text).expect("parse");
        // Re-render to check idempotence.
        assert_eq!(text, format!("{back:#}"), "non-idempotent round trip");
        assert_eq!(orig.solver, back.solver);
        assert_eq!(orig.version, back.version);
        assert_eq!(orig.problem, back.problem);
        assert_eq!(orig.termination.status, back.termination.status);
        assert_eq!(orig.termination.raw_reason, back.termination.raw_reason);
        assert_eq!(orig.timing.wall_seconds, back.timing.wall_seconds);
        assert_eq!(orig.bounds.primal, back.bounds.primal);
        assert_eq!(orig.presolve.rows_before, back.presolve.rows_before);
        assert_eq!(orig.presolve.rows_after, back.presolve.rows_after);
        assert_eq!(orig.cuts, back.cuts);
        assert_eq!(orig.progress.len(), back.progress.len());
        let (a, b) = (
            orig.progress.iter().collect::<Vec<_>>(),
            back.progress.iter().collect::<Vec<_>>(),
        );
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.time_seconds, y.time_seconds);
            assert_eq!(x.nodes_explored, y.nodes_explored);
            assert_eq!(x.primal, y.primal);
            assert_eq!(x.event, y.event);
        }
    }

    #[test]
    fn magic_required() {
        assert!(matches!(
            from_text("solver: name=scip version=-"),
            Err(TextError::MissingMagic)
        ));
        assert!(matches!(
            from_text("miplog-text 99\n"),
            Err(TextError::WrongVersion(_))
        ));
    }

    #[test]
    fn empty_cuts_and_no_progress() {
        let log = SolverLog::new(Solver::Highs);
        let back = from_text(&format!("{log:#}")).unwrap();
        assert_eq!(back.solver, Solver::Highs);
        assert!(back.cuts.is_empty());
        assert_eq!(back.progress.len(), 0);
    }

    #[test]
    fn comments_are_ignored() {
        let mut t = format!("{:#}", sample());
        t.insert_str(0, "# preamble comment\n");
        t.push_str("\n# trailing\n");
        let back = from_text(&t).unwrap();
        assert_eq!(back.termination.status, Status::Optimal);
    }
}
