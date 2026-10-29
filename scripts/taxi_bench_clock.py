"""Use monotonic timing in the pinned cocotb regression manager only."""
import time
from types import SimpleNamespace


def configure(_argv):
    import cocotb
    from cocotb import regression

    if cocotb.__version__ != '2.0.1' or regression.time is not time:
        raise RuntimeError('Taxi timing hook requires unmodified cocotb 2.0.1')
    # The pinned manager only calls time.time() on this module global. Do not
    # mutate the stdlib time module: stimulus and other libraries keep their
    # original clocks. Start/end boundaries and XML generation stay in cocotb.
    regression.time = SimpleNamespace(time=time.perf_counter)
    print('TAXI_BENCH_CLOCK=perf_counter', flush=True)
