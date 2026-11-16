import cocotb
from cocotb.handle import Force, Release
from cocotb.triggers import Timer


@cocotb.test()
async def optimized_force_and_release(dut):
    dut.a.value = 1
    await Timer(1, unit="ps")
    assert int(dut.total.value) == 5
    dut.x.value = Force(3)
    await Timer(1, unit="ps")
    assert int(dut.x.value) == 3
    assert int(dut.total.value) == 13
    dut.x.value = Release()
    await Timer(1, unit="ps")
    assert int(dut.x.value) == 1
    assert int(dut.total.value) == 5
