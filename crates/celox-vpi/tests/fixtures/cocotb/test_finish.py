"""The design's $finish ends a cocotb run (#1730)."""

import cocotb
from cocotb.regression import SimFailure
from cocotb.triggers import Timer


@cocotb.test(expect_error=SimFailure)
async def design_finish_ends_the_run(dut):
    await Timer(100, unit="ns")
    dut._log.info("TRACE the run went on")
