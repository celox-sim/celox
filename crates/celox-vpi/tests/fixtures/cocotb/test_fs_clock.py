"""The original 1ps SV scope can be driven by a 1fs-resolution testbench."""
import cocotb
from cocotb.clock import Clock
from cocotb.triggers import RisingEdge, Timer
from cocotb.utils import get_sim_time


@cocotb.test()
async def femtosecond_clock(dut):
    dut.d.value = 1
    clock = Clock(dut.clk, 6.206206, unit="ns")
    cocotb.start_soon(clock.start())
    await RisingEdge(dut.clk)
    first = get_sim_time(unit="fs")
    await RisingEdge(dut.clk)
    assert get_sim_time(unit="fs") - first == 6206206
    await Timer(1, unit="fs")
    assert int(dut.q.value) == 1
