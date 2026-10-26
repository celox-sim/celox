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

## External resources: file I/O and VPI {#external-resources}

A design containing file I/O, `$fscanf` (including stdin), or VCD tasks can
be captured at the normal API boundary. `checkpoint()` copies state without
executing HDL; `save_state()` / `saveState()` retain their usual combinational
settling behavior. A simulation suspended mid-slot by `$stop` must still be
continued before capture.

Rust hosts can supply runtime I/O through `FileSystem`, keeping the OS-backed
`HostFileSystem` as the default:

```rust
let mut sim = Simulation::from_sv_sources(sources, "Top")
    .file_system(|| Box::new(MyHostIo::new()))
    .build()?;
let saved = sim.checkpoint()?;
// Advance the simulation, then restore both HDL and host I/O state.
sim.restore(&saved)?;
```

`MyHostIo` implements `celox::FileSystem`. The factory runs before declaration
initialization and creates an independent backend for each simulator. It handles
open/read/write/seek/close/flush, stdin and stderr; runtime `$writemem*` operations
also use it. This boundary covers runtime file tasks, not compiler source-file
loading, compile-time memory initialization, or VCD writers. It does not require
any particular filesystem or storage strategy.

Core checkpoints contain HDL memory, heap values, process state and scheduling
state. They do **not** contain effect-handler state. A design using file I/O or
VPI can take and restore a core checkpoint without asking its handlers to save
anything. Restoring only the core leaves those handlers attached and unchanged;
subsequent SV calls are interpreted by their current state.

The file-I/O handler owns HDL descriptor allocation, FD/MCD mappings, readability,
EOF flags, scan lookahead and standard-stream flags. Its injected `FileSystem`
owns the backing resources and defines their effect semantics. Rust hosts can
compose these independently at a quiescent execution boundary:

```rust,ignore
let core = simulation.checkpoint()?;
let io = simulation.file_io().snapshot();
// ... execute a scenario ...
let restored_io = simulation.file_io().prepare_restore(&io)?;
simulation.replace_file_io(restored_io);
simulation.restore(&core)?;
```

The host chooses which handlers to include, how external effects are retained,
and the persistence format of its **system checkpoint**. Core does not require
a memory filesystem, a disk overlay, or handler persistence. The default Rust
`HostFileSystem` continues to use the OS directly. Its lack of an independent
resource snapshot does not prevent core checkpoint capture or restore.

`FileSystem::restore()` prepares a replacement handler with the saved private
handle identities. This helper's contract leaves the existing handler and
external resources untouched during preparation. A host with a different effect
commit protocol can coordinate its own replacement instead. Validation and
commit order belong to that host system, not to the core checkpoint.

`SimulationCheckpoint::to_bytes()` encodes the core, including live fork
activations and heap objects, without event-code pointers. An instance built
from the same design binds saved event IDs through
`unsafe Simulation::checkpoint_from_bytes()`. This implementation format is
versioned and checks layout and accidental corruption; load only trusted,
unmodified artifacts from a compatible build. It is separate from portable
`StateFile`, which contains named HDL values and an optional schedule.

Portable state-file restore retains its existing host-resource restrictions.
System hosts can compose core and effect-handler snapshots into their own
resumable checkpoint format.

For native VPI hosts, Rust `VpiSession::save_state()` and
`NativeProgramInstance::save_state()` return the same `StateFile` format,
without evaluating HDL or dispatching callbacks. Capture between dispatch runs
or in a read-only synchronization callback for settled values. In other callback
phases, the file contains the values currently visible to VPI reads, which may
not yet be settled. The file has no simulation schedule: pending VPI writes,
callback registrations, force registrations, plugin memory, and cocotb/Python
coroutines are excluded. Applied force values are visible in the HDL data.
These files support inspection, comparison, and HDL-only initialization; they
are not resumable VPI sessions. C plugins can call
`int celox_vpi_save_state(const char *path)` under the same contract (1 for
success, 0 for failure). The path is UTF-8; an existing file is overwritten.

Opaque external component state also remains with its host. Core checkpoints
do not capture it; portable state-file APIs retain their component restrictions.

## Waveforms

VCD output keeps recording across a restore: the next `dump()` writes the
restored values as changes. A VCD file cannot go back in time, so `dump()`
throws when its timestamp is earlier than the last one written.

A `Simulation` dumps at every step, so `restore()` and `loadState()` throw,
without changing anything, when they would return to a time the VCD file has
already passed. Continue in a new file with `switchVcd()` first; this is also
how to record each scenario forked from a checkpoint in its own file:

```typescript
const sim = Simulation.fromSource(SOURCE, "Counter", { vcd: "./main.vcd" });
// ...
sim.switchVcd("./retry.vcd"); // timestamps start over in the new file
sim.restore(checkpoint);
sim.runUntil(500);
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
need not match; their saved values only seed combinational loops declared with
`true_loop`, which settle back into the saved state. A `Simulation` state file also records the time, clocks and
pending events by name, and where the design's processes are suspended or
waiting.

The `celox` command line tool inspects state files:

```bash
celox state dump before_failure.state
celox state diff native.state interpreter.state
```

`celox run` saves state files at chosen cycles or times of a SystemVerilog run
with `--save-state-on`, `--save-state-at` and `--save-state-at-time` (see
[the command line](cocotb.md#save-state-files-during-a-systemverilog-run)).

`diff` exits with status 1 when the files differ. Both commands skip
combinational signals unless you pass `--comb`. Dead store elimination at
`O2` leaves unread combinational signals unwritten, so their saved values are
stale. The state of combinational loops is held by combinational signals too,
so `diff` compares it only with `--comb`.

## Further Reading

- [Writing Tests](./writing-tests.md) -- Simulator and Simulation patterns.
- [VCD Waveform Output](./vcd.md) -- Recording waveforms.
