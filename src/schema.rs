//! Unified, solver-agnostic log schema.
//!
//! Fields are `Option<_>` because no single solver emits everything; parsers
//! fill in what they observe and leave the rest `None`. Solver-specific data
//! that doesn't fit the common vocabulary goes under [`SolverLog::other_data`].
//!
//! # Two tiers of fields
//!
//! The schema is one struct but the fields fall into two **tiers** by
//! reliability:
//!
//! 1. **Core (`verify_common`)** — fields we guarantee are populated when the
//!    solver log contains the corresponding information. Missing a Core field
//!    on a well-formed log is a parser bug. Downstream tooling can build
//!    cross-solver reports on these without defensive coding.
//!    - `solver` (trivially)
//!    - `termination.status` (non-`Unknown` for a complete run)
//!    - `timing.wall_seconds`
//!    - `bounds.primal` + `bounds.dual` when [`Status::Optimal`]
//!
//! 2. **Extended (best-effort)** — everything else. Parsers populate these
//!    when the log makes it easy, skip them when it doesn't. Missing an
//!    Extended field is not a bug. Examples: `version`, `solver_git_hash`,
//!    `cuts`, pre-presolve dims, root-LP times, simplex iterations.
//!
//! Promotion from Extended to Core happens with a minor version bump when
//! all active parsers reliably populate a field.
//!
//! [`Status::Optimal`]: Status::Optimal

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Closed set of supported solvers. Adding one requires a PR + minor version
/// bump — this gives `match` exhaustiveness and keeps the schema coherent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Solver {
    Gurobi,
    Xpress,
    Scip,
    Highs,
    Cplex,
    Cbc,
    Copt,
    Optverse,
    Mosek,
}

impl Solver {
    /// Short lowercase key. Stable — treat as part of the public API.
    pub const fn key(self) -> &'static str {
        match self {
            Solver::Gurobi => "gurobi",
            Solver::Xpress => "xpress",
            Solver::Scip => "scip",
            Solver::Highs => "highs",
            Solver::Cplex => "cplex",
            Solver::Cbc => "cbc",
            Solver::Copt => "copt",
            Solver::Optverse => "optverse",
            Solver::Mosek => "mosek",
        }
    }
}

/// Which version of `solverlog` produced this [`SolverLog`]. Captured so
/// persisted parse results can be re-validated after parser changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserInfo {
    /// Crate version (semver), e.g. "0.1.0".
    pub version: String,
    /// Short git hash of the crate build, empty string if unavailable.
    pub git_hash: String,
}

impl ParserInfo {
    /// Version + git hash of the currently-running crate.
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_hash: env!("MIPLOG_GIT_HASH").to_string(),
        }
    }
}

/// Top-level parsed representation of a solver log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverLog {
    pub solver: Solver,
    pub parser: ParserInfo,
    /// Free-form version string as reported by the log (e.g. "11.0.0").
    pub version: Option<String>,
    /// Solver git hash, when the solver emits one (SCIP's `[GitHash: ...]`,
    /// HiGHS's `git hash: ...`). Distinct from [`ParserInfo::git_hash`].
    pub solver_git_hash: Option<String>,
    /// Problem name as the solver reported it (often the input filename stem).
    pub problem: Option<String>,

    pub termination: Termination,
    pub timing: Timing,
    pub bounds: Bounds,
    pub tree: TreeStats,
    pub presolve: PresolveStats,

    /// Counts of cuts applied, keyed by solver-reported family name.
    /// (Families don't map cleanly across solvers — we preserve raw labels.)
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub cuts: BTreeMap<String, u64>,

    /// Every B&B progress-line the solver emitted, in chronological order.
    /// Stored columnar (struct-of-arrays) for compression and columnar
    /// analytics; use [`ProgressTable::iter`] for row-oriented access.
    #[serde(skip_serializing_if = "ProgressTable::is_empty", default)]
    pub progress: ProgressTable,

    /// Everything the unified schema doesn't cover. Each entry is a
    /// solver-specific or solver-specific-but-common name paired with an
    /// arbitrary JSON value. Stable names (e.g. `"scip.heuristics"`,
    /// `"scip.root_node"`) let downstream tooling pattern-match without
    /// promising cross-solver compatibility.
    ///
    /// The `Display` summary skips this field — use JSON for full fidelity.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub other_data: Vec<NamedValue>,
}

/// A named, freeform-value entry. Used in [`SolverLog::other_data`] as the
/// escape hatch for solver-specific data that doesn't fit the common schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedValue {
    pub name: String,
    pub value: serde_json::Value,
}

