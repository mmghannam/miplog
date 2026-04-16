# Test fixture logs

Each `<solver>.log` is one solver's output for solving **p0201** (MIPLIB),
a 201-variable / 133-constraint binary set-partitioning instance with known
optimal objective 7615. The instance is small enough that every free
community-tier solver can handle it in under a second.

## Two fixture suites

### `<solver>.log` — p0201 (optimal completion)

A 201-variable binary set-partitioning problem from MIPLIB. Every solver
finishes in <1s; the log still triggers presolve, cuts, heuristics, and a
short B&B tree. Used to validate the optimal-completion code paths.

### `<solver>-timelimit.log` — glass4 with a 2s wall-time cap

A 322-variable bin-packing-like instance (also MIPLIB), historically hard
for many solvers. Capped at 2 seconds so even fast solvers (Gurobi 12,
COPT 8) hit the time limit and produce a `Status::TimeLimit` run with
non-zero gap. Exercises the parsers' early-termination paths: time-limit
status detection, primal/dual bounds when not proven optimal, gap > 0.

### `<solver>-nodelimit.log` — glass4 with a 5-node cap

Same instance, capped at 5 B&B nodes via each solver's node-limit knob
(Gurobi `NodeLimit`, SCIP `limits/nodes`, HiGHS `mip_max_nodes`,
COPT `NodeLimit`, Xpress `MAXNODE`, CBC `maxN`, CPLEX `mip.limits.nodes`).
Termination strings vary per solver — `"node limit reached"` for most,
`"Solution limit reached"` for HiGHS, `STOPPING - MAXNODE …` for Xpress —
all should classify as `Status::OtherLimit`.

### `<solver>-infeasible.log` — trivially infeasible MIP

A 1-variable binary problem with `x ≥ 1` and `x ≤ 0` — every solver
detects infeasibility in presolve. Validates `Status::Infeasible`
classification and that parsers don't blow up when there's no primal
or dual to extract.

### `<solver>-concat.log` — Mittelmann-style bundled runs

Three runs of the same solver on different instances, stitched with
`@01 modified/<instance>.mps.gz ===========` start markers and
`@05 7200` end markers (the format the [Mittelmann benchmarks][m] use to
package 240 instance solves into one file per solver). Exercises
`orlog::input::split_concatenated`, which yields a `Vec<ConcatEntry>`
each parser then handles independently.

The three runs are: p0201 (Optimal), glass4 with time limit (TimeLimit),
glass4 with node limit (OtherLimit) — covers the three distinct status
classes in a single bundle.

[m]: https://plato.asu.edu/ftp/milp_log12/

### `<solver>-lp.log` — pure LP (no integer variables)

A 3-variable continuous LP with known optimum −5. Exercises the
LP-only termination paths in solvers that support both MIP and LP:
no B&B progress table, no cuts, no incumbents, just LP iterations and
a final objective. Each solver uses different wording — Gurobi
`Solved in N iterations`, COPT `Status: Optimal Objective: …`,
HiGHS `Model status: Optimal`, Xpress `Dual solved problem` /
`Final objective`, CBC `Optimal - objective value` — all classify as
`Status::Optimal` with `primal == dual` and `gap = 0`.

## Currently-committed versions

Each fixture log was produced by the solver version shown below — the
parsers in `src/solvers/*` are regression-tested against these exact
outputs, so these are the canonical "known-good" formats we support.

| Solver   | Version in fixture | Notes |
|----------|--------------------|-------|
| SCIP     | 11.0.0 (GitHash `4f4f68fb97-dirty`) | Generated via the `scip` CLI so the banner + solver git hash are captured |
| Gurobi   | 12.0.3             | Via `gurobipy` restricted license |
| Xpress   | 9.8.1 Community    | Via the `xpress` Python package |
| HiGHS    | 1.14.0 (git `7df0786`) | Via `highspy` |
| COPT     | 8.0.3              | Via `coptpy` free tier |
| CPLEX    | 22.1.2.0           | Via `cplex` community edition |

Parsers are expected to stay compatible with **at least one minor version
back and one forward** from these without code changes. Breaking format
changes in a solver should appear as a new fixture alongside the old one,
not a replacement, so we keep parsing both.

## How these were generated

```bash
python3 tests/generate_logs.py
```

Each solver generator in that script uses the solver's own Python API or CLI
to solve `tests/fixtures/p0201.mps`, writing the log file here. The script
silently skips any solver that isn't installed locally, so running on a
machine without (say) COPT just means no `copt.log` gets produced.

## Why these are committed

These logs are **our own output** — nothing downloaded or redistributed from
a third party. Every solver used was in its free community tier:

| Solver   | License used to produce the log                               |
|----------|---------------------------------------------------------------|
| HiGHS    | MIT (fully free, no tier restrictions)                        |
| SCIP     | Apache 2.0 via PySCIPOpt (fully free)                         |
| Gurobi   | `gurobipy` restricted license (≤ 2000 vars/constraints)       |
| COPT     | `coptpy` free tier (≤ 2000 vars/constraints)                  |
| Xpress   | `xpress` community license (≤ 5000 vars/constraints)          |
| CPLEX    | `cplex` community edition (≤ 1000 vars/constraints)           |

Committing them:

- Makes `cargo test` runnable without the solver install dance.
- Makes parser regressions visible in diffs (a log that got parsed differently
  after a refactor stands out because the rendered `orlog-text` will change).
- Pins the exact solver versions exercised by the test suite; regenerating
  replaces these with whatever versions happen to be installed.

## Regenerating

If you've upgraded a solver and want the fixtures to reflect the newer output
format, re-run `tests/generate_logs.py` and commit the updated logs. The CI
workflow also regenerates them on every run and uploads the result as an
artifact on failure, so mismatches surface quickly.

## Don't commit other logs here

Only p0201 logs produced by `generate_logs.py` belong in this directory.
Anything else — Mittelmann benchmark dumps, customer logs, third-party
benchmark sets — has unclear license status and must never be committed.
Point `SOLVERLOG_SAMPLES` at those from outside the repo instead.
