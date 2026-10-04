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
- `$display` output and assertion messages emitted after the checkpoint are
  not withdrawn. Running the same cycles again emits them again.
- Checkpoints live in memory. To keep state across processes, use a state
  file.

## Waveforms

VCD output keeps recording across a restore: the next `dump()` writes the
restored values as changes. A VCD file cannot go back in time, so `dump()`
throws when its timestamp is earlier than the last one written. To record a
rewound `Simulation`, or each scenario forked from a checkpoint, continue in a
new file with `switchVcd()`:

```typescript
const sim = Simulation.fromSource(SOURCE, "Counter", { vcd: "./main.vcd" });
// ...
sim.restore(checkpoint);
sim.switchVcd("./retry.vcd"); // timestamps start over in the new file
sim.dump(sim.time());
```

In Rust, `try_dump()` returns these errors instead of panicking like
`dump()`, and `switch_vcd()` starts the new file.

## Rust API

The Rust `Simulator` and `Simulation` offer the same operations:

```rust
let checkpoint = sim.checkpoint()?;
sim.restore(&checkpoint)?;
```

Both return a `CheckpointError` for the cases above.

## State Files

A state file saves the state in a binary file that records every value by
its signal path. Because it does not depend on the memory layout, a file
saved by one simulator loads into another one built with a different backend
or optimization level. This lets you, for example, capture the state of an
optimized native build just before a failure and replay it on the
interpreter at `O0`.

`saveState()` returns the file contents as bytes, and `loadState()` takes
them back:

```typescript
import { readFileSync, writeFileSync } from "node:fs";

writeFileSync("before_failure.state", sim.saveState());

const other = Simulator.fromSource<CounterPorts>(SOURCE, "Counter", {
  optLevel: "O0",
});
other.loadState(readFileSync("before_failure.state"));
```

State files are available with the native addon. The WASM build does not
support them yet.

The Rust API saves to and loads from a `StateFile`:

```rust
let file = sim.save_state()?;
file.write_to(std::fs::File::create("before_failure.state")?)?;

let file = StateFile::read_from(std::fs::File::open("before_failure.state")?)?;
other.load_state(&file)?;
```

Loading needs every register, memory and input of the design to be present
with the same width. If any is missing, nothing is changed and the error lists
the differences. Combinational signals are recomputed after loading, so they
need not match. A `Simulation` state file also records the time, clocks and
pending events by name.

The `celox` command line tool inspects state files:

```bash
celox state dump before_failure.state
celox state diff native.state interpreter.state
```

`diff` exits with status 1 when the files differ. Both commands skip
combinational signals unless you pass `--comb`. Dead store elimination at
`O2` leaves unread combinational signals unwritten, so their saved values are
stale.

## Further Reading

- [Writing Tests](./writing-tests.md) -- Simulator and Simulation patterns.
- [VCD Waveform Output](./vcd.md) -- Recording waveforms.
