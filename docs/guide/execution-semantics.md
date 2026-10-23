# Execution Semantics

The clock-driving operations of Celox are defined by IEEE 1800 scheduling.
Each host call of the event-based `Simulator` is one time slot in which a
fixed SystemVerilog statement executes, and each clock or reset component of
a Veryl native testbench is a fixed piece of SystemVerilog. An IEEE 1800
simulator running that code observes the same values as Celox does.

In the code below, `s` is the signal of an event (a clock or reset port),
`A` its active level and `I` its inactive one. `A` is 1 for a positive-edge
clock and an active-high reset, and 0 for a negative-edge clock and an
active-low reset. For Veryl's abstract `clock` and `reset` types it follows
the build's clock and reset types.

Reset polarity and synchronicity belong to the design, not to these rules.
Veryl emits `always_ff (clk, rst) { if_reset ... }` as
`always_ff @(posedge clk, negedge rst_n) if (!rst_n) ...` for an
asynchronous active-low reset and as `always_ff @(posedge clk) if (!rst_n)
...` for a synchronous one, and the IEEE 1800 scheduler produces each
behavior from the signal moves alone.

## The `Simulator` API

A `Simulator` has no simulation time, but it schedules as an IEEE 1800
simulation does. Each host call is one time slot (IEEE 1800-2023 4.4): the
call's statement executes in that slot, the slot runs until every region is
empty, and the next call starts a later slot. A read (`get`, `sim.dut.x`)
between two calls sees the completed slot.

| Call | Statement of the slot |
|---|---|
| `set(x, v)`, `sim.dut.x = v` | `x = v;` |
| `edge(e, Rising)`, `posedge(e)` | `s <= 1;` |
| `edge(e, Falling)`, `negedge(e)` | `s <= 0;` |
| `active_edge(e)` / `inactive_edge(e)` | `s <= A;` / `s <= I;` |
| `edges([(e1, l1), …, (ek, lk)])` | `s1 <= l1; … sk <= lk;` |
| `tick(e)` | `s <= A;` then, in the next slot, `s <= I;` |
| `tick(e, n)` | `tick(e)` `n` times |
| `tick_events([e1, …, ek])` | `s1 <= A1; … sk <= Ak;` then, in the next slot, `s1 <= I1; … sk <= Ik;` |

The host is not a process of the design, so no delay appears above: a slot
is the unit in which the design responds to one statement. To run the same
sequence on a SystemVerilog simulator, write the statements into one
`initial` process and follow each with a delay of one time precision unit.
A `Simulator` rejects designs with delays, so the length of that unit is not
observable.

Some consequences:

- A tick is one clock period. Every domain sensitive to either edge runs:
  the event's own domain on the active edge, domains of the opposite edge
  (`@(negedge clk)`, a register clocked by `~clk`), and clocks computed from
  the signal, such as `clk & en`, on whichever edge they see. The signal ends
  at its inactive level.
- A signal already at its active level takes no active edge, as
  `s <= A` changes nothing. A tick of a two-state active-low reset that starts
  at 0 only releases it; assert a reset with `negedge(rst)` or
  `active_edge(rst)`.
- Domains triggered by one slot sample the state from before it and update
  together, as nonblocking assignments do. Coincident edges of several clocks
  (`edges`, `tick_events`) sample each other's values from before their shared
  slot. A clock the slot itself moves, such as a register-driven divider,
  rises inside the slot after that commit, and its domain runs in the same
  slot.
- In a four-state design an unknown level counts as either level: `x` to 1 is
  a rising edge and `x` to 0 a falling one (IEEE 1800-2023 Table 9-2).

`set` and writes through `sim.dut` must not move an event signal or a clock
computed from one: Celox runs domains only for the edge calls above. Drive
those signals with `edge`, `tick` and their variants.

### Resets from the host

A reset held for `n` clock periods is the active edge of the reset, `n` ticks
of the clock, and the inactive edge of the reset:

```typescript
sim.activeEdge(rst);
sim.tick(clk, n);
sim.inactiveEdge(rst);
```

