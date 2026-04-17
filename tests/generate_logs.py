#!/usr/bin/env python3
"""Run every available solver on each fixture instance, capturing log output.

Usage:
    python tests/generate_logs.py [--out-dir tests/fixtures/logs]

Solvers that aren't installed are silently skipped.

Two fixture sets are produced:

  *.log
    Each solver on p0201 (MIPLIB) — a 201-variable binary set-partitioning
    problem. Every solver finishes in <1s but the log still triggers presolve,
    cuts, heuristics, and a short B&B tree.

  *-timelimit.log
    Each solver on glass4 (MIPLIB) with a 5-second wall-clock cap. glass4 is
    a notoriously hard 322-variable bin-packing-like instance; commercial
    solvers don't close it in 5s. This exercises the parsers' time-limit
    code paths (Status::TimeLimit, non-zero gap on termination, …).
"""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
P0201 = HERE / "fixtures" / "p0201.mps"
GLASS4 = HERE / "fixtures" / "glass4.mps"
INFEASIBLE = HERE / "fixtures" / "infeasible.mps"
TINYLP = HERE / "fixtures" / "tinylp.mps"
UNBOUNDED = HERE / "fixtures" / "unbounded.mps"


# ---------------------------------------------------------------------------
# Per-solver generators — each takes (mps_path, time_limit, log_path).
# `time_limit` is None for "let it finish".
# Returns True if a usable log was produced.
# ---------------------------------------------------------------------------

