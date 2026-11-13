"""Distinguish the clock edge from the subsequent nonblocking update."""

import cocotb
from cocotb.triggers import ReadOnly, RisingEdge, Timer


@cocotb.test()
async def edge_precedes_nba(dut):
    dut.clk.value = 0
    dut.d.value = 1

    async def drive():
        await Timer(10, unit="ns")
        dut.clk.value = 1

    cocotb.start_soon(drive())
    await RisingEdge(dut.clk)
    at_edge = int(dut.q.value)
    await ReadOnly()
    after_nba = int(dut.q.value)
    dut._log.info("at RisingEdge: q=%d; at ReadOnly: q=%d", at_edge, after_nba)
    assert (at_edge, after_nba) == (0, 1)
