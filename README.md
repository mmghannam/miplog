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

Sample summary output:

```
solver: scip 11.0.0
problem: p0201
status: optimal in 0.55s
obj: 7615
sols: 13
presolve: 133→107 rows, 201→183 cols
convergence: ██▄▄▄▄▂▂▂▂▂▂▂▂▁▁▁▁▁▁
```

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
