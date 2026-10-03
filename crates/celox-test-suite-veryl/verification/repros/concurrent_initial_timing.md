# Concurrent initial timing audit (Veryl 0.22.0)

The Celox reset regression and the upstream timing discrepancies are separate
observations. The upstream runs below use release revision
`967e477a4457ce59eec808b7be2ec0dbe7ff938e`, with AOT-C compilation both synchronous
and asynchronous. Machine-readable observations are in
[concurrent_initial_timing.json](concurrent_initial_timing.json).

## Why multiple initial processes exist

Veryl deliberately introduced this behavior in
[PR #3363](https://github.com/veryl-lang/veryl/pull/3363), merged September 8, 2026,
and [commit 87640d7d](https://github.com/veryl-lang/veryl/commit/87640d7d35ef4719a96a5861e946190183fd5b6b).
Each block runs until it waits or ends. Other blocks can then drive inputs or
check outputs while the first waits. These are cooperative simulation processes,
not parallel host threads.

The released [testbench scheduler](https://github.com/veryl-lang/veryl/blob/967e477a4457ce59eec808b7be2ec0dbe7ff938e/crates/simulator/src/testbench.rs#L1051)
runs ready blocks in declaration order, shares clock edges between waiters,
advances by relative periods, and stops globally on `$finish`. Clocks with no
waiters do not advance. Its comment explicitly says simultaneous rising edges
sample pre-edge values. The release also has
[separate-process and simultaneous-clock tests](https://github.com/veryl-lang/veryl/blob/967e477a4457ce59eec808b7be2ec0dbe7ff938e/crates/simulator/src/tests/testbench.rs#L3273).

The published [Multi-Clock Domains chapter](https://doc.veryl-lang.org/book/05_language_reference/18_execution_model/06_multi_clock.html),
consulted October 3, 2026, instead describes sequential evaluation/commit and
unspecified cross-domain order. That description does not match the released
scheduler's explicit pre-edge contract and tests. It cannot settle the behavior
of these fixtures. The evidence establishes implementation behavior and a
specific stated intent; it does not establish why the documentation differs.

## Inverted-clock timing, independently of coincident-edge order

The shared `mixed_edges` fixture uses source periods 3 and 6, with the second
clock inverted. A third period-2 generator observes the design before the
period-6 clock falls at time 3.

| Observation | Celox (all four backends) | Veryl 0.22.0 (both AOT-C modes) |
| --- | --- | --- |
| Time 2, before the falling edge | `x=1, y=0` | `x=1, y=2` |
| Time 6, original fixture without the earlier assertion | `x=1, y=2` | `x=3, y=2` |

This earlier observation removes simultaneous-edge ordering as an explanation:
`y` has changed before the source edge that is meant to trigger it.

The released [`step_with_derived_clocks` fall pass](https://github.com/veryl-lang/veryl/blob/967e477a4457ce59eec808b7be2ec0dbe7ff938e/crates/simulator/src/simulator.rs#L2441)
sets the master clock low and fires inverted-clock logic inside the same step.
The testbench advances simulated time only after `step_events` returns;
its queued falling edges are waveform updates. This explains the early result:
the fall pass is appropriate when no process can observe the half-cycle, but
another process can now run before that falling edge's physical time. This is
our diagnosis from the source and reproduction, not an upstream confirmation.

The [hand-written SV fixture](concurrent_initial_mixed_edges.sv) passes both
Icarus and Verilator, including the time-2 observation. Its coincident edges
also follow IEEE 1800-2023 4.5 and 4.9.4. SV agreement is corroboration, not proof
that an arbitrary Veryl native test must have identical semantics.

## Asynchronous reset between clock edges

The shared `reset_between_edges` fixture has period-10 and period-2 clocks.
An initial reset establishes `count=0`; the time-10 edge increments it to 1.
Reset is reasserted at time 12 and another process checks at time 14, before the
next slow-clock edge at time 20.

Before the fix, all four Celox backends returned `count=1`. Reset was written
directly, then the edge detector was rebased to that new value. The fix captures
the baseline before process writes and submits reset assertion/deassertion as
current-timestamp events. All four backends now return `count=0`.

Additional Celox API tests cover both polarities, synchronous resets (which must
retain 1 until a clock edge), two-/four-state storage, and a reset timestamp
with no clock edge at all. Component hooks and native image restoration are
also covered. Generated clocks start at known low, avoiding loss of their first
edge when four-state storage initially contains X.

Veryl 0.22.0 also returns `count=1` on the unchanged shared fixture. Its
[`ResetHold`/`take_edges` implementation](https://github.com/veryl-lang/veryl/blob/967e477a4457ce59eec808b7be2ec0dbe7ff938e/crates/simulator/src/testbench.rs#L1000)
writes the asserted level immediately but carries the reset assertion event
with the first waited-on clock edge. This is a second timing discrepancy, not
evidence that the original Celox behavior was correct. The published native
`rst.assert` method is described as synchronized to its clock, so the source
proves the deferral but does not by itself settle the intended API contract
when another process observes an asynchronous-reset register mid-cycle.

The [minimal SV reset fixture](concurrent_initial_reset_between_edges.sv) passes
both tools. Its immediate asynchronous-reset effect is governed by IEEE
1800-2023 9.4.2 (event control). Again, this fixture is hand-written.

## Reproduction and remaining validation limit

For each shared fixture, copy it into a temporary Veryl project and run:

```sh
cargo run -p celox-bench --bin veryl-heliodor -- --project /tmp/project --test Top
cargo run -p celox-bench --bin veryl-heliodor -- --project /tmp/project --test Top --aot-c-async
```

For either SV file (replace `CASE.sv` with its path):

```sh
iverilog -g2012 -s Top -o /tmp/run.vvp CASE.sv
vvp /tmp/run.vvp
verilator --binary --timing --assert -Wno-fatal --top-module Top --Mdir /tmp/obj_dir CASE.sv
/tmp/obj_dir/VTop
```

The full Veryl-to-SV adapters were attempted for all eight shared concurrency
fixtures. Both still fail during native-component emission, before running an
external simulator. Their exact exclusions remain enabled; the two native
runner timing discrepancies are documented separately and are not counted as
upstream parity passes. No upstream issue or comment was submitted by this audit.
