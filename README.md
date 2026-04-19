# miplog

Parse MIP/LP solver log files into a unified, machine-readable format.

Every solver writes its log differently — different column names, different ways
to say "optimal", different units. `miplog` reads any supported solver's log
and gives you back the same shape, so you can analyze runs across solvers
without writing one parser per format.

**Supported solvers:** SCIP 10–11, Gurobi 11–13, Xpress 9, HiGHS 1.12–14,
CPLEX 12.7–12.8, CBC 2.9, COPT 8.0, OptVerse 2.0, Mosek 11.0.

## Python

```bash
pip install miplog
```

```python
import miplog

log = miplog.parse_file("run.log.gz")   # plain or gzipped, solver auto-detected
print(log["solver"], log["termination"]["status"])
print(f"obj = {log['bounds']['primal']}, gap = {log['bounds']['gap']}")

# B&B progress is stored columnar — drop straight into pandas/numpy
import pandas as pd
df = pd.DataFrame(log["progress"])
```

Available functions: `parse_file(path, solver=None)`, `parse_text(text)`,
`split_concatenated(text)` for Mittelmann-style bundled runs.

## Command line

Requires the Rust toolchain ([install via rustup.rs](https://rustup.rs/)):

```bash
cargo install miplog
```

```bash
miplog run.log                    # human-readable summary
miplog run.log --format json      # machine-readable JSON
miplog run.log -o run.json.gz     # compressed archive (extension-inferred)
cat run.log | miplog -            # stdin works too
```

Sample output (a SCIP run hitting a time limit on `glass4`):

```
solver: scip 11.0.0
problem: glass4
status: time-limit in 2.00s
primal: 4350038500
dual: 800004879.356464
gap: 443.75%
sols: 2
presolve: 396→393 rows, 322→317 cols
convergence: ████████████████████

       time     nodes         primal           dual     gap  event
       0.00         1              -     8.000024e8       -
       0.00         1     4.450042e9     8.000024e8  456.2%  H
    … same for 4 more rows …
       0.00         1     4.450042e9     8.000031e8  456.2%
       0.10         1     4.450042e9     8.000033e8  456.2%
       0.10         1     4.450042e9     8.000044e8  456.2%
       0.10         1     4.450042e9     8.000046e8  456.2%
    … same for 5 more rows …
       0.60         1     4.450042e9     8.000049e8  456.2%
       0.90         1     4.350038e9     8.000049e8  443.8%  H
    … same for 3 more rows …
       1.70         1     4.350038e9     8.000049e8  443.8%
```

Identical-looking rows are elided. Incumbent updates (`H` for heuristic,
`*` for branch-found solution) are always kept. Pass `--no-progress` to
suppress the table entirely.

## Rust

```rust
use miplog::{autodetect, input};

let text = input::read_file("run.log.gz")?;  // plain or gzipped
let log = autodetect(&text)?;                // solver auto-detected
println!("{log}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Full API reference: **[docs.rs/miplog](https://docs.rs/miplog)**.

## License

MIT OR Apache-2.0.
