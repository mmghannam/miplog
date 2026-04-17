# miplog

Parse MIP/LP solver log files into a unified, serde-serializable schema.

When you benchmark optimization solvers, every solver emits its results in a
different format — different column names for the same concepts, different
phrases for the same termination status, different units. `miplog` reads those
logs and gives you back one consistent Rust structure.

Parsed logs are stored as gzipped JSON (`.json.gz`).

## Status

Very early. Parsers: **SCIP 10–11**, **Gurobi 11–13**, **Xpress 9**,
**HiGHS 1.12–14**, **CPLEX 12.7–12.8**, **CBC 2.9**, **COPT 8.0**,
**OptVerse 2.0**, **Mosek 11.0**. Roadmap: GLPK/GLOP, SoPlex, XOPT.

## Quickstart (CLI)

```bash
cargo install miplog

miplog run.log                       # print miplog-text v1 to stdout
miplog run.log --format json | jq    # pipe JSON into other tools
miplog run.log -o run.json.gz        # archive as gzipped JSON (extension-inferred)
miplog run.log --no-progress         # skip the B&B progress table
cat run.log | miplog -               # stdin works
```

Output format is inferred from `-o`'s extension: `.json.gz` → gzipped
JSON, `.json` → JSON, anything else → miplog-text.

## Quickstart (library)

```rust
use miplog::{autodetect, input, output};

let text = input::read_file("run.log.gz")?;   // plain or gzipped, auto-detect
let log = autodetect(&text)?;                 // Solver picked from content

println!("{log}");                            // unified human summary
output::write_json_gz("run.json.gz", &log)?;  // archival storage
# Ok::<(), Box<dyn std::error::Error>>(())
```

Example `Display` output on a SCIP 11 run (shape is identical across solvers —
any solver's `Display` renders the same fields in the same order):

```
solver: scip 11.0.0
problem: p0201
status: optimal in 0.55s
obj: 7615
sols: 13
presolve: 133→107 rows, 201→183 cols
convergence: ██▄▄▄▄▂▂▂▂▂▂▂▂▁▁▁▁▁▁
```

If the solver emitted a B&B progress table, it's rendered below as a compact
columnar view — incumbent-update rows (`H` heuristic, `*` branch solution) are
always kept; identical-looking interior rows are elided:

```
       time     nodes         primal           dual     gap  event
       0.00         1              -     8.000024e8       -
       0.00         1     4.450042e9     8.000024e8  456.2%  H
    … same for 4 more rows …
       0.90         1     4.350038e9     8.000049e8  443.8%  H
    … same for 3 more rows …
       2.00         1     4.350038e9     8.000049e8  443.8%
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
- `other_data: Vec<NamedValue>` — escape hatch for solver-specific data that
  doesn't fit the common vocabulary; each entry is `{ name, value }` where
  `value` is freeform JSON. Skipped by the text format; use JSON for full fidelity.
- `parser: ParserInfo { version, git_hash }` — captures which build of
  `miplog` produced the parse, so persisted `.json.gz` files stay reproducible
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
# let log = miplog::SolverLog::new(miplog::Solver::Gurobi);
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

## Output / storage

Parsed logs are gzipped JSON — human-inspectable (`zcat run.json.gz | jq`)
and compressing well via the columnar progress layout.

- `output::write_json(path, log)` — compact single-line JSON
- `output::write_json_pretty(path, log)` — indented
- `output::write_json_gz(path, log)` — gzip-compressed (recommended)
- `output::read_json(path)` — auto-detects `.gz`

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
stash it in `other_data`. If two or more solvers share a solver-specific
concept, that's the moment to promote it to the common schema.

## Testing

Unit tests live inline. `cargo test` runs against committed fixture logs in
`tests/fixtures/logs/` — one log per solver per scenario (optimal, time-limit,
node-limit, infeasible, LP-only, concatenated). No setup required.

For broader integration testing against real-world logs, point
`$SOLVERLOG_SAMPLES` at a directory of solver/instance logs:

```bash
SOLVERLOG_SAMPLES=/path/to/your/logs cargo test
```

Those tests skip silently when the variable is unset. The fixture logs are
regenerated via `python3 tests/generate_logs.py` (requires the solvers locally).

## License

MIT OR Apache-2.0.
