# Parallel Simulation

Celox can evaluate one design on several threads. Compilation splits the
combinational settle and the sequential update of each clock event into
*lanes*, one per thread, and the lanes run concurrently while every result
stays identical to sequential simulation.

## Enabling threads

```rust
use celox::Simulator;

let mut sim = Simulator::builder(code, "Top")
    .threads(4)
    .build()?;
```

```typescript
const sim = await Simulator.create(module, { threads: 4 });
```

`threads` counts the calling thread: `threads(4)` starts three worker threads.
The default, `1`, compiles only the sequential kernels.

## When it helps

Parallel execution pays off when one clock cycle contains a lot of
independent work, for example many instances of a datapath, wide replicated
logic, or large banks of registers. Each cycle has a fixed synchronization
cost of roughly a few hundred nanoseconds, so small designs whose cycle
already takes about a microsecond or less do not get faster.

Celox protects you from slowdowns in two ways:

1. **At compile time**, a phase is partitioned only when the estimated
   speedup outweighs the synchronization it needs. Otherwise that phase keeps
   its sequential code.
2. **At run time**, every partitioned phase is measured against its
   sequential version during the first calls, and the faster one is kept.
   The choice is measured again at intervals that double each time, so a
   choice made while the machine was busy is corrected later.

The measurement is part of the normal simulation, so it does not change any
result. Each measurement costs a few dozen calls of the slower version.

## Choosing a thread count

- Use at most the number of **physical** cores. Lanes wait for each other by
  spinning, so SMT siblings and oversubscribed cores slow everything down.
- Measure the complete workload with the same inputs and cycle count, as with
  [optimization levels](./optimization-tuning.md).
- More threads only help while each lane still has enough work per cycle.

## What runs in parallel

| Phase | Partitioned |
|---|---|
| Combinational settle (`eval_comb`) | Yes |
| Sequential update of one clock or reset event | Yes |
| Simultaneous or cascaded clock domains | Sequential |
| Native fused comb + FF tick loop | Replaced by the partitioned phases when they are faster |

Runtime events such as `$display` and assertions, and stores that can
trigger clock events, stay ordered exactly as in sequential simulation.

## Backends

Parallel execution is available on the native x86-64 and AArch64 backends
and on Cranelift. The WebAssembly backend and the interpreter always run
sequentially. Native force support for foreign interfaces keeps sequential
execution.

## Memory layout

Each lane's state lives in its own memory segment, aligned to cache lines and
separated by guard bytes, so lanes never write to the same cache line except
for objects that several lanes write by design (for example, different
elements of one array). See [Parallel Execution](/internals/parallel-simulation)
for the design.
