//! `miplog` — command-line front-end for the [`miplog`] crate.
//!
//! Reads a solver log, emits the parsed result as a human summary or JSON.
//! Output format is inferred from `--output`'s extension (`.json.gz` → gzipped
//! JSON, `.json` → JSON, else → summary). Override with `--format`.

use clap::{Parser, ValueEnum};
use miplog::{autodetect, input, output, parse as miplog_parse, Solver, SolverLog};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "miplog",
    version,
    about = "Parse MIP/LP solver log files into a unified, serde-serializable schema.",
    long_about = None,
)]
struct Cli {
    /// Solver log file to read (plain or gzipped). Use `-` for stdin.
    input: PathBuf,

    /// Write to file instead of stdout. Format inferred from extension:
    /// `.json.gz` → gzipped JSON, `.json` → JSON, else → summary.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Explicit output format (overrides extension inference).
    #[arg(short, long, value_enum)]
    format: Option<OutputFormat>,

    /// Force a specific parser instead of sniffing the log.
    #[arg(long, value_enum)]
    solver: Option<SolverArg>,

    /// Skip the B&B progress table in the summary output. The convergence
    /// sparkline is kept — it's a one-line derived view that stays useful
    /// even when you don't want the full table.
    #[arg(long)]
    no_progress: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Short, human-readable summary (default for stdout).
    Summary,
    /// Compact JSON on a single line.
    Json,
    /// Indented JSON.
    JsonPretty,
    /// Gzipped compact JSON (the `.json.gz` format).
    JsonGz,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SolverArg {
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

impl From<SolverArg> for Solver {
    fn from(a: SolverArg) -> Self {
        match a {
            SolverArg::Gurobi => Solver::Gurobi,
            SolverArg::Xpress => Solver::Xpress,
            SolverArg::Scip => Solver::Scip,
            SolverArg::Highs => Solver::Highs,
            SolverArg::Cplex => Solver::Cplex,
            SolverArg::Cbc => Solver::Cbc,
            SolverArg::Copt => Solver::Copt,
            SolverArg::Optverse => Solver::Optverse,
            SolverArg::Mosek => Solver::Mosek,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("miplog: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Read input: "-" means stdin; otherwise transparently handle .gz.
    let text = if cli.input.as_os_str() == "-" {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s)?;
        s
    } else {
        input::read_file(&cli.input)?
    };

    let log: SolverLog = match cli.solver {
        Some(s) => miplog_parse(&text, s.into())?,
        None => autodetect(&text)?,
    };

    let format = cli
        .format
        .unwrap_or_else(|| infer_format(cli.output.as_deref()));

    // Summary renders with or without the progress table depending on the
    // --no-progress flag; other formats ignore it (they always carry full data).
    let summary_str = || -> String {
        if cli.no_progress {
            format!("{}", log.summary_no_table())
        } else {
            format!("{log}")
        }
    };

    match (cli.output, format) {
        (Some(path), OutputFormat::JsonGz) => output::write_json_gz(&path, &log)?,
        (Some(path), OutputFormat::Json) => output::write_json(&path, &log)?,
        (Some(path), OutputFormat::JsonPretty) => output::write_json_pretty(&path, &log)?,
        (Some(path), OutputFormat::Summary) => std::fs::write(&path, summary_str())?,
        (None, OutputFormat::JsonGz) => {
            return Err(
                "--format json-gz requires --output (gzip to stdout is rarely useful)".into(),
            );
        }
        (None, OutputFormat::Json) => {
            serde_json::to_writer(io::stdout().lock(), &log)?;
            println!();
        }
        (None, OutputFormat::JsonPretty) => {
            serde_json::to_writer_pretty(io::stdout().lock(), &log)?;
            println!();
        }
        (None, OutputFormat::Summary) => {
            let mut out = io::stdout().lock();
            write!(out, "{}", summary_str())?;
        }
    }
    Ok(())
}

fn infer_format(out: Option<&Path>) -> OutputFormat {
    // Stdout default = summary (what humans want when they type `miplog run.log`).
    // Extension-bearing -o paths default to their natural format.
    let Some(p) = out else {
        return OutputFormat::Summary;
    };
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.ends_with(".json.gz") {
        OutputFormat::JsonGz
    } else if name.ends_with(".json") {
        OutputFormat::Json
    } else {
        OutputFormat::Summary
    }
}
