"""Force and release a register of a design with processes (#1730)."""

import cocotb
from cocotb.handle import Force, Release
from cocotb.triggers import ReadOnly, RisingEdge, Timer


@cocotb.test()
async def force_holds_a_register_until_release(dut):
    await RisingEdge(dut.clk)
    dut.x.value = Force(50)
    await ReadOnly()
    assert (int(dut.x.value), int(dut.y.value)) == (50, 150)
    for _ in range(2):
        await RisingEdge(dut.clk)
        await ReadOnly()
        assert (int(dut.x.value), int(dut.y.value)) == (50, 150)
    # Between two edges, a released variable keeps its value until its next
    # assignment (IEEE 1800-2023 10.6.2).
    await Timer(2, unit="ns")
    dut.x.value = Release()
    await ReadOnly()
    assert (int(dut.x.value), int(dut.y.value)) == (50, 150)
    await RisingEdge(dut.clk)
    await ReadOnly()
    assert (int(dut.x.value), int(dut.y.value)) == (51, 151)
