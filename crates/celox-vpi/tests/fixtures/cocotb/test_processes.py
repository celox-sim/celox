"""SV processes of the DUT run in step with cocotb (#1730)."""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import Edge, ReadOnly, RisingEdge, Timer


def trace(dut, message):
    dut._log.info("TRACE %s", message)


@cocotb.test()
async def internal_clock_edges_precede_nba(dut):
    """A clock the DUT generates is seen before its registers update."""
    for _ in range(3):
        await RisingEdge(dut.clk)
        at_edge = int(dut.count.value)
        await ReadOnly()
        after = int(dut.count.value)
        trace(dut, f"t={cocotb.utils.get_sim_time('ns')} edge={at_edge} after={after}")
        assert after == at_edge + 1


@cocotb.test()
async def dut_waits_for_a_cocotb_event(dut):
    """A DUT process waits for an edge cocotb writes, then delays."""
    dut.go.value = 0
    await Timer(20, unit="ns")
    dut.go.value = 1
    written = cocotb.utils.get_sim_time("ns")
    await RisingEdge(dut.ack)
    trace(dut, f"ack {cocotb.utils.get_sim_time('ns') - written} after go")
    assert cocotb.utils.get_sim_time("ns") == written + 3


@cocotb.test()
async def cocotb_clock_drives_a_dut_process(dut):
    """A DUT process with a delay runs on a clock cocotb drives."""
    dut.tick.value = 0
    cocotb.start_soon(Clock(dut.tick, 10, unit="ns").start())
    for _ in range(3):
        await RisingEdge(dut.tick)
        edge = cocotb.utils.get_sim_time("ns")
        await Edge(dut.delayed)
        trace(dut, f"delayed {cocotb.utils.get_sim_time('ns') - edge} after tick")
        assert cocotb.utils.get_sim_time("ns") == edge + 1


@cocotb.test()
async def data_changes_are_seen_settled(dut):
    """Celox reports a value change of a register after the slot has settled,
    so its combinational readers already show the new value. That is an
    implementation choice: IEEE 1800-2023 4.7 leaves the order open, and
    Icarus reports the change before `twice` follows."""
    for _ in range(3):
        await Edge(dut.count)
        count = int(dut.count.value)
        settled = int(dut.twice.value) == 2 * count
        dut._log.info("settled at value change: %s", settled)
        if cocotb.SIM_NAME == "Celox":
            assert settled
