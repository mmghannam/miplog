# Test fixture logs

Each `<solver>.log` is one solver's output for solving **p0201** (MIPLIB),
a 201-variable / 133-constraint binary set-partitioning instance with known
optimal objective 7615. The instance is small enough that every free
community-tier solver can handle it in under a second.

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
