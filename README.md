# orlog

Parse MIP/LP solver log files into a unified, serde-serializable schema.

When you benchmark optimization solvers, every solver emits its results in a
different format — different column names for the same concepts, different
phrases for the same termination status, different units. `orlog` reads those
logs and gives you back one consistent Rust structure.

The unified on-disk form is called **`.olog`** (gzipped JSON of a `SolverLog`).

## Status

Very early. Parsers: **SCIP 10–11**, **Gurobi 11–13**, **Xpress 9**,
**HiGHS 1.12–14**, **CPLEX 12.7–12.8**, **CBC 2.9**, **COPT 8.0**,
**OptVerse 2.0**, **Mosek 11.0**. Roadmap: GLPK/GLOP, SoPlex, XOPT.

## Quickstart (CLI)

```bash
cargo install orlog

orlog run.log                       # print orlog-text v1 to stdout
orlog run.log --format json | jq    # pipe JSON into other tools
orlog run.log -o run.olog           # archive as gzipped JSON (extension-inferred)
orlog run.log --no-progress         # skip the B&B progress table
cat run.log | orlog -               # stdin works
```

Output format is inferred from `-o`'s extension: `.olog` / `.json.gz` → gzipped
JSON, `.json` → JSON, anything else → orlog-text.

## Quickstart (library)

```rust
use orlog::{autodetect, input, output};

let text = input::read_file("run.log.gz")?;   // plain or gzipped, auto-detect
let log = autodetect(&text)?;                 // Solver picked from content

println!("{log}");                            // unified human summary
output::write_json_gz("run.olog", &log)?;     // archival storage
# Ok::<(), Box<dyn std::error::Error>>(())
```

