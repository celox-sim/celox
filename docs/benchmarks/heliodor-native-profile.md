# Heliodor Native Backend Profile Notes

This note records the current Heliodor `test_soc_linux_boot` profile for the
native backend and the optimization directions that were measured and rejected.

## Baseline

Reference run:

- Veryl cc boot: `65.667s`
- Veryl boot cycles: `0x009ae070 = 10,150,000`
- Veryl cc `perf stat`: `636,928,081,861` host instructions, IPC `2.02`

Celox native sample:

- 120 second timeout reached about `1,389,676` ticks
- `avg_apply_us=2.547`
- `avg_comb_us=59.893`
- Celox `perf stat` over 80 seconds: `736,078,011,950` host instructions, IPC `1.83`

The dominant gap is host instructions per simulated tick, not host IPC.
Veryl cc is about `62.8k` host instructions per tick for this benchmark.
Celox native is roughly an order of magnitude higher.

## Hot Code

Function-level JIT perf map:

- `[JIT]` code accounts for most runtime.
- `eval_comb` accounts for about `66%` of sampled cycles.

Block-level JIT perf map:

- `eval_comb.bb0`: about `23.6%`
- `eval_comb.bb432`: about `20.2%`
- `eval_comb.bb402`: about `5.1%`
- `eval_comb.bb408`: about `3.2%`

The top blocks are very large:

- `eval_comb.bb0`: about `597 KiB` of machine code
- `eval_comb.bb432`: about `501 KiB` of machine code

MIR/regalloc stats for `eval_comb`:

- MIR after optimization: about `263k` instructions
- MIR after regalloc: about `357k` instructions
- regalloc adds about `93k` MIR instructions
- stack reloads: about `41k`
- stack stores: about `21k`
- immediate reloads: about `28k`

Top block example:

- `bb0` after MIR optimization: about `82.9k` instructions
- `bb0` after regalloc: about `113k` instructions
- `bb0` stack loads: about `13.9k`
- `bb0` stack stores: about `7.8k`

## What The Hot Blocks Look Like

The beginning of `eval_comb.bb0` is mostly repeated scalar bitfield updates:

```text
load sim byte
and/or to clear or insert a bitfield
store sim byte
cmp
select const/0
or into a trigger or dirty byte
store sim byte
```

The cost is not just "full comb evaluation". Verilator-style full comb
evaluation can be fast when generated code is compact and register allocation is
good. The current Celox native path expands a large amount of scalar logic and
then spills heavily.

## Rejected Experiments

The following changes were implemented locally and measured. They should not be
reintroduced without a different safety or cost model.

### Pre-regalloc overwritten sim-store elimination

Deleting same-block sim-state stores that are overwritten before a same-block
load reduced MIR instruction count slightly, but worsened runtime.

Measured result:

- `avg_comb_us` worsened from about `59.893` to about `65.051`

Reason:

- Many stores are useful lifetime cuts.
- Removing them extends live ranges and makes regalloc/runtime worse even when
MIR instruction count drops.

### Boolean `select(cond, const, 0)` lowering

Lowering boolean selects to `neg cond; and const` reduced some constant/select
forms but worsened runtime.

Measured result:

- `avg_comb_us` worsened to about `63.617`

Reason:

- The transform adds a transient value and can increase register pressure.
- Local instruction-count reductions are not enough if live ranges get longer.

### Trigger byte pending accumulation in ISel

Accumulating `triggered_bits` byte updates in a VReg until block exit did not
materially reduce optimized MIR and worsened runtime.

Measured result:

- `avg_comb_us` worsened to about `64.364`

Reason:

- Existing MIR store/load forwarding and DCE already remove much of the obvious
redundancy.
- Keeping the byte accumulator live increases pressure.

### Post-regalloc same-block dead stack-store elimination

Deleting stack spill stores that are not loaded again in the same block is
incorrect.

Measured result:

- Heliodor runner segfaulted.

Reason:

- Stack spill slots can be reloaded in successor blocks.
- A same-block-only liveness test is not a valid proof that the spill store is
dead.

## Implications

The next optimization should be pressure-aware. Transformations that remove
stores or introduce accumulator temporaries can be counterproductive because
stores often shorten live ranges in the current allocator.

Promising directions:

- Add regalloc diagnostics that attribute each spill to the victim VReg, its
  defining instruction, next-use distance, and whether it is reloaded in the
  same block or a successor.
- Improve allocator decisions before changing high-level store scheduling.
- Prefer rematerialization and memory-operand folding that does not lengthen
  live ranges.
- Consider block-local scheduling only with a register-pressure budget.
- Treat broad store sinking and trigger-byte accumulation as unsafe until they
  are pressure-aware.

