"""Verify the pinned cocotb manager uses its native monotonic clock."""
import time

SUPPORTED_COCOTB = '2.1.0'


def configure():
    import cocotb
    from cocotb import regression

    if cocotb.__version__ != SUPPORTED_COCOTB or regression.time is not time:
        raise RuntimeError(f'Taxi timing requires unmodified cocotb {SUPPORTED_COCOTB}')
    # cocotb 2.1.0 measures test and regression durations with time.monotonic().
    # Keep its clock and test boundaries intact; the runner requires this
    # marker before accepting XML timings from either simulator.
    print('TAXI_BENCH_CLOCK=cocotb-2.1.0-monotonic', flush=True)
