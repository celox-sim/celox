#!/usr/bin/env python3
"""Run unmodified cocotb upstream tests against the Celox VPI runtime."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
REVISION = Path(__file__).with_name("revision").read_text().strip()
# Sources, top and Python modules are taken from the pinned upstream Makefiles.
CASES = {
    "test_first_on_coincident_triggers": (
        ["test_cases/test_first_on_coincident_triggers/test.sv"],
        "test", "test_first_on_coincident_triggers",
    ),
    "test_logic_array_indexing": (
        ["test_cases/test_logic_array_indexing/test_logic_array_indexing.v"],
        "test_logic_array_indexing", "test_logic_array_indexing",
    ),
    "test_compare": (
        ["designs/basic_hierarchy_module/basic_hierarchy_module.v"],
        "basic_hierarchy_module", "test_compare",
    ),
    "test_multi_dimension_array": (
        ["designs/multi_dimension_array/cocotb_array_pkg.sv",
         "designs/multi_dimension_array/cocotb_array.sv"],
        "cocotb_array", "test_cocotb_array",
    ),
}
CASES["test_cocotb"] = (
    ["designs/sample_module/sample_module.sv"], "sample_module",
    ",".join((
        "test_deprecated", "test_synchronization_primitives", "test_first_combine",
        "test_tests", "test_testfactory", "test_timing_triggers", "test_scheduler",
        "test_clock", "test_edge_triggers", "test_async_coroutines", "test_async_generators",
        "test_handle", "test_logging", "pytest_assertion_rewriting", "test_queues",
        "test_sim_time_utils", "test_start_soon", "test_ci", "test_waiters", "test_task_manager",
    )),
)
DEFAULT_CASES = tuple(CASES)


def result_counts(path):
    cases = list(ET.parse(path).iter("testcase"))
    counts = {
        "passed": sum(not any(case.find(tag) is not None
                              for tag in ("failure", "error", "skipped")) for case in cases),
        "failed": sum(any(case.find(tag) is not None
                          for tag in ("failure", "error")) for case in cases),
        "skipped": sum(case.find("skipped") is not None for case in cases),
    }
    if not counts["passed"] or counts["failed"]:
        raise ValueError(f"upstream tests did not pass: {counts}")
    return counts


def run(args):
    source = args.source.resolve()
    celox = args.celox.resolve()
    python = args.python.absolute()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    summary = output / "summary.json"
    summary.unlink(missing_ok=True)
    head = subprocess.check_output(
        ["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if head != REVISION:
        raise ValueError(f"expected cocotb revision {REVISION}, found {head}")
    subprocess.run(["git", "-C", str(source), "diff", "--exit-code", "HEAD", "--", "tests"],
                   check=True, capture_output=True)
    version = subprocess.check_output(
        [str(python), "-c", "import cocotb, pytest; print(cocotb.__version__)"], text=True).strip()
    if version != "2.1.0":
        raise ValueError(f"expected cocotb 2.1.0, found {version}")
    rows = []
    for name in args.case or DEFAULT_CASES:
        sources, top, module = CASES[name]
        directory = output / name
        directory.mkdir(parents=True, exist_ok=True)
        executable = directory / "sim"
        results = directory / "results.xml"
        results.unlink(missing_ok=True)
        (directory / "run.log").unlink(missing_ok=True)
        row = {"case": name, "status": "failed"}
        try:
            with (directory / "compile.log").open("w") as log:
                subprocess.run(
                    [str(celox), "vpi", "build",
                     *(str(source / "tests" / path) for path in sources),
                     "--top", top, "--four-state", "--timescale", "1ns/1ps",
                     "--output", str(executable)],
                    check=True, stdout=log, stderr=subprocess.STDOUT, timeout=args.timeout,
                )
            environment = os.environ.copy()
            for key in ("COCOTB_TESTCASE", "COCOTB_TEST_FILTER", "COCOTB_TEST_MODULES",
                        "COCOTB_TOPLEVEL", "COCOTB_TRUST_INERTIAL_WRITES"):
                environment.pop(key, None)
            environment.update(TOPLEVEL_LANG="verilog", PYTHONPATH=str(
                source / "tests" / "test_cases" / name))
            with (directory / "run.log").open("w") as log:
                subprocess.run(
                    [str(executable), "--python", str(python), "--test-module", module,
                     "--results-file", str(results)],
                    cwd=directory, env=environment, check=True, stdout=log,
                    stderr=subprocess.STDOUT, timeout=args.timeout,
                )
            row.update(result_counts(results), status="passed")
        except (subprocess.SubprocessError, OSError, ValueError, ET.ParseError) as error:
            row["error"] = str(error)
        rows.append(row)
        print(f"{name}: {row['status']} ({directory})", flush=True)
    summary.write_text(json.dumps({"revision": head, "cases": rows}, indent=2) + "\n")
    return int(any(row["status"] != "passed" for row in rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="checkout of the pinned upstream cocotb")
    parser.add_argument("--celox", type=Path, default=ROOT / "target/debug/celox")
    parser.add_argument("--python", type=Path, default=Path(sys.executable))
    parser.add_argument("--output", type=Path, default=ROOT / "work/cocotb-results")
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    try:
        return run(args)
    except (subprocess.SubprocessError, OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    sys.exit(main())
