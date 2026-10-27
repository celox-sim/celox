"""Inertial updates must not race the FF kernel of the triggering edge."""

import os

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import ReadOnly, ReadWrite, RisingEdge, Timer, with_timeout


@cocotb.test(timeout_time=100, timeout_unit="ns")
async def inertial_updates_follow_the_current_edge(dut):
    dut.clk.value = 0
    dut.d.value = 0
    await Timer(1, unit="ns")

    # Register after the deposit. It must not miss an already applied edge.
    dut.clk.value = 1
    await with_timeout(RisingEdge(dut.clk), 5, "ns")
    await ReadOnly()
    assert int(dut.q.value) == 0
    await Timer(1, unit="ns")
    dut.clk.value = 0
    await Timer(1, unit="ns")

    clock = Clock(dut.clk, 10, unit="ns")
    expected_impl = "gpi" if int(os.environ["COCOTB_TRUST_INERTIAL_WRITES"]) else "py"
    assert clock.impl == expected_impl
    clock.start()
    for previous, new in [(0, 1), (1, 2)]:
        await RisingEdge(dut.clk)
        dut.d.value = new
        await ReadOnly()
        assert int(dut.q.value) == previous
    clock.stop()


@cocotb.test(timeout_time=100, timeout_unit="ns")
async def last_inertial_deposit_wins_before_readwrite(dut):
    dut.clk.value = 0
    dut.d.value = 0
    await Timer(1, unit="ns")
    dut.d.value = 17
    dut.d.value = 41
    await ReadWrite()
    # The legacy Python applicator itself runs in ReadWrite, so its inertial
    # update is not visible until the following update round.
    if int(os.environ["COCOTB_TRUST_INERTIAL_WRITES"]):
        assert int(dut.d.value) == 41
    dut.d.value = 73
    await ReadOnly()
    assert int(dut.d.value) == 73
