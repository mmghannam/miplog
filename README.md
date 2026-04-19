# miplog

A command-line tool to parse MIP/LP solver log files into a unified, machine-readable format.

Every solver writes its log differently — different column names, different ways
to say "optimal", different units. `miplog` reads any supported solver's log
and gives you back the same JSON shape, so you can analyze runs across solvers
without writing one parser per format.

**Supported solvers:** SCIP 10–11, Gurobi 11–13, Xpress 9, HiGHS 1.12–14,
CPLEX 12.7–12.8, CBC 2.9, COPT 8.0, OptVerse 2.0, Mosek 11.0.

## Install

`miplog` is currently distributed as a Rust crate. You need the Rust toolchain
([rustup.rs](https://rustup.rs/)) installed once, then:

```bash
cargo install miplog
```

This puts a single self-contained `miplog` binary on your `$PATH`. Pre-built
binaries and `pip install` are on the roadmap.

## Use it from the command line

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

## Use it from Python

Pipe the JSON output into your existing pipeline — no Python bindings needed:

```python
import json, subprocess

result = subprocess.run(
    ["miplog", "run.log", "--format", "json"],
    capture_output=True, text=True, check=True,
)
log = json.loads(result.stdout)

print(log["solver"], log["termination"]["status"])
print(f"obj = {log['bounds']['primal']}, gap = {log['bounds']['gap']}")

# B&B progress is stored columnar, so you can drop straight into pandas/numpy
import pandas as pd
df = pd.DataFrame(log["progress"])
```

## Use it from C / C++

Same pattern: invoke `miplog` as a subprocess, read JSON from stdout.
The schema is documented in the [API docs](https://docs.rs/miplog).

## Use it from Rust

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
