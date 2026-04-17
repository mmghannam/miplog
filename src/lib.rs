//! `miplog` — parse MIP/LP solver logs into a unified, serde-serializable schema.
//!
//! ```no_run
//! use miplog::{parse, Solver};
//! let text = std::fs::read_to_string("run.log").unwrap();
//! let log = parse(&text, Solver::Gurobi).unwrap();
//! println!("{}", serde_json::to_string_pretty(&log).unwrap());
//! ```

pub mod input;
pub mod output;
pub mod schema;
pub mod solvers;
pub mod text;

pub use schema::*;
pub use text::{from_text, TextError};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("log appears empty or truncated")]
    Empty,
    #[error("log does not look like a {0} log")]
    WrongSolver(&'static str),
    #[error("{0}")]
    Other(String),
}

/// Implemented by per-solver parsers.
///
/// A parser is stateless — it inspects the text and returns a [`SolverLog`].
/// Implementors should be defensive: partial or truncated logs must still
/// return a best-effort [`SolverLog`] with `Unknown` status rather than error.
pub trait LogParser {
    /// The [`Solver`] this parser handles.
    fn solver(&self) -> Solver;

    /// Cheap heuristic: does this text look like a log this parser handles?
    /// Used by [`autodetect`] and for error messages.
    fn sniff(&self, text: &str) -> bool;

    fn parse(&self, text: &str) -> Result<SolverLog, ParseError>;
}

/// Parse with a specific parser.
pub fn parse(text: &str, solver: Solver) -> Result<SolverLog, ParseError> {
    if text.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    match solver {
        Solver::Gurobi => solvers::gurobi::GurobiParser.parse(text),
        Solver::Xpress => solvers::xpress::XpressParser.parse(text),
        Solver::Scip => solvers::scip::ScipParser.parse(text),
        Solver::Highs => solvers::highs::HighsParser.parse(text),
        Solver::Cplex => solvers::cplex::CplexParser.parse(text),
        Solver::Cbc => solvers::cbc::CbcParser.parse(text),
        Solver::Copt => solvers::copt::CoptParser.parse(text),
        Solver::Optverse => solvers::optverse::OptverseParser.parse(text),
        Solver::Mosek => solvers::mosek::MosekParser.parse(text),
    }
}

/// Try each known parser's `sniff` and parse with the first match.
pub fn autodetect(text: &str) -> Result<SolverLog, ParseError> {
    if text.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    let candidates: &[&dyn LogParser] = &[
        &solvers::gurobi::GurobiParser,
        &solvers::xpress::XpressParser,
        &solvers::scip::ScipParser,
        &solvers::highs::HighsParser,
        &solvers::cplex::CplexParser,
        &solvers::cbc::CbcParser,
        &solvers::copt::CoptParser,
        &solvers::optverse::OptverseParser,
        &solvers::mosek::MosekParser,
    ];
    for p in candidates {
        if p.sniff(text) {
            return p.parse(text);
        }
    }
    Err(ParseError::Other("no parser recognized the log".into()))
}
