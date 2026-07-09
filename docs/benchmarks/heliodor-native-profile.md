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

## Tenfold Gap Model

The roughly tenfold gap is too large to explain with register allocation alone.
The current data decomposes the problem as follows:

- Veryl cc boots `10,150,000` simulated cycles in `65.667s`, about `6.47 us`
  per simulated cycle.
- Celox native spends about `59.893 us` per tick in `eval_comb` alone, before
  counting `eval_apply_ff`.
- Celox `eval_comb` has about `263k` MIR instructions after normal MIR
  optimization, before regalloc.
- Regalloc then expands this to about `357k` MIR instructions.

So the allocator expansion is a serious multiplier, but removing all regalloc
overhead would not make the code competitive by itself. The native backend is
already generating several times too much scalar work before allocation.

The likely structural causes are:

- bitfield and trigger/dirty updates are lowered as many independent scalar
  load/modify/store sequences;
- constants, masks, and boolean results are represented as SSA VRegs too often
  instead of being kept as immediates, flags, or memory operands at x86 lowering
  time;
- very large merged `eval_comb` blocks create high live pressure and poor
  locality;
- regalloc then amplifies this shape by repeatedly spilling long-lived
  boolean/bitfield transients and reloading rematerialized constants.

This means the fix needs two layers: reduce the pre-regalloc scalar work, then
make allocation pressure-aware. Only doing one side is unlikely to recover the
full order of magnitude.

The hot `eval_comb.bb0` SIR block confirms that the pre-regalloc work is already
large. A dump of block 0 after SIR optimization contained `72,345` SIR
instructions. The largest instruction classes were:

- `And`: `13,068`
- `Mux`: `11,808`
- `LogicAnd`: `8,234`
- `Eq`: `6,944`
- `Shr`: `6,350`
- `Load`: `5,107`
- `Concat`: `4,607`
- `Store`: `2,020`

This is why the gap should not be described as "just regalloc". The allocator
amplifies an already oversized scalar boolean/bitfield program.

A phase-timed 45 second sample confirms the same split:

- `eval_comb` merge input: `145,104` SIR instructions
- ISel output: `495,111` MIR instructions
- MIR after optimization: `263,530` instructions
- MIR after regalloc and peepholes: `347,580` instructions
- emitted `eval_comb` machine code: `1,764,047` bytes
- runtime at timeout: `avg_comb_us=64.016`, `avg_apply_us=2.637`

The compile-time cost is visible too, especially `eval_comb` MIR optimization at
about `4.63s`, but it is not the order-of-magnitude runtime gap. The runtime gap
is already present in the per-tick hot code: `eval_comb` alone takes about ten
times the Veryl cc per-cycle time.

An opt-in priority-encoder lowering exists behind `CELOX_NATIVE_PRIORITY_ENCODE`,
but it did not help this benchmark in a same-conditions 60 second sample:

- without priority lowering: `avg_comb_us=61.508`
- with priority lowering: `avg_comb_us=62.280`

So the observed `Mux` volume is not primarily the already-recognized priority
encoder shape, or the lowering is not cost-effective for this generated code.

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

Regalloc spill/reload attribution with `CELOX_REGALLOC_TRACE=1`:

- `eval_comb` produced `91,848` regalloc spill/reload events.
- Largest group: rematerialized immediate reloads, `28,340` events.
- Largest stack reload groups:
  - `and` results: `8,484` reloads
  - `cmp_imm` results: `6,173` reloads
  - `sim_state_snapshot` loads: `4,347` reloads
  - `sim_state_snapshot` `and_imm` results: `3,981` reloads
- Largest stack spill groups:
  - `and` results with next use beyond `1024` MIR positions: `1,974` spills
  - `and` results with next use `257-1024`: `1,750` spills
  - `cmp_imm` results with next use `257-1024`: `1,611` spills

This points to two separate problems: constants are still being loaded into
registers too often after regalloc, and long-lived boolean/bitfield transient
values are being kept across large regions and later reloaded from stack.

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

### Regalloc-time rematerialized constant folding

Folding rematerialized constants into MIR immediate operands during regalloc
reduced the number of recorded regalloc events but worsened runtime.

Measured result:

- `eval_comb` regalloc events fell from `91,848` to `75,998`
- rematerialized immediate reloads fell from `28,340` to `12,605`
- `avg_comb_us` worsened from about `63.911` to about `64.565` in the
  trace-enabled 60 second sample

Reason:

- Reducing allocator events is not the same as improving generated machine code.
- This transform changes instruction selection after MIR optimization and can
  trade explicit reloads for less favorable immediate instruction forms without
  addressing the large scalar `And`/`Mux`/`LogicAnd`/`Eq` workload.
- Immediate folding should be revisited in the x86 emitter or ISel cost model,
  not as a blind regalloc rewrite.

### GVN alias algebraic simplification

GVN-time alias simplification for identities such as `x | 0`, `x ^ 0`,
`x & all_ones`, and `mux(c, x, x)` was also rejected.

Measured result:

- baseline `eval_comb` after regalloc: about `347k` MIR instructions
- with the simplification: about `354k` MIR instructions

Reason:

- Even width-safe aliasing can lengthen live ranges by deleting useful temporary
  definitions.
- On this workload, the allocator pays more for the longer live ranges than the
  optimizer saves by removing local algebraic operations.

## Implications

The next optimization should be pressure-aware. Transformations that remove
stores or introduce accumulator temporaries can be counterproductive because
stores often shorten live ranges in the current allocator.

Promising directions:

- Add regalloc diagnostics that attribute each spill to the victim VReg, its
  defining instruction, next-use distance, and whether it is reloaded in the
  same block or a successor.
- Fold rematerialized constants into immediate operands or rematerialize at the
  x86 operand level where possible, instead of materializing every use into a
  GPR before instruction selection.
- Split long-lived boolean/bitfield transients when their next use is far away,
  especially `and` and `cmp_imm` results in large `eval_comb` blocks.
- Improve allocator decisions before changing high-level store scheduling.
- Prefer rematerialization and memory-operand folding that does not lengthen
  live ranges.
- Consider block-local scheduling only with a register-pressure budget.
- Treat broad store sinking and trigger-byte accumulation as unsafe until they
  are pressure-aware.