An asynchronous reset domain runs in the first slot, before any clock edge; a
synchronous one resets at the first tick. The sequence is the same slots as
the testbench's `rst.assert(n)` below.

### Several clocks

A tick is one period of its event, so it cannot express a phase relation
between two clock inputs. For clocks with a frequency ratio, such as `clk`
and a `clk_half` of twice its period, drive each instant with `edges`:

```typescript
sim.edges([[clk, "posedge"], [clkHalf, "posedge"]]);
sim.edges([[clk, "negedge"]]);
sim.edges([[clk, "posedge"], [clkHalf, "negedge"]]);
sim.edges([[clk, "negedge"]]);
```

or use the timed [`Simulation`](./writing-tests.md#time-based-simulation)
with one `addClock` per input. Alternating `tick_events([clk, clk_half])` with
`tick(clk)` gives the same results only to domains that see the rising edges:
it makes `clk_half` fall with `clk`'s falling edge instead of its next rising
one.

## Veryl native testbenches

The initial blocks of a Veryl testbench are SystemVerilog `initial`
processes, and `$finish()` is `$finish`. Unlike the host, these processes
have simulation time: the components below are SystemVerilog code, and their
delays are the testbench's time.

### `$tb::clock_gen`

`inst clk: $tb::clock_gen #(period: P);` is a clock generator that runs from
time 0, whether or not a process waits on it. `P` defaults to 2 and is at
least 2; with `H = ceil(P / 2)` and `L = floor(P / 2)`:

```systemverilog
bit clk = 1'b0;
initial forever begin
  clk <= 1'b1; #H;
  clk <= 1'b0; #L;
end
```

### `clk.next(n)`

```systemverilog
task automatic clk_next(int unsigned n);
  if (n != 0) begin
    repeat (n) @(posedge clk);
    #P;
  end
endtask
```

`clk.next()` is `clk.next(1)`.

- The process resumes a period after the `n`-th rising edge it waits for,
  just before the next one. It then observes the state after both edges of
  that period: the clock is low, and negative-edge domains have run.
- The clock rises through a nonblocking assignment, after the processes that
  resume at the same time have run. What a process writes between two
  `next` calls is therefore sampled by the rising edge that follows, without
  a race.
- Several initial processes, and several clocks of different periods, need no
  further rule: they are concurrent SystemVerilog processes. A clock nobody
  waits on keeps running and clocking its domains.

### `$tb::reset_gen`

`inst rst: $tb::reset_gen #(cycles: C) (clk);` is a reset net of the
testbench's `reset` type, deasserted from time 0:

```systemverilog
bit rst = I;
```

### `rst.assert(n)`

With `P` the period of the generator's clock:

```systemverilog
task automatic rst_assert(int unsigned n);
  if (n == 0) n = 1;
  rst = A;
  repeat (n) @(posedge clk);
  #P;
  rst = I;
endtask
```

`rst.assert()` is `rst.assert(C)`, and `C` defaults to 3.

- The assertion is a blocking write at the point the process has reached,
  like any other testbench write. An asynchronous reset domain runs at that
  instant, with no clock edge, whether or not its clock is running. A
  synchronous reset domain resets at the next rising edge.
- The level is held for `n` periods of `clk`: the `n` rising edges starting
  with the next one, or with the one at the same instant, since the clock
  rises after the process has written, and the period after the last of them.
  Both kinds of reset see `n` reset edges and are released before edge
  `n + 1`.
- The process resumes, and the reset releases, at the phase at which `next`
  resumes: a period after the last awaited edge, just before the next one.
  Writes that follow are sampled by that edge without a race, and
  `rst.assert(n); clk.next(m);` is `n + m` periods.
- `rst.assert(0)` is `rst.assert(1)`. A reset released in the slot that
  asserted it is a glitch in SystemVerilog and is not given a meaning.

This page defines `$tb::clock_gen`, `clk.next`, `$tb::reset_gen` and
`rst.assert`. The other testbench components, `$tb::file` and `$tb::random`,
take no part in scheduling.