impl NamedValue {
    pub fn new(name: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Columnar store for B&B progress rows. Maintains an invariant: every
/// column vector has the same length. Rows are appended via [`ProgressTable::push`].
///
/// Column storage gives us:
/// * order-of-magnitude smaller size after gzip than row-oriented JSON
///   (repeated patterns in `time_seconds`, `nodes_explored`, etc. dedupe)
/// * natural shape for columnar analytics (`primal`, `dual`, `gap` as
///   time-series)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProgressTable {
    pub time_seconds: Vec<f64>,
    pub nodes_explored: Vec<Option<u64>>,
    pub primal: Vec<Option<f64>>,
    pub dual: Vec<Option<f64>>,
    pub gap: Vec<Option<f64>>,
    pub depth: Vec<Option<u32>>,
    pub lp_iterations: Vec<Option<u64>>,
    pub event: Vec<Option<NodeEvent>>,
}

impl ProgressTable {
    pub fn len(&self) -> usize {
        self.time_seconds.len()
    }
    pub fn is_empty(&self) -> bool {
        self.time_seconds.is_empty()
    }
    /// Append one row. Maintains the equal-length invariant across columns.
    pub fn push(&mut self, row: NodeSnapshot) {
        self.time_seconds.push(row.time_seconds);
        self.nodes_explored.push(row.nodes_explored);
        self.primal.push(row.primal);
        self.dual.push(row.dual);
        self.gap.push(row.gap);
        self.depth.push(row.depth);
        self.lp_iterations.push(row.lp_iterations);
        self.event.push(row.event);
    }
    /// Iterate rows as [`NodeSnapshot`] views.
    pub fn iter(&self) -> impl Iterator<Item = NodeSnapshot> + '_ {
        (0..self.len()).map(move |i| NodeSnapshot {
            time_seconds: self.time_seconds[i],
            nodes_explored: self.nodes_explored[i],
            primal: self.primal[i],
            dual: self.dual[i],
            gap: self.gap[i],
            depth: self.depth[i],
            lp_iterations: self.lp_iterations[i],
            event: self.event[i].clone(),
        })
    }
    /// The last recorded time (useful for end-of-run display).
    pub fn last_time(&self) -> Option<f64> {
        self.time_seconds.last().copied()
    }
}

/// Row view / input type for the B&B progress table. Solvers use different
/// column names for the same concepts: Gurobi `BestBd` ↔ Xpress/COPT/HiGHS
/// `BestBound` ↔ SCIP `dualbound`; `Incumbent` ↔ `BestSol` ↔ `primalbound`.
/// We collapse to one vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeSnapshot {
    /// Elapsed wall time the solver reported at this row (seconds).
    pub time_seconds: f64,
    /// Total nodes explored so far (Gurobi "Expl", Xpress "Node", COPT "Nodes").
    pub nodes_explored: Option<u64>,
    /// Best integer-feasible objective so far (primal bound).
    pub primal: Option<f64>,
    /// Best dual bound (valid lower bound on the optimal value for minimization).
    pub dual: Option<f64>,
    /// Relative gap as a fraction (0.0423 = 4.23%).
    pub gap: Option<f64>,
    /// Current search depth, when the solver reports one.
    pub depth: Option<u32>,
    /// Simplex iterations (per-node or cumulative — solver-specific).
    pub lp_iterations: Option<u64>,
    /// Optional row marker. Solvers flag noteworthy rows (incumbent update,
    /// heuristic hit, cutoff, branch by …). We normalize the common ones and
    /// stash the rest as [`NodeEvent::Other`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub event: Option<NodeEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeEvent {
    /// New incumbent found by a primal heuristic.
    Heuristic,
    /// New incumbent found during branching.
    BranchSolution,
    /// Node cut off by bound.
    Cutoff,
    /// Raw solver-specific marker that didn't map to the common set.
    Other(String),
}

// `Display` is implemented in `text.rs` and emits the human-readable summary.

impl SolverLog {
    /// Empty log for a given solver — parsers start here and fill fields in.
    pub fn new(solver: Solver) -> Self {
        Self {
            solver,
            parser: ParserInfo::current(),
            version: None,
            solver_git_hash: None,
            problem: None,
            termination: Termination::default(),
            timing: Timing::default(),
            bounds: Bounds::default(),
            tree: TreeStats::default(),
            presolve: PresolveStats::default(),
            cuts: BTreeMap::new(),
            progress: ProgressTable::default(),
            other_data: Vec::new(),
        }
    }

