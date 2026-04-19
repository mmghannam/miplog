//! Integration tests against real logs from the MIPLIB benchmark run.
//! Point `SOLVERLOG_SAMPLES` at a dir containing solver/inst.log(.gz) files
//! and this picks a few known-outcomes to smoke-test the parsers.

use miplog::{autodetect, input, output, Solver, Status};
use std::path::{Path, PathBuf};

fn samples_dir() -> Option<PathBuf> {
    std::env::var_os("SOLVERLOG_SAMPLES").map(PathBuf::from)
}

#[test]
fn smoke_known_outcomes() {
    let Some(base) = samples_dir() else { return };
    let base = base.to_string_lossy().into_owned();
    let cases = [
        ("gurobi", Solver::Gurobi, "neos5", Status::Optimal),
        (
            "gurobi",
            Solver::Gurobi,
            "fhnw-binpack4-4",
            Status::Infeasible,
        ),
        ("xpress", Solver::Xpress, "30n20b8", Status::Optimal),
    ];
    for (dir, expect_solver, inst, want) in cases {
        let plain = Path::new(&base).join(dir).join(format!("{inst}.log"));
        let gzp = Path::new(&base).join(dir).join(format!("{inst}.log.gz"));
        let text = input::read_file(&plain)
            .ok()
            .or_else(|| input::read_file(&gzp).ok());
        let Some(text) = text else {
            eprintln!("skip {dir}/{inst}: not found");
            continue;
        };
        let log = autodetect(&text).expect("parse");
        assert_eq!(log.termination.status, want, "{dir}/{inst}");
        assert_eq!(log.solver, expect_solver);
    }
}

/// Walk every *.log(.gz) in SOLVERLOG_SAMPLES and assert parsers don't
/// panic, autodetect picks the right solver, and basic fields populate.
/// Prints a coverage summary so regressions are visible.
#[test]
fn bulk_parse_all_samples() {
    let Some(base) = samples_dir() else { return };

    #[derive(Default, Debug)]
    struct Stats {
        total: usize,
        parsed: usize,
        detected_solver: usize,
        has_runtime: usize,
        has_status: usize,
        has_bounds: usize,
        has_presolve_before: usize,
        has_presolve_after: usize,
        has_progress: usize,
        total_progress_rows: usize,
        by_status: std::collections::BTreeMap<String, usize>,
    }
    let mut stats = std::collections::BTreeMap::<String, Stats>::new();

    for solver_dir in ["gurobi", "xpress"] {
        let expected = match solver_dir {
            "gurobi" => Solver::Gurobi,
            "xpress" => Solver::Xpress,
            _ => unreachable!(),
        };
        let dir = base.join(solver_dir);
        if !dir.is_dir() {
            continue;
        }
        let s = stats.entry(solver_dir.into()).or_default();
        for entry in std::fs::read_dir(&dir).expect("readdir") {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.ends_with(".log") || name.ends_with(".log.gz")) {
                continue;
            }
            s.total += 1;
            let text = match input::read_file(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let log = match autodetect(&text) {
                Ok(l) => l,
                Err(_) => continue,
            };
            s.parsed += 1;
            if log.solver == expected {
                s.detected_solver += 1;
            }
            if log.timing.wall_seconds.is_some() {
                s.has_runtime += 1;
            }
            if log.termination.status != Status::Unknown {
                s.has_status += 1;
            }
            if log.bounds.primal.is_some() || log.bounds.dual.is_some() {
                s.has_bounds += 1;
            }
            if log.presolve.rows_before.is_some() {
                s.has_presolve_before += 1;
            }
            if log.presolve.rows_after.is_some() {
                s.has_presolve_after += 1;
            }
            if !log.progress.is_empty() {
                s.has_progress += 1;
                s.total_progress_rows += log.progress.len();
                // Every row must have a time; optionals may be None.
                for r in log.progress.iter() {
                    assert!(r.time_seconds.is_finite() && r.time_seconds >= 0.0);
                }
            }
            let status_key = format!("{:?}", log.termination.status);
            *s.by_status.entry(status_key).or_default() += 1;
        }
    }

    for (solver, s) in &stats {
        eprintln!("\n=== {solver} ===");
        eprintln!("  total logs:          {}", s.total);
        eprintln!("  parsed OK:           {}/{}", s.parsed, s.total);
        eprintln!("  correct solver:      {}/{}", s.detected_solver, s.parsed);
        eprintln!("  has wall time:       {}/{}", s.has_runtime, s.parsed);
        eprintln!("  has status != Unknown: {}/{}", s.has_status, s.parsed);
        eprintln!("  has primal or dual:  {}/{}", s.has_bounds, s.parsed);
        eprintln!(
            "  has pre-presolve:    {}/{}",
            s.has_presolve_before, s.parsed
        );
        eprintln!(
            "  has post-presolve:   {}/{}",
            s.has_presolve_after, s.parsed
        );
        eprintln!(
            "  has progress rows:   {}/{}  ({} total)",
            s.has_progress, s.parsed, s.total_progress_rows
        );
        eprintln!("  by status: {:?}", s.by_status);

        // Hard invariants for real logs from completed runs.
        assert!(s.total > 0, "expected logs in {solver} dir");
        assert_eq!(s.parsed, s.total, "every log should parse ({solver})");
        assert_eq!(
            s.detected_solver, s.parsed,
            "autodetect should match ({solver})"
        );
        // Allow a small fraction unknown (e.g. license-failure logs, truncated).
        let known = s.has_status as f64 / s.parsed as f64;
        assert!(known > 0.95, "{solver}: <95% classified ({known:.2})");
    }
}

