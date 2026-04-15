#!/usr/bin/env python3
"""Solve p0201 (MIPLIB) with every available solver, capturing log output.

Usage:
    python tests/generate_logs.py [--out-dir tests/fixtures/logs]

Each solver writes its log to <out-dir>/<solver>.log.
Solvers that aren't installed are silently skipped.

p0201 is a 201-variable (all binary), 133-constraint set-partitioning problem.
Known optimal: 7615.  Solves in <1s on any modern solver but is large enough
to trigger presolve, cuts, heuristics, and a short B&B tree.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
MPS = HERE / "fixtures" / "p0201.mps"
EXPECTED_OBJ = 7615.0


def out_path(out_dir: Path, solver: str) -> Path:
    return out_dir / f"{solver}.log"


# ---------------------------------------------------------------------------
# Solver generators — each returns True if a usable log was produced.
# ---------------------------------------------------------------------------

def generate_highs(out_dir: Path) -> bool:
    log = out_path(out_dir, "highs")

    # CLI
    try:
        r = subprocess.run(
            ["highs", str(MPS), "--solution_file", "/dev/null",
             "--log_file", str(log)],
            capture_output=True, text=True, timeout=60,
        )
        if log.exists() and log.stat().st_size > 200:
            return True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    # Python fallback
    try:
        import highspy
        h = highspy.Highs()
        h.setOptionValue("log_file", str(log))
        h.readModel(str(MPS))
        h.run()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_scip(out_dir: Path) -> bool:
    """SCIP — prefer the CLI because it emits the banner (with version +
    GitHash) that PySCIPOpt's `setLogfile` skips. The binary path is taken
    from $SCIP_BINARY, falls back to `scip` on PATH.
    """
    log = out_path(out_dir, "scip")
    binary = os.environ.get("SCIP_BINARY", "scip")
    try:
        # Argument order matters: `-l` must precede `-f`, otherwise SCIP solves
        # *before* logging gets enabled and only the banner lands in the file.
        r = subprocess.run(
            [binary, "-l", str(log), "-f", str(MPS)],
            capture_output=True, text=True, timeout=60,
        )
        if log.exists() and log.stat().st_size > 200:
            return True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    # Fallback: PySCIPOpt. Missing version/git, but better than nothing.
    try:
        from pyscipopt import Model
        m = Model()
        m.setLogfile(str(log))
        m.readProblem(str(MPS))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_gurobi(out_dir: Path) -> bool:
    """Gurobi via gurobipy. pip package includes a restricted license
    (up to 2000 vars / 2000 constraints, no key needed)."""
    log = out_path(out_dir, "gurobi")
    try:
        import gurobipy as gp
        m = gp.read(str(MPS))
        m.setParam("LogFile", str(log))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_copt(out_dir: Path) -> bool:
    """COPT via coptpy. pip package includes a free tier
    (up to 2000 vars / 2000 constraints, no key needed)."""
    log = out_path(out_dir, "copt")
    try:
        import coptpy as cp
        env = cp.Envr()
        m = env.createModel()
        m.setLogFile(str(log))
        m.read(str(MPS))
        m.solve()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_xpress(out_dir: Path) -> bool:
    """Xpress via its Python API. pip package includes a community license
    (up to ~5000 vars / 5000 constraints, no key needed)."""
    log = out_path(out_dir, "xpress")
    try:
        import xpress as xp
        # Initialize with community license to silence warning
        try:
            lic = Path(xp.__file__).parent / "license" / "community-xpauth.xpr"
            if lic.exists():
                xp.init(str(lic))
        except Exception:
            pass
        m = xp.problem()
        m.read(str(MPS))
        m.setControl("outputlog", 1)
        m.setLogFile(str(log))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_cbc(out_dir: Path) -> bool:
    """CBC via CLI (apt install coinor-cbc)."""
    log = out_path(out_dir, "cbc")
    try:
        sol = tempfile.NamedTemporaryFile(suffix=".sol", delete=False)
        sol.close()
        r = subprocess.run(
            ["cbc", str(MPS), "solve", "solution", sol.name],
            capture_output=True, text=True, timeout=60,
        )
        log.write_text(r.stdout + r.stderr)
        os.unlink(sol.name)
        return log.stat().st_size > 200
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return False


def generate_cplex(out_dir: Path) -> bool:
    """CPLEX via its Python API. pip package includes a community edition
    (up to 1000 vars / 1000 constraints, no key needed)."""
    log = out_path(out_dir, "cplex")
    try:
        import cplex
        c = cplex.Cplex()
        c.read(str(MPS))
        f = open(str(log), "w")

        class W:
            def __init__(self, fh):
                self.fh = fh
            def write(self, msg):
                self.fh.write(msg)
            def flush(self):
                self.fh.flush()

        w = W(f)
        c.set_log_stream(w)
        c.set_results_stream(w)
        c.set_warning_stream(w)
        c.solve()

        # CPLEX prints the final summary to stdout, not log streams — append it
        status_str = c.solution.get_status_string()
        obj = c.solution.get_objective_value()
        iters = c.solution.progress.get_num_iterations()
        nodes = c.solution.progress.get_num_nodes_processed()
        f.write(f"\nMIP - {status_str}:  Objective = {obj:.10e}\n")
        f.write(f"Solution time = 0.00 sec.  Iterations = {iters}  Nodes = {nodes}\n")
        f.close()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_mosek(out_dir: Path) -> bool:
    """Mosek via its Python API. Requires a license file."""
    log = out_path(out_dir, "mosek")
    try:
        import mosek
        with mosek.Env() as env:
            with env.Task(0, 0) as task:
                fh = open(str(log), "w")
                task.set_Stream(mosek.streamtype.log, lambda msg: fh.write(msg))
                task.readdata(str(MPS))
                task.putintparam(mosek.iparam.mio_max_time, 60)
                task.optimize()
                fh.close()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


GENERATORS = {
    "highs":   generate_highs,
    "scip":    generate_scip,
    "gurobi":  generate_gurobi,
    "copt":    generate_copt,
    "xpress":  generate_xpress,
    "cbc":     generate_cbc,
    "cplex":   generate_cplex,
    "mosek":   generate_mosek,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=HERE / "fixtures" / "logs",
    )
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    generated = []
    skipped = []

    for name, gen in GENERATORS.items():
        ok = gen(args.out_dir)
        if ok:
            generated.append(name)
            sz = out_path(args.out_dir, name).stat().st_size
            print(f"  ✓ {name:12s}  {sz:>8,} bytes")
        else:
            skipped.append(name)
            print(f"  ✗ {name:12s}  (not available)")

    print(f"\nGenerated {len(generated)}/{len(GENERATORS)}: {', '.join(generated)}")
    if skipped:
        print(f"Skipped: {', '.join(skipped)}")

    return 0 if generated else 1


if __name__ == "__main__":
    sys.exit(main())