Example `Display` output on a SCIP 10 run (shape is identical across solvers —
any solver's `Display` renders the same fields in the same order):

```
scip 10.0.0
  problem    : p_30n20b8
  status     : Optimal (optimal solution found)
  time       : wall=448.93s presolve=10.16s
  bounds     : primal=302.00 dual=302.00 gap=0.00%
  presolve   : rows 576 → 487  cols 18380 → 4579  nnz - → -
  tree       : nodes=- simplex_iters=- sols=4
  parsed by  : orlog 0.1.0 (abc123def456)
```

If the solver emitted a B&B progress table, it's rendered below as a compact,
columnar view — head + tail with the middle elided and every incumbent-update
row (Heuristic `H`, branch-found-solution `*`) kept:

```
  progress   : 231 rows (last at 120.00s)
        time       nodes        open          primal            dual      gap  event
        0.00           0           0               -          302.00        -
        0.00           0           2          553.00          302.00    45.4%
        0.00          38          41          402.00          302.00    24.9%  H
    … 220 rows elided …
      115.00       53600      210000         302.00          302.00     0.00%
      120.00       53747      210000         302.00          302.00     0.00%
```

## Schema

The core type is `SolverLog`:

- `solver: Solver` — closed enum (`Gurobi`, `Xpress`, `Scip`, …). Adding a
  solver is a minor-version bump, not breaking.
- `version`, `solver_git_hash`, `problem` — what the solver reports about itself
- `termination: { status: Status, raw_reason }` — `Status` is the only enum we
  check; everything else is data. Variants: `Optimal`, `Infeasible`, `Unbounded`,
  `InfeasibleOrUnbounded`, `TimeLimit`, `MemoryLimit`, `OtherLimit`,
  `UserInterrupt`, `NumericalError`, `Unknown`.
- `timing` — `wall_seconds`, `cpu_seconds`, `reading_seconds`, `presolve_seconds`,
  `root_relaxation_seconds`
- `bounds` — `primal`, `dual`, `gap` (as a fraction, `0.0423 = 4.23%`)
- `tree` — `nodes_explored`, `simplex_iterations`, `solutions_found`
- `presolve` — row/col/nonzero counts before and after
- `cuts: BTreeMap<String, u64>` — freeform per-family cut counts
  (solver-specific taxonomies don't map cleanly, we preserve raw labels)
- `progress: ProgressTable` — **columnar** B&B progress (see below)
- `extras: Option<serde_json::Value>` — escape hatch for anything
  solver-specific that doesn't fit the common vocabulary
- `parser: ParserInfo { version, git_hash }` — captures which build of
  `orlog` produced the parse, so persisted `.olog` files stay reproducible
  across parser changes

Every field except `solver` and `parser` is `Option<_>` or otherwise skippable.
No solver emits everything; parsers fill what they see.

### Progress table (columnar)

`ProgressTable` stores B&B progress lines as parallel columns
(struct-of-arrays), not row-of-structs. That gives us:

- **Massive gzip compression** — the monotonic `time_seconds` column, the null
  patterns in `lp_iterations`, etc. dedupe to almost nothing.
- **Columnar analytics** — `log.progress.primal.iter().zip(&log.progress.dual)`
  is the natural shape for computing gap-over-time or incumbent plots.

Row-oriented access is available via `log.progress.iter() -> NodeSnapshot`:

```rust
# let log = orlog::SolverLog::new(orlog::Solver::Gurobi);
for row in log.progress.iter() {
    println!("{:>6.1}s  primal={:?}  dual={:?}", row.time_seconds, row.primal, row.dual);
}
```

Each row's `event: Option<NodeEvent>` normalizes markers like Gurobi's `H` / `*`
to `NodeEvent::Heuristic` / `BranchSolution`; unknown markers end up as
`NodeEvent::Other(String)` preserving the raw character.

## Input handling

- **Single file, plain or gzipped**: `input::read_file(path)`
- **Concatenated logs** (Mittelmann benchmark style — all 240 instances in
  one file, `@01 modified/X.mps.gz ===========` delimiters):
  `input::split_concatenated(text)` returns `Vec<ConcatEntry { instance, text }>`
  that you feed to `autodetect`.
- **Roadmap**: folder walking, zip archives, tar.gz streams.

## Output / storage — the `.olog` format

`.olog` is gzipped JSON of a `SolverLog`. That's it — we deliberately kept it
as serde-friendly JSON rather than a binary format so it's human-inspectable
(`zcat run.olog | jq`) while still compressing well via the columnar progress
layout.

- `output::write_json(path, log)` — compact single-line JSON
- `output::write_json_pretty(path, log)` — indented
- `output::write_json_gz(path, log)` — gzip-compressed `.olog` (recommended)
- `output::read_json(path)` — auto-detects `.gz` / `.olog`

Binary formats (`postcard`, `bincode`, `ciborium`) aren't pulled in by default;
because everything is `serde`-derived they're a one-line addition behind a
feature flag when needed.

## Adding a solver

Implement the `LogParser` trait:

```rust,ignore
pub trait LogParser {
    fn solver(&self) -> Solver;
    fn sniff(&self, text: &str) -> bool;
    fn parse(&self, text: &str) -> Result<SolverLog, ParseError>;
}
```

Then add your `Solver` enum variant and register the parser in `autodetect`.
Be defensive — partial or truncated logs should return a best-effort
`SolverLog` with `Status::Unknown` rather than erroring.

Schema philosophy: if the new solver has a concept that maps cleanly onto the
common vocabulary, populate that field. If it's genuinely solver-specific,
stash it in `extras: Value`. If two or more solvers share a solver-specific
concept, that's the moment to promote it to the common schema.

## Testing

Unit tests live inline. Integration tests walk a log directory pointed to by
`$SOLVERLOG_SAMPLES` — the logs themselves are **never committed**
(third-party license status is unclear). Example:

```bash
SOLVERLOG_SAMPLES=/path/to/your/logs cargo test
```

Without the variable, integration tests skip silently.

## License

MIT OR Apache-2.0.