def generate_highs(mps: Path, time_limit, node_limit, log: Path) -> bool:
    # CLI first.
    args = ["highs", str(mps), "--solution_file", "/dev/null", "--log_file", str(log)]
    if time_limit:
        args += ["--time_limit", str(time_limit)]
    if node_limit:
        args += ["--mip_max_nodes", str(node_limit)]
    try:
        subprocess.run(args, capture_output=True, text=True, timeout=time_limit + 30 if time_limit else 60)
        if log.exists() and log.stat().st_size > 200:
            return True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    # Python fallback.
    try:
        import highspy
        h = highspy.Highs()
        h.setOptionValue("log_file", str(log))
        if time_limit:
            h.setOptionValue("time_limit", float(time_limit))
        if node_limit:
            h.setOptionValue("mip_max_nodes", int(node_limit))
        h.readModel(str(mps))
        h.run()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_scip(mps: Path, time_limit, node_limit, log: Path) -> bool:
    binary = os.environ.get("SCIP_BINARY", "scip")
    pre = []
    if time_limit:
        pre.append(f"set limits time {time_limit}")
    if node_limit:
        pre.append(f"set limits nodes {node_limit}")
    if pre:
        # Bundle the limit-set + read + optimize into one `-c` string. SCIP's
        # CLI treats each `-c` as a separate session script and only runs the
        # last instruction set; using `-c` plus `-f` would silently drop the
        # `-c` content (and its banner suppression).
        cmd = [binary, "-l", str(log), "-c",
               f"{' '.join(pre)} read {mps} optimize quit"]
    else:
        cmd = [binary, "-l", str(log), "-f", str(mps)]
    try:
        subprocess.run(cmd, capture_output=True, text=True, timeout=time_limit + 30 if time_limit else 60)
        if log.exists() and log.stat().st_size > 200:
            return True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    # PySCIPOpt fallback (skips banner; less informative).
    try:
        from pyscipopt import Model
        m = Model()
        m.setLogfile(str(log))
        if time_limit:
            m.setParam("limits/time", float(time_limit))
        if node_limit:
            m.setParam("limits/nodes", int(node_limit))
        m.readProblem(str(mps))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_gurobi(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        import gurobipy as gp
        m = gp.read(str(mps))
        m.setParam("LogFile", str(log))
        if time_limit:
            m.setParam("TimeLimit", float(time_limit))
        if node_limit:
            m.setParam("NodeLimit", int(node_limit))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_copt(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        import coptpy as cp
        env = cp.Envr()
        m = env.createModel()
        m.setLogFile(str(log))
        if time_limit:
            m.setParam("TimeLimit", float(time_limit))
        if node_limit:
            m.setParam("NodeLimit", int(node_limit))
        m.read(str(mps))
        m.solve()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_xpress(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        import xpress as xp
        # Initialize community license if available
        try:
            lic = Path(xp.__file__).parent / "license" / "community-xpauth.xpr"
            if lic.exists():
                xp.init(str(lic))
        except Exception:
            pass
        m = xp.problem()
        m.read(str(mps))
        m.setControl("outputlog", 1)
        m.setLogFile(str(log))
        if time_limit:
            # Xpress uses `maxtime` (negative = soft limit including output).
            m.setControl("maxtime", int(time_limit))
        if node_limit:
            m.setControl("maxnode", int(node_limit))
        m.optimize()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_cbc(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        sol = tempfile.NamedTemporaryFile(suffix=".sol", delete=False)
        sol.close()
        cmd = ["cbc", str(mps)]
        if time_limit:
            cmd += ["seconds", str(time_limit)]
        if node_limit:
            cmd += ["maxN", str(node_limit)]
        cmd += ["solve", "solution", sol.name]
        r = subprocess.run(cmd, capture_output=True, text=True,
                           timeout=time_limit + 30 if time_limit else 60)
        log.write_text(r.stdout + r.stderr)
        os.unlink(sol.name)
        return log.stat().st_size > 200
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return False


def generate_cplex(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        import cplex
        c = cplex.Cplex()
        c.read(str(mps))
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
        if time_limit:
            c.parameters.timelimit.set(float(time_limit))
        if node_limit:
            c.parameters.mip.limits.nodes.set(int(node_limit))
        c.solve()
        status_str = c.solution.get_status_string()
        try:
            obj = c.solution.get_objective_value()
        except Exception:
            obj = float("nan")
        iters = c.solution.progress.get_num_iterations()
        nodes = c.solution.progress.get_num_nodes_processed()
        f.write(f"\nMIP - {status_str}:  Objective = {obj:.10e}\n")
        f.write(f"Solution time = 0.00 sec.  Iterations = {iters}  Nodes = {nodes}\n")
        f.close()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


def generate_mosek(mps: Path, time_limit, node_limit, log: Path) -> bool:
    try:
        import mosek
        with mosek.Env() as env:
            with env.Task(0, 0) as task:
                fh = open(str(log), "w")
                task.set_Stream(mosek.streamtype.log, lambda msg: fh.write(msg))
                task.readdata(str(mps))
                if time_limit:
                    task.putdouparam(mosek.dparam.mio_max_time, float(time_limit))
                else:
                    task.putdouparam(mosek.dparam.mio_max_time, 60.0)
                task.optimize()
                fh.close()
        return log.exists() and log.stat().st_size > 200
    except Exception:
        return False


GENERATORS = {
    "highs": generate_highs,
    "scip": generate_scip,
    "gurobi": generate_gurobi,
    "copt": generate_copt,
    "xpress": generate_xpress,
    "cbc": generate_cbc,
    "cplex": generate_cplex,
    "mosek": generate_mosek,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=HERE / "fixtures" / "logs")
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    suites = [
        # (instance, time_limit, node_limit, log_suffix)
        (P0201, None, None, ""),
        # 2-second cap on glass4 — short enough that even Gurobi 12 on a fast
        # machine doesn't close it (we measured ~3.75s for full solve), so
        # every solver hits the time limit and we exercise the parser's
        # `Status::TimeLimit` + non-zero gap code paths.
        (GLASS4, 2, None, "-timelimit"),
        # 5-node cap on glass4 — exercises the node-limit / "OtherLimit"
        # code paths. Solvers that don't expose a node-limit knob via their
        # API simply produce no fixture.
        (GLASS4, None, 5, "-nodelimit"),
        # Trivially infeasible MIP — exercises `Status::Infeasible` paths
        # and ensures parsers don't blow up when there's no primal/dual.
        (INFEASIBLE, None, None, "-infeasible"),
        # Pure LP (no integer variables) — exercises the LP-only code paths
        # in solvers that handle both MIP and LP. No B&B, no incumbents,
        # no cuts; usually a much shorter log shape.
        (TINYLP, None, None, "-lp"),
        # Trivially unbounded LP — exercises `Status::Unbounded` paths.
        # Solvers vary: some detect in presolve, some at root LP, some
        # produce "infeasible or unbounded" without distinguishing.
        (UNBOUNDED, None, None, "-unbounded"),
    ]

    # Stitched after individual suites finish — wraps each solver's existing
    # fixtures with Mittelmann-style `@01 modified/<inst>.mps.gz ==========`
    # markers and `@05 N` end-of-run markers.
    def build_concat_fixtures():
        components = [
            ("p0201", "p0201.mps.gz", ".log"),
            ("glass4-tl", "glass4.mps.gz", "-timelimit.log"),
            ("glass4-nl", "glass4.mps.gz", "-nodelimit.log"),
        ]
        for solver in GENERATORS:
            parts = []
            for tag, instance_path, suffix in components:
                src = args.out_dir / f"{solver}{suffix}"
                if not src.exists():
                    continue
                parts.append(f"@01 modified/{instance_path} ===========\n")
                parts.append(src.read_text())
                parts.append("\n@05 7200\n")
            if not parts:
                continue
            out = args.out_dir / f"{solver}-concat.log"
            out.write_text("".join(parts))
            print(f"  ✓ {solver:12s}-concat  {out.stat().st_size:>8,} bytes")

    for mps, time_limit, node_limit, suffix in suites:
        if not mps.exists():
            print(f"skip {mps.name}: not found")
            continue
        bits = []
        if time_limit:
            bits.append(f"time limit {time_limit}s")
        if node_limit:
            bits.append(f"node limit {node_limit}")
        label = mps.stem + (f" ({', '.join(bits)})" if bits else "")
        print(f"\n=== {label} ===")
        generated, skipped = [], []
        for name, gen in GENERATORS.items():
            log = args.out_dir / f"{name}{suffix}.log"
            # Always start clean — most solvers append to LogFile, so leftover
            # content from prior runs would shadow whatever this invocation
            # produces (and confuse parsers that pick the first match).
            if log.exists():
                log.unlink()
            ok = gen(mps, time_limit, node_limit, log)
            # Clean up partial / too-small outputs whether `gen` claimed
            # success or not — Mosek without a license, for instance, writes
            # ~14 lines of read-only banner before erroring out.
            if not ok or (log.exists() and log.stat().st_size < 500):
                if log.exists():
                    log.unlink()
                ok = False
            if ok:
                generated.append(name)
                print(f"  ✓ {name:12s}  {log.stat().st_size:>8,} bytes")
            else:
                skipped.append(name)
                print(f"  ✗ {name:12s}  (not available or failed)")
        print(f"\nGenerated {len(generated)}/{len(GENERATORS)}: {', '.join(generated)}")
        if skipped:
            print(f"Skipped: {', '.join(skipped)}")

    print("\n=== Mittelmann-style concatenated fixtures ===")
    build_concat_fixtures()


if __name__ == "__main__":
    main()
