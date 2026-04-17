# miplog

Parse MIP/LP solver log files into a unified, serde-serializable schema.

Every solver emits results in a different format — different column names, different
termination phrases, different units. `miplog` reads those logs and gives you back
one consistent Rust structure.

**Parsers:** SCIP 10–11, Gurobi 11–13, Xpress 9, HiGHS 1.12–14, CPLEX 12.7–12.8,
CBC 2.9, COPT 8.0, OptVerse 2.0, Mosek 11.0.

**[API docs →](https://docs.rs/miplog)**

## CLI

```bash
cargo install miplog

miplog run.log                    # human summary to stdout
miplog run.log --format json | jq # JSON
miplog run.log -o run.json.gz     # archive (format inferred from extension)
cat run.log | miplog -            # stdin
```

```
solver: scip 11.0.0
problem: p0201
status: optimal in 0.55s
obj: 7615
sols: 13
presolve: 133→107 rows, 201→183 cols
convergence: ██▄▄▄▄▂▂▂▂▂▂▂▂▁▁▁▁▁▁
```

## Library

```rust
use miplog::{autodetect, input, output};

let text = input::read_file("run.log.gz")?;  // plain or gzipped
let log = autodetect(&text)?;                // solver auto-detected

println!("{log}");                           // human summary
output::write_json_gz("run.json.gz", &log)?; // archive
# Ok::<(), Box<dyn std::error::Error>>(())
```

## License

MIT OR Apache-2.0.
