import cocotb
from cocotb.handle import ArrayObject
from cocotb.types import LogicArray
from cocotb.triggers import Timer, ValueChange


@cocotb.test()
async def indexes_unpacked_arrays(dut):
    assert isinstance(dut.flags, ArrayObject)
    dut.flags.value = [0, 1]
    assert isinstance(dut.words, ArrayObject)
    assert list(dut.words.range) == [3, 2]
    assert list(dut.words[2].range) == [-1, 0, 1]
    assert len(dut.words[2]) == 3
    assert isinstance(dut.words[2][0].value, LogicArray)
    assert len(dut.words[2][0]) == 8
    for row in dut.words:
        for word in row:
            word.value = 0
    dut.words[3][-1].value = 0xA5
    dut.words[2][1].value = 0x5A
    await Timer(1, unit="ns")
    assert int(dut.flag.value) == 1
    assert int(dut.flags[3].value) == 0
    assert int(dut.first.value) == 0xA5
    assert int(dut.last.value) == 0x5A
    async def expect_change():
        await ValueChange(dut.words[2][0])

    changed = cocotb.start_soon(expect_change())
    await Timer(1, unit="ns")
    dut.words[2][0].value = 0x37
    await changed
    await Timer(1, unit="ns")
    assert int(dut.middle.value) == 0x37
    assert int(dut.first.value) == 0xA5
    assert int(dut.last.value) == 0x5A
    try:
        dut.words[2][2]
    except IndexError:
        pass
    else:
        assert False, "out-of-range array index was accepted"
