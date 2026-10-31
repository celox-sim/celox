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
SAMPLE = ["designs/sample_module/sample_module.sv"]
for name, module in {
    "test_array_simple": "test_array_simple",
    "test_async_bridge": "test_async_bridge",
    "test_deadlock": "test_deadlock",
    "test_discovery": "test_discovery",
    "test_force_release": "test_force_release",
    "test_forked_exception": "test_forked_exception",
    "test_inertial_writes": "inertial_writes_tests",
    "test_one_empty_test": "test_one_empty_test",
    "test_struct": "test_struct",
    "test_trigger_memory_leak": "test_trigger_memory_leak",
    "test_xfail": "test_xfail",
    "test_multi_level_module_path": "test_package.test_module_path",
    "test_random_test_order": "test_random_test_order",
    "test_select_testcase": "x_tests,y_tests,y_tests_again",
    "test_test_filter": "x_tests",
    "test_skipped_explicitly_run": "test_skipped_explicitly_run",
}.items():
    CASES[name] = (SAMPLE, "sample_module", module)

for name, files, top, module in (
    ("test_array", ["designs/array_module/array_module.sv"], "array_module", "test_array"),
    ("test_defaultless_parameter",
     ["test_cases/test_defaultless_parameter/test_defaultless_parameter.sv"],
     "cocotb_defaultless_parameter", "test_defaultless_parameter"),
    ("test_iteration_verilog", ["test_cases/test_iteration_verilog/endian_swapper.sv"],
     "endian_swapper_sv", "test_iteration_es"),
    ("test_long_log_msg", ["test_cases/test_long_log_msg/test.sv"], "test", "test_long_log_msg"),
    ("test_package", ["test_cases/test_package/cocotb_package_pkg.sv",
                      "test_cases/test_package/cocotb_package.sv"], "cocotb_package", "test_package"),
    ("test_packed_array", ["test_cases/test_packed_array/packed_array.sv"],
     "packed_array", "test_packed_array"),
    ("test_packed_union", ["test_cases/test_packed_union/test_packed_union.sv"],
     "test_packed_union", "test_packed_union"),
    ("test_similar_scope_name", ["test_cases/test_similar_scope_name/test.sv"],
     "test", "test_similar_scope_name"),
    ("test_sv_interface", ["test_cases/test_sv_interface/top.sv"], "top", "test_sv_if"),
    ("test_verilog_include_dirs", ["test_cases/test_verilog_include_dirs/simple_and.sv"],
     "simple_and", "test_verilog_include_dirs"),
):
    CASES[name] = (files, top, module)

# Run both upstream inertial-write configurations without filtering tests.
CASES["test_inertial_writes_trust"] = CASES["test_inertial_writes"]
CASE_DIRECTORIES = {"test_inertial_writes_trust": "test_inertial_writes"}
CASE_ENVIRONMENT = {
    "test_inertial_writes": {"COCOTB_TRUST_INERTIAL_WRITES": "0"},
    "test_inertial_writes_trust": {"COCOTB_TRUST_INERTIAL_WRITES": "1"},
    "test_random_test_order": {"COCOTB_RANDOM_SEED": "1233", "COCOTB_RANDOM_TEST_ORDER": "1"},
    "test_long_log_msg": {"COCOTB_LOG_LEVEL": "DEBUG"},
    "test_select_testcase": {"COCOTB_TESTCASE": "y_test"},
    "test_test_filter": {"COCOTB_TEST_FILTER": "y_test"},
    "test_skipped_explicitly_run": {"COCOTB_TEST_FILTER": "test_skipped"},
}
CASE_INCLUDE_DIRECTORIES = {
    "test_verilog_include_dirs": ["test_cases/test_verilog_include_dirs/common",
                                  "test_cases/test_verilog_include_dirs/const_stream"],
}
CASE_BUILD_ARGUMENTS = {
    "test_force_release": ["--force-signal", "sample_module.stream_out_data_comb",
                           "--force-signal", "sample_module.stream_out_data_registered"],
}

# Unresolved groups remain runnable individually and with --all; their failures
# are reported normally, never converted to skips or expected failures.
PENDING_CASES = {
    "test_array", "test_array_simple", "test_deadlock", "test_discovery", "test_struct",
    "test_package", "test_packed_array", "test_similar_scope_name",
    "test_trigger_memory_leak",
}
DEFAULT_CASES = tuple(name for name in CASES if name not in PENDING_CASES)


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
        if name == "test_skipped_explicitly_run":
            (directory / "ran_skipped_test~").unlink(missing_ok=True)
        row = {"case": name, "status": "failed"}
        try:
            with (directory / "compile.log").open("w") as log:
                subprocess.run(
                    [str(celox), "vpi", "build",
                     *(str(source / "tests" / path) for path in sources),
                     *CASE_BUILD_ARGUMENTS.get(name, ()),
                     *(argument for path in CASE_INCLUDE_DIRECTORIES.get(name, ())
                       for argument in ("-I", str(source / "tests" / path))),
                     "--top", top, "--four-state", "--timescale", "1ns/1ps",
                     "--output", str(executable)],
                    check=True, stdout=log, stderr=subprocess.STDOUT, timeout=args.timeout,
                )
            environment = os.environ.copy()
            for key in ("COCOTB_TESTCASE", "COCOTB_TEST_FILTER", "COCOTB_TEST_MODULES",
                        "COCOTB_TOPLEVEL", "COCOTB_TRUST_INERTIAL_WRITES"):
                environment.pop(key, None)
            for key in ("COCOTB_RANDOM_SEED", "COCOTB_RANDOM_TEST_ORDER", "COCOTB_LIST_TESTS",
                        "COCOTB_MAX_FAILURES", "GPI_EXTRA", "GPI_USERS", "PYGPI_USERS"):
                environment.pop(key, None)
            environment.update(TOPLEVEL_LANG="verilog", PYTHONPATH=str(
                source / "tests" / "test_cases" / CASE_DIRECTORIES.get(name, name)))
            environment.update(CASE_ENVIRONMENT.get(name, {}))
            if "COCOTB_TESTCASE" in environment:
                warnings = environment.get("PYTHONWARNINGS", "")
                environment["PYTHONWARNINGS"] = ",".join(filter(None, (
                    warnings, "ignore::DeprecationWarning:cocotb.regression")))
            with (directory / "run.log").open("w") as log:
                subprocess.run(
                    [str(executable), "--python", str(python), "--test-module", module,
                     "--results-file", str(results)],
                    cwd=directory, env=environment, check=True, stdout=log,
                    stderr=subprocess.STDOUT, timeout=args.timeout,
                )
            row.update(result_counts(results), status="passed")
            if (name == "test_skipped_explicitly_run"
                    and not (directory / "ran_skipped_test~").is_file()):
                raise ValueError("the explicitly selected skipped test did not create its marker")
            if (name == "test_long_log_msg"
                    and "Log message construction failed" in (directory / "run.log").read_text()):
                raise ValueError("upstream long-log-message check failed")
        except (subprocess.SubprocessError, OSError, ValueError, ET.ParseError) as error:
            row.update(status="failed", error=str(error))
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
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--case", action="append", choices=CASES)
    selection.add_argument("--all", action="store_true", help="include unresolved groups; failures fail the run")
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    if args.all:
        args.case = tuple(CASES)
    try:
        return run(args)
    except (subprocess.SubprocessError, OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    sys.exit(main())
