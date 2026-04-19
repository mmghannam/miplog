//! Python bindings (enabled via the `python` Cargo feature, built with maturin).
//!
//! Returns parsed logs as plain Python dicts (via `pythonize`) so callers get
//! native dict/list access without needing per-type wrapper classes.

use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pythonize::pythonize;

use crate::{autodetect, input, parse, Solver};

/// Parse the contents of a solver log. Solver is auto-detected.
#[pyfunction]
#[pyo3(signature = (text))]
fn parse_text(py: Python<'_>, text: &str) -> PyResult<PyObject> {
    let log = autodetect(text).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(pythonize(py, &log)
        .map_err(|e| PyValueError::new_err(e.to_string()))?
        .into())
}

/// Parse a solver log file (plain or gzipped). Solver is auto-detected unless
/// `solver` is given (one of "gurobi", "xpress", "scip", "highs", "cplex",
/// "cbc", "copt", "optverse", "mosek").
#[pyfunction]
#[pyo3(signature = (path, solver = None))]
fn parse_file(py: Python<'_>, path: &str, solver: Option<&str>) -> PyResult<PyObject> {
    let text = input::read_file(path).map_err(|e| PyIOError::new_err(e.to_string()))?;
    let log = match solver {
        None => autodetect(&text).map_err(|e| PyValueError::new_err(e.to_string()))?,
        Some(s) => {
            let solver = solver_from_str(s)?;
            parse(&text, solver).map_err(|e| PyValueError::new_err(e.to_string()))?
        }
    };
    Ok(pythonize(py, &log)
        .map_err(|e| PyValueError::new_err(e.to_string()))?
        .into())
}

/// Split a Mittelmann-style concatenated log into one entry per instance.
/// Each entry is a dict with `instance` and `text`. Feed `text` back into
/// `parse_text` to get the per-instance parsed log.
#[pyfunction]
fn split_concatenated(py: Python<'_>, text: &str) -> PyResult<PyObject> {
    let entries = input::split_concatenated(text);
    Ok(pythonize(py, &entries)
        .map_err(|e| PyValueError::new_err(e.to_string()))?
        .into())
}

fn solver_from_str(s: &str) -> PyResult<Solver> {
    match s.to_lowercase().as_str() {
        "gurobi" => Ok(Solver::Gurobi),
        "xpress" => Ok(Solver::Xpress),
        "scip" => Ok(Solver::Scip),
        "highs" => Ok(Solver::Highs),
        "cplex" => Ok(Solver::Cplex),
        "cbc" => Ok(Solver::Cbc),
        "copt" => Ok(Solver::Copt),
        "optverse" => Ok(Solver::Optverse),
        "mosek" => Ok(Solver::Mosek),
        other => Err(PyValueError::new_err(format!("unknown solver: {other}"))),
    }
}

#[pymodule]
fn miplog(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_text, m)?)?;
    m.add_function(wrap_pyfunction!(parse_file, m)?)?;
    m.add_function(wrap_pyfunction!(split_concatenated, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
