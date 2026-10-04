# Checkpoints

A checkpoint saves the state of a running simulation so you can return to it
later. Restore it as often as you like, into the same simulator or into another
one created from the same design. Typical uses are running many scenarios from
one post-reset state, or retrying the end of a long test without re-running the
start.

## Event-Based Simulation

```typescript
const sim = Simulator.fromSource<CounterPorts>(SOURCE, "Counter");
sim.dut.rst = 0n;
sim.tick();
sim.dut.rst = 1n;

const afterReset = sim.checkpoint();

for (const scenario of scenarios) {
  sim.restore(afterReset);
  scenario(sim);
}
```

A restore brings back every register, memory and input, and settles
combinational outputs, so `sim.dut` reads the restored values right away.

To run scenarios side by side, restore the checkpoint into another simulator:

```typescript
const fork = Simulator.fromSource<CounterPorts>(SOURCE, "Counter");
fork.restore(sim.checkpoint());
```

## Time-Based Simulation

A `Simulation` checkpoint also holds the simulation time, the registered
clocks and the pending scheduled events:

```typescript
const sim = Simulation.fromSource<CounterPorts>(SOURCE, "Counter");
sim.addClock("clk", { period: 10 });
sim.runUntil(100);

const checkpoint = sim.checkpoint();
sim.runUntil(500);

sim.restore(checkpoint);
sim.time(); // 100
```

## Limitations

- `restore()` throws when the checkpoint comes from a different design.
- `restore()` throws when VCD output is enabled, because the waveform cannot
  go back in time.
- `$display` output and assertion messages emitted after the checkpoint are
  not withdrawn. Running the same cycles again emits them again.
- Checkpoints live in memory. They cannot be saved to a file yet.

## Rust API

The Rust `Simulator` and `Simulation` offer the same operations:

```rust
let checkpoint = sim.checkpoint()?;
sim.restore(&checkpoint)?;
```

Both return a `CheckpointError` for the cases above.

## Further Reading

- [Writing Tests](./writing-tests.md) -- Simulator and Simulation patterns.
- [VCD Waveform Output](./vcd.md) -- Recording waveforms.
