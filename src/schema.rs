//! Unified, solver-agnostic log schema.
//!
//! Fields are `Option<_>` because no single solver emits everything; parsers
//! fill in what they observe and leave the rest `None`. Solver-specific data
//! that doesn't fit the common vocabulary goes under [`SolverLog::extras`].

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
            git_hash: env!("ORLOG_GIT_HASH").to_string(),
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

    /// Anything the unified schema doesn't cover — preserved verbatim as JSON.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extras: Option<serde_json::Value>,
}

/// Columnar store for B&B progress rows. Maintains an invariant: every
/// column vector has the same length. Rows are appended via [`push`].
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

// `Display` is implemented in `text.rs` and emits the `orlog-text` v1 format
// documented in `FORMAT.md`. Keeping it colocated with the parser keeps the
// serialization/deserialization pair honest.

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
            extras: None,
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