/// Round-trip a few real logs through write+read, compare JSON vs gzipped JSON
/// size. Prints sizes so we can watch the compression ratio over time.
#[test]
fn roundtrip_and_size_comparison() {
    let Some(base) = samples_dir() else { return };
    let tmp = std::env::temp_dir().join("solverlog-roundtrip");
    std::fs::create_dir_all(&tmp).unwrap();

    let mut json_total = 0u64;
    let mut gz_total = 0u64;
    let mut samples = 0;

    for solver_dir in ["gurobi", "xpress"] {
        let dir = base.join(solver_dir);
        if !dir.is_dir() {
            continue;
        }
        // Sample up to 5 files per solver to keep the test fast.
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy().into_owned();
                n.ends_with(".log") || n.ends_with(".log.gz")
            })
            .collect();
        paths.sort();
        paths.truncate(5);
        for path in paths {
            let Ok(text) = input::read_file(&path) else {
                continue;
            };
            let Ok(log) = autodetect(&text) else { continue };
            if log.progress.is_empty() {
                continue;
            }

            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let j = tmp.join(format!("{solver_dir}-{stem}.json"));
            let gz = tmp.join(format!("{solver_dir}-{stem}.json.gz"));
            output::write_json(&j, &log).unwrap();
            output::write_json_gz(&gz, &log).unwrap();

            let js = std::fs::metadata(&j).unwrap().len();
            let gs = std::fs::metadata(&gz).unwrap().len();
            json_total += js;
            gz_total += gs;
            samples += 1;

            // Round-trip: read back identical.
            let back = output::read_json(&gz).unwrap();
            assert_eq!(back.progress.len(), log.progress.len(), "roundtrip len");
            assert_eq!(
                back.termination.status, log.termination.status,
                "roundtrip status"
            );
        }
    }

    if samples > 0 {
        eprintln!(
            "\nRound-trip sizes over {samples} logs: JSON={}KB  JSON.gz={}KB  ratio={:.1}x",
            json_total / 1024,
            gz_total / 1024,
            json_total as f64 / gz_total.max(1) as f64,
        );
    }
}

/// Display smoke-test: pretty-print survives for real logs.
#[test]
fn display_renders() {
    let Some(base) = samples_dir() else { return };
    for solver_dir in ["gurobi", "xpress"] {
        let dir = base.join(solver_dir);
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).unwrap().flatten().take(3) {
            let path = entry.path();
            let n = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(n.ends_with(".log") || n.ends_with(".log.gz")) {
                continue;
            }
            let text = input::read_file(&path).unwrap();
            let log = autodetect(&text).unwrap();
            let rendered = format!("{log}");
            assert!(
                rendered.contains(log.solver.key()),
                "Display must include solver key"
            );
            eprintln!("--- {solver_dir}/{n} ---\n{rendered}\n");
        }
    }
}
