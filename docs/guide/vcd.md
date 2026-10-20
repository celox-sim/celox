# VCD Waveform Output

Celox can write [Value Change Dump](https://en.wikipedia.org/wiki/Value_change_dump) (VCD) files for viewing waveforms in tools like [GTKWave](https://gtkwave.sourceforge.net/) or [Surfer](https://surfer-project.org/).

## Basic Usage

Pass `vcd` with the output file path when creating a simulator:

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  vcd: "./dump.vcd",
});
```

Then call `sim.dump(timestamp)` at each point in time you want recorded:

```typescript
sim.dut.a = 10n;
sim.dump(0);      // record initial state at t=0

sim.dut.a = 20n;
sim.tick();
sim.dump(10);     // record state at t=10

sim.dispose();    // flushes and closes the file
```

::: warning
VCD output is buffered. Always call `dispose()` when you are done so the remaining bytes are written — or use a try/finally block.
:::

In Rust, call `Simulator::flush_vcd()` or `Simulation::flush_vcd()` to make
buffered output visible before dropping the simulator and to handle write errors.
Standalone `VcdWriter` instances provide `flush()` and `into_inner()`.

The [VCD performance notes](../internals/vcd-performance.md) describe change
tracking, shared-memory behavior, and reproducible benchmarks.

## With Time-Based Simulation

With `Simulation`, `sim.time()` gives you the current time to pass to `dump()`:

```typescript
const sim = Simulation.fromSource(SOURCE, "Top", {
  vcd: "./dump.vcd",
});
sim.addClock("clk", { period: 10 });

sim.reset("rst");
sim.dump(sim.time());

sim.dut.en = 1n;
sim.runUntil(100);
sim.dump(sim.time());

sim.dispose();
```

## Further Reading

- [Writing Tests](./writing-tests.md) -- Simulator and Simulation patterns.

## SystemVerilog controls {#systemverilog-source-controls}

A timed SystemVerilog simulation can select and control tracing from HDL:

```systemverilog
initial begin
    $dumpfile("waves.vcd");
    $dumpvars(0, Top);
end
```

`$dumpvars` starts recording after the current simulation timestamp settles.
All calls must execute at the same simulation time. A level of `0` includes
all descendants; `1` includes only the named module's own variables. Individual
variables and multiple module or variable selections are supported.

`$dumpoff` records unknown values and suspends tracing. `$dumpon` records current
values and resumes it. `$dumpall` records every selected value without changing
that state. `$dumpflush` publishes buffered output, and `$dumplimit(bytes)` stops
recording at the size limit with a comment in the file. Complete records may
extend past the limit. A zero limit removes the bound.

`$dumpfile` accepts a string literal or an integral expression containing the
filename and must run before `$dumpvars`. The default filename is `dump.vcd`.
Source controls enable the necessary trace storage automatically; a host VCD
path is optional. Host waveform methods remain available.
