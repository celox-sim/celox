# Execution Semantics

The clock-driving operations of Celox are defined by SystemVerilog: each one
is equivalent to a fixed piece of SystemVerilog, and running that code on an
IEEE 1800 simulator observes the same values as Celox does. This page gives
that code for the event-based `Simulator` API and for the `$tb::clock_gen`
component of Veryl native testbenches.

In the code below, `s` is the signal of an event (a clock or reset port),
`A` its active level and `I` its inactive one. `A` is 1 for a positive-edge
clock and an active-high reset, and 0 for a negative-edge clock and an
active-low reset. For Veryl's abstract `clock` type it follows the build's
clock type.

## The `Simulator` API

A sequence of host calls is the `initial` process of a wrapper module that
instantiates the design and drives its inputs, with each call replaced by:

| Call | SystemVerilog |
|---|---|
| `set(x, v)`, `sim.dut.x = v` | `x = v; #1;` |
| `edge(e, Rising)`, `posedge(e)` | `s <= 1; #1;` |
| `edge(e, Falling)`, `negedge(e)` | `s <= 0; #1;` |
| `active_edge(e)` / `inactive_edge(e)` | `s <= A; #1;` / `s <= I; #1;` |
| `edges([(e1, l1), …])` | `s1 <= l1; …; #1;` |
| `tick(e)` | `s <= A; #H; s <= I; #L;` |
| `tick(e, n)` | `tick(e)` `n` times |
| `tick_events([e1, …, ek])` | `s1 <= A1; …; sk <= Ak; #H; s1 <= I1; …; sk <= Ik; #L;` |

A read (`get`, `sim.dut.x`) returns the value at the point between two calls,
when the previous call's events have all settled.

A `Simulator` has no simulation time: it rejects designs with delays, so the
lengths of the delays above are not observable. They only order the calls:
every call starts after the previous one has settled.

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
- Domains triggered by one edge sample the state from before it and update
  together, as nonblocking assignments do. Coincident ticks of several clocks
  (`tick_events`) sample each other's values from before their shared edge.
- In a four-state design an unknown level counts as either level: `x` to 1 is
  a rising edge and `x` to 0 a falling one (IEEE 1800-2023 Table 9-2).

`set` and writes through `sim.dut` must not move an event signal or a clock
computed from one: Celox runs domains only for the edge calls above. Drive
those signals with `edge`, `tick` and their variants.

## Veryl native testbenches

The initial blocks of a Veryl testbench are SystemVerilog `initial`
processes, and `$finish()` is `$finish`.

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

This page defines only `$tb::clock_gen` and `clk.next` among the testbench
components.
