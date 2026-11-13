"""Run a cocotb test module against a SystemVerilog source on Icarus.

Usage: run_icarus.py SOURCE TEST_MODULE BUILD_DIR
"""

import sys
from pathlib import Path

from cocotb_tools.runner import get_runner

source, module, build = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
runner = get_runner("icarus")
runner.build(
    sources=[source],
    hdl_toplevel="Top",
    build_dir=build,
    always=True,
    timescale=("1ns", "1ps"),
)
runner.test(
    hdl_toplevel="Top",
    test_module=module,
    build_dir=build,
    test_dir=build,
    extra_env={"PYTHONPATH": str(source.parent)},
)
