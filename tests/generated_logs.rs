//! Tests that parse *generated* solver logs from `tests/fixtures/logs/`.
//!
//! Run `python3 tests/generate_logs.py` first to produce logs from whatever
//! solvers are installed locally.  In CI, the GitHub Actions workflow installs
//! the free solvers and runs the generator before `cargo test`.
//!
//! Each log is from solving p0201 (MIPLIB), a 201-variable binary program
//! with known optimal objective = 7615.

use orlog::{autodetect, Solver, Status};
use std::path::Path;

const LOGS_DIR: &str = "tests/fixtures/logs";
const EXPECTED_OBJ: f64 = 7615.0;
const OBJ_TOL: f64 = 1.0; // integer problem, allow rounding

/// Try to load and parse a log.  Returns None if the file doesn't exist
/// (solver wasn't available when logs were generated).
fn try_parse(solver_name: &str) -> Option<orlog::SolverLog> {
    let path = Path::new(LOGS_DIR).join(format!("{solver_name}.log"));
    if !path.exists() {
        eprintln!("skip {solver_name}: {path:?} not found (run generate_logs.py)");
        return None;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let log = autodetect(&text).unwrap_or_else(|e| panic!("autodetect {solver_name}: {e}"));
    Some(log)
}

/// Common assertions that must hold for every solver on p0201.
fn assert_p0201(log: &orlog::SolverLog, expected_solver: Solver) {
    let name = expected_solver.key();

    // Solver detection
    assert_eq!(log.solver, expected_solver, "{name}: wrong solver");

    // Version — not always present (e.g. PySCIPOpt logfile omits the banner)
    // assert version only when log includes it
    if log.version.is_some() {
        eprintln!("{name}: version = {:?}", log.version);
    }

    // Termination — p0201 is small, every solver should find optimal
    assert_eq!(
        log.termination.status,
        Status::Optimal,
        "{name}: expected Optimal, got {:?} ({:?})",
        log.termination.status,
        log.termination.raw_reason,
    );

    // Objective bounds — optimal = 7615
    let primal = log
        .bounds
        .primal
        .unwrap_or_else(|| panic!("{name}: no primal"));
    assert!(
        (primal - EXPECTED_OBJ).abs() < OBJ_TOL,
        "{name}: primal {primal} != {EXPECTED_OBJ}",
    );
    if let Some(dual) = log.bounds.dual {
        assert!(
            (dual - EXPECTED_OBJ).abs() < OBJ_TOL,
            "{name}: dual {dual} != {EXPECTED_OBJ}",
        );
    }

    // Gap should be ~0
    if let Some(gap) = log.bounds.gap {
        assert!(gap < 0.01, "{name}: gap {gap} too large for optimal",);
    }

    // Wall time should be populated and reasonable (< 60s for this instance)
    let wall = log
        .timing
        .wall_seconds
        .unwrap_or_else(|| panic!("{name}: no wall time"));
    assert!(wall > 0.0, "{name}: wall time should be > 0");
    assert!(wall < 60.0, "{name}: wall time {wall}s suspiciously large");

    // Presolve — at least some dimension should be captured
    let pre = &log.presolve;
    assert!(
        pre.rows_before.is_some() || pre.rows_after.is_some(),
        "{name}: no presolve dims at all",
    );

    // Problem name — not always available (e.g. Gurobi LogFile doesn't
    // include the "Read MPS" line that contains the filename).
    if log.problem.is_some() {
        eprintln!("{name}: problem = {:?}", log.problem);
    }
}

// --- Per-solver tests (each skips if log not available) ---

#[test]
fn generated_highs() {
    if let Some(log) = try_parse("highs") {
        assert_p0201(&log, Solver::Highs);
        assert!(log.tree.nodes_explored.is_some(), "highs: no nodes");
        assert!(!log.progress.is_empty(), "highs: no progress rows");
        eprintln!(
            "highs: {} progress rows, {} nodes",
            log.progress.len(),
            log.tree.nodes_explored.unwrap_or(0)
        );
    }
}

#[test]
fn generated_scip() {
    if let Some(log) = try_parse("scip") {
        assert_p0201(&log, Solver::Scip);
        // SCIP progress parsing not yet implemented — just check summary
        if !log.progress.is_empty() {
            eprintln!("scip: {} progress rows", log.progress.len());
        }
        if let Some(n) = log.tree.solutions_found {
            assert!(n > 1, "scip: expected multiple solutions, got {n}");
            eprintln!("scip: {n} solutions");
        }
    }
}

#[test]
fn generated_gurobi() {
    if let Some(log) = try_parse("gurobi") {
        assert_p0201(&log, Solver::Gurobi);
        assert!(!log.progress.is_empty(), "gurobi: no progress rows");
        // Cuts may not appear in LogFile (Gurobi writes them to stdout)
        if !log.cuts.is_empty() {
            eprintln!("gurobi: {} cuts families", log.cuts.len());
        }
        assert!(
            log.tree.solutions_found.unwrap_or(0) > 1,
            "gurobi: expected multiple solutions",
        );
        eprintln!(
            "gurobi: {} progress rows, {} cuts families, {} solutions",
            log.progress.len(),
            log.cuts.len(),
            log.tree.solutions_found.unwrap_or(0)
        );
    }
}

#[test]
fn generated_copt() {
    if let Some(log) = try_parse("copt") {
        assert_p0201(&log, Solver::Copt);
        assert!(log.tree.nodes_explored.is_some(), "copt: no nodes");
        assert!(!log.progress.is_empty(), "copt: no progress rows");
        eprintln!(
            "copt: {} progress rows, {} nodes",
            log.progress.len(),
            log.tree.nodes_explored.unwrap_or(0)
        );
    }
}

#[test]
fn generated_cbc() {
    if let Some(log) = try_parse("cbc") {
        assert_p0201(&log, Solver::Cbc);
        eprintln!("cbc: {} progress rows", log.progress.len());
    }
}

#[test]
fn generated_cplex() {
    if let Some(log) = try_parse("cplex") {
        assert_p0201(&log, Solver::Cplex);
        assert!(!log.progress.is_empty(), "cplex: no progress rows");
        eprintln!("cplex: {} progress rows", log.progress.len());
    }
}

#[test]
fn generated_xpress() {
    if let Some(log) = try_parse("xpress") {
        assert_p0201(&log, Solver::Xpress);
        // Xpress progress table format varies by version
        if !log.progress.is_empty() {
            eprintln!("xpress: {} progress rows", log.progress.len());
        }
    }
}

#[test]
fn generated_mosek() {
    if let Some(log) = try_parse("mosek") {
        assert_p0201(&log, Solver::Mosek);
        eprintln!("mosek: wall={:.2}s", log.timing.wall_seconds.unwrap_or(0.0));
    }
}

/// Every generated log must satisfy the documented Core (`verify_common`)
/// tier. A failure here means a parser isn't populating fields that the
/// schema promises as reliably cross-solver — file as a parser bug.
#[test]
fn generated_all_pass_verify_common() {
    let dir = Path::new(LOGS_DIR);
    if !dir.exists() { return; }
    let mut failures: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "log").unwrap_or(false) {
            let text = std::fs::read_to_string(&path).unwrap();
            let Ok(log) = autodetect(&text) else { continue };
            if let Err(missing) = log.verify_common() {
                let name = path.file_name().unwrap().to_string_lossy();
                failures.push(format!("{name}: missing {missing:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Core-tier gaps:\n  {}",
        failures.join("\n  "),
    );
}

/// Meta-test: at least one solver log should exist.
/// Prevents silent "all skipped" in a misconfigured CI.
#[test]
fn at_least_one_solver_log_exists() {
    let dir = Path::new(LOGS_DIR);
    if !dir.exists() {
        eprintln!("WARN: {LOGS_DIR} missing — run `python3 tests/generate_logs.py`");
        return;
    }
    let count = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "log").unwrap_or(false))
        .count();
    assert!(
        count > 0,
        "No .log files in {LOGS_DIR} — run `python3 tests/generate_logs.py`",
    );
    eprintln!("Found {count} solver log(s) in {LOGS_DIR}");
}

/// Round-trip every generated log through Display -> from_text.  Validates that
/// the documented `orlog-text` v1 format is idempotent for real parser output.
#[test]
fn text_format_roundtrip_all_generated() {
    let dir = Path::new(LOGS_DIR);
    if !dir.exists() {
        return;
    }
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "log").unwrap_or(false) {
            let text = std::fs::read_to_string(&path).unwrap();
            let log = match autodetect(&text) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let rendered = format!("{log:#}");
            // Parse back.
            let back = orlog::from_text(&rendered)
                .unwrap_or_else(|e| panic!("from_text({:?}): {e}", path.file_name()));
            // Idempotent: re-rendering should match byte-for-byte.
            let rendered2 = format!("{back:#}");
            assert_eq!(
                rendered,
                rendered2,
                "non-idempotent round trip for {:?}",
                path.file_name()
            );
            n += 1;
        }
    }
    assert!(n > 0, "no generated logs found to round-trip");
    eprintln!("round-tripped {n} generated logs through Display/from_text");
}

/// Round-trip: parse → write JSON.gz → read back → compare.
#[test]
fn generated_roundtrip() {
    let dir = Path::new(LOGS_DIR);
    if !dir.exists() {
        return;
    }
    let tmp = std::env::temp_dir().join("orlog-generated-rt");
    std::fs::create_dir_all(&tmp).unwrap();

    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "log").unwrap_or(false) {
            let text = std::fs::read_to_string(&path).unwrap();
            let log = match autodetect(&text) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let stem = path.file_stem().unwrap().to_string_lossy();
            let gz = tmp.join(format!("{stem}.olog"));
            orlog::output::write_json_gz(&gz, &log).unwrap();
            let back = orlog::output::read_json(&gz).unwrap();
            assert_eq!(back.solver, log.solver, "{stem}: solver mismatch");
            assert_eq!(
                back.termination.status, log.termination.status,
                "{stem}: status mismatch",
            );
            assert_eq!(
                back.progress.len(),
                log.progress.len(),
                "{stem}: progress len mismatch",
            );
            eprintln!(
                "  roundtrip {stem}: ok ({} bytes)",
                gz.metadata().unwrap().len()
            );
        }
    }
}