    /// Check that the **Core** fields (see module-level docs) are populated.
    /// Returns the list of missing field names, or `Ok(())` if all present.
    /// This is the strict tier — a well-formed log that fails this check
    /// indicates a parser gap worth filing.
    pub fn verify_common(&self) -> Result<(), Vec<&'static str>> {
        let mut missing = Vec::new();
        if self.termination.status == Status::Unknown {
            missing.push("termination.status");
        }
        if self.timing.wall_seconds.is_none() {
            missing.push("timing.wall_seconds");
        }
        // For Optimal runs, both bounds are expected (solver proved them equal).
        if self.termination.status == Status::Optimal {
            if self.bounds.primal.is_none() {
                missing.push("bounds.primal");
            }
            if self.bounds.dual.is_none() {
                missing.push("bounds.dual");
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(missing)
        }
    }
}

/// Why the solver stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Search finished and optimality was proved.
    Optimal,
    /// Problem proved infeasible.
    Infeasible,
    /// Problem proved unbounded.
    Unbounded,
    /// Problem either infeasible or unbounded (solver couldn't distinguish).
    InfeasibleOrUnbounded,
    /// Stopped by wall-time limit.
    TimeLimit,
    /// Stopped by memory limit.
    MemoryLimit,
    /// Stopped by node/iteration limit or other numeric limit.
    OtherLimit,
    /// Stopped by user (signal, callback).
    UserInterrupt,
    /// Numerical failure (ill-conditioned, unrecoverable).
    NumericalError,
    /// Parser couldn't determine a terminal status.
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Termination {
    pub status: Status,
    /// Solver-specific termination string, if any ("Time limit reached" etc.).
    pub raw_reason: Option<String>,
}

impl Termination {
    pub fn solved_to_completion(&self) -> bool {
        matches!(
            self.status,
            Status::Optimal
                | Status::Infeasible
                | Status::Unbounded
                | Status::InfeasibleOrUnbounded
        )
    }
}

/// Seconds. We standardize on wall time unless explicitly CPU time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Timing {
    pub wall_seconds: Option<f64>,
    pub cpu_seconds: Option<f64>,
    /// Time spent reading the problem file (before presolve).
    pub reading_seconds: Option<f64>,
    pub presolve_seconds: Option<f64>,
    pub root_relaxation_seconds: Option<f64>,
}

/// Objective bounds at termination. `gap` is solver-reported when available;
/// otherwise parsers may leave it `None` and let consumers compute it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Bounds {
    pub primal: Option<f64>,
    pub dual: Option<f64>,
    /// Gap as reported by the solver (as a fraction, `0.0423 = 4.23%`).
    /// Use [`Bounds::effective_gap`] to get a value derived from primal/dual
    /// when the solver didn't print one directly.
    pub gap: Option<f64>,
    /// Dual bound after the root LP (before branching). Equivalent to the
    /// first-LP objective on a minimization problem. Interesting quality
    /// signal independent of the final dual bound.
    pub root_dual: Option<f64>,
    /// Primal value of the first feasible solution, and when it was found.
    pub first_primal: Option<f64>,
    pub first_primal_time_seconds: Option<f64>,
    /// Primal-dual integral at termination. Rewards solvers that close the
    /// gap early even if they take equal total time; a direct benchmarking
    /// metric. Gurobi 11+ and SCIP 10+ report it natively.
    pub primal_dual_integral: Option<f64>,
}

impl Bounds {
    /// Reported gap if present, otherwise the gap derived from primal/dual
    /// using Gurobi's convention: `|primal - dual| / max(1e-10, |primal|)`.
    /// Returns `None` only if primal **and** dual are missing.
    pub fn effective_gap(&self) -> Option<f64> {
        if let Some(g) = self.gap {
            return Some(g);
        }
        match (self.primal, self.dual) {
            (Some(p), Some(d)) => Some((p - d).abs() / p.abs().max(1e-10)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TreeStats {
    pub nodes_explored: Option<u64>,
    pub simplex_iterations: Option<u64>,
    pub solutions_found: Option<u64>,
    /// Maximum depth reached in the B&B tree.
    pub max_depth: Option<u32>,
    /// Number of solver restarts (SCIP calls these "runs"; Gurobi occasionally
    /// does an internal restart; most solvers default to 0/1).
    pub restarts: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresolveStats {
    pub rows_before: Option<u64>,
    pub cols_before: Option<u64>,
    pub nonzeros_before: Option<u64>,
    pub rows_after: Option<u64>,
    pub cols_after: Option<u64>,
    pub nonzeros_after: Option<u64>,
}
