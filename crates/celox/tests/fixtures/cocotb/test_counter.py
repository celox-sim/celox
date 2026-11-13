import cocotb
from cocotb.clock import Clock
from cocotb.triggers import ReadOnly, RisingEdge, Timer


@cocotb.test()
async def drives_native_flip_flop(dut):
    dut.i_clk.value = 0
    dut.i_rst.value = 0
    dut.d.value = 0x2A

    cocotb.start_soon(Clock(dut.i_clk, 10, unit="ps").start())
    await RisingEdge(dut.i_clk)
    await ReadOnly()
    assert dut.q.value == 0
    assert dut.child.q.value == 0

    await Timer(1, unit="ps")
    dut.i_rst.value = 1
    dut.d.value = 0x5A
    await RisingEdge(dut.i_clk)
    await ReadOnly()
    assert dut.q.value == 0x5A
    assert dut.child.q.value == 0x5A
